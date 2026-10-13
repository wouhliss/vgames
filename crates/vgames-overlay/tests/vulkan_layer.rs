//! The Vulkan layer in a real Vulkan app (A4-T11, GAME-08): Mesa's software driver (lavapipe)
//! with a headless surface, the layer enabled explicitly from a manifest next to the built
//! library, and a fake broker. Checks that the layer connects with the launch token, that a
//! toast lands in the presented images and that the rest of the frame is untouched, that it
//! follows a swapchain resize, and (feature `fault-injection`) that a lost device presents
//! untouched while a new device gets the overlay again.
//!
//! Skipped (with a message) when lavapipe is not installed, unless `VGAMES_REQUIRE_VULKAN=1`.
#![cfg(all(feature = "renderer", target_os = "linux"))]
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ash::vk;
use uuid::Uuid;
use vgames_overlay::protocol::{
    self, PROTOCOL_VERSION, RendererKind, TOKEN_LEN, ToBroker, ToRenderer, Toast, ToastKind, View,
};

const LVP: &str = "/usr/share/vulkan/icd.d/lvp_icd.json";
const LAYER: &str = "VK_LAYER_VGAMES_overlay";
const W: u32 = 640;
const H: u32 = 480;

fn built_library() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let deps = exe.parent()?;
    [
        deps.join("libvgames_overlay.so"),
        deps.parent()?.join("libvgames_overlay.so"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

fn write_manifest(dir: &Path, library: &Path) {
    let manifest = serde_json_like(library);
    std::fs::write(dir.join("vgames_overlay_test.json"), manifest).unwrap();
}

/// The explicit-layer manifest (no serde_json dependency in this crate).
fn serde_json_like(library: &Path) -> String {
    let lib = library
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        r#"{{"file_format_version":"1.1.2","layer":{{"name":"{LAYER}","type":"GLOBAL",
"library_path":"{lib}","api_version":"1.3.0","implementation_version":"1",
"description":"vgames overlay (test)",
"functions":{{"vkNegotiateLoaderLayerInterfaceVersion":"vgames_overlay_vkNegotiateLoaderLayerInterfaceVersion"}}}}}}"#
    )
}

/// Welcomes one renderer, reports its kind, sends a toast and keeps the link alive.
fn broker(token: [u8; TOKEN_LEN]) -> (String, mpsc::Receiver<RendererKind>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let ToBroker::Hello {
            token: got,
            renderer,
            ..
        } = protocol::read_frame::<ToBroker>(&mut s).unwrap()
        else {
            panic!("expected Hello");
        };
        assert_eq!(got, token);
        tx.send(renderer).unwrap();
        protocol::write_frame(
            &mut s,
            &ToRenderer::Welcome {
                version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        let mut v = View::default();
        v.toasts.push(Toast {
            id: Uuid::now_v7(),
            kind: ToastKind::Message,
            title: "Sam".into(),
            body: "gg".into(),
            ttl_secs: 60,
        });
        protocol::write_frame(&mut s, &ToRenderer::View(v)).unwrap();
        while protocol::read_frame::<ToBroker>(&mut s).is_ok() {}
    });
    (addr, rx)
}

#[test]
fn the_layer_draws_a_toast_into_presented_frames() {
    let required = std::env::var_os("VGAMES_REQUIRE_VULKAN").is_some();
    if !Path::new(LVP).is_file() {
        assert!(!required, "lavapipe is not installed");
        eprintln!("skipped: lavapipe ({LVP}) is not installed");
        return;
    }
    let library = built_library().expect("the layer library next to the test binary");
    let dir = tempfile_dir();
    write_manifest(&dir, &library);
    let token = [5u8; TOKEN_LEN];
    let (endpoint, hello) = broker(token);
    // SAFETY: set before any Vulkan or link thread starts; this is the only test here.
    unsafe {
        let mut path = std::ffi::OsString::from(&dir);
        if std::env::var_os("VGAMES_VK_VALIDATION").is_some() {
            path.push(":/usr/share/vulkan/explicit_layer.d");
        }
        std::env::set_var("VK_LAYER_PATH", path);
        std::env::set_var("VK_ICD_FILENAMES", LVP);
        std::env::set_var("VK_DRIVER_FILES", LVP);
        std::env::set_var(protocol::env::ENABLED, "1");
        std::env::set_var(protocol::env::ENDPOINT, &endpoint);
        std::env::set_var(protocol::env::TOKEN, protocol::token_hex(&token));
    }

    // SAFETY: plain Vulkan usage below; every handle is created and destroyed in order.
    unsafe { run(&hello) };
    std::fs::remove_dir_all(&dir).ok();
}

/// The counters of the layer the loader loaded (already in the process: same instance).
fn layer_stats() -> (u64, Duration) {
    let lib = unsafe { libloading::Library::new(built_library().unwrap()) }.unwrap();
    let f: libloading::Symbol<'_, unsafe extern "C" fn(*mut u64, *mut u64)> =
        unsafe { lib.get(b"vgames_overlay_draw_stats\0") }.unwrap();
    let (mut frames, mut nanos) = (0u64, 0u64);
    unsafe { f(&mut frames, &mut nanos) };
    (frames, Duration::from_nanos(nanos))
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vgames-vk-layer-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The game's side: one device, its queue, and the objects every frame uses.
struct Device {
    device: ash::Device,
    queue: vk::Queue,
    swapchain_fn: ash::khr::swapchain::Device,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *const u8,
}

/// One swapchain and what the game keeps per image.
struct Chain {
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    rendered: Vec<vk::Semaphore>,
    seen: Vec<bool>,
    extent: vk::Extent2D,
}

/// Readback big enough for the largest swapchain used here.
const MAX_W: u32 = 800;
const MAX_H: u32 = 600;

struct Vk {
    entry: ash::Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    family: u32,
    surface_fn: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
}

impl Vk {
    unsafe fn device(&self) -> Device {
        let prio = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(self.family)
            .queue_priorities(&prio)];
        let dexts = [ash::khr::swapchain::NAME.as_ptr()];
        let device = unsafe {
            self.instance.create_device(
                self.physical,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&dexts),
                None,
            )
        }
        .unwrap();
        let queue = unsafe { device.get_device_queue(self.family, 0) };
        let swapchain_fn = ash::khr::swapchain::Device::new(&self.instance, &device);
        let size = u64::from(MAX_W * MAX_H * 4);
        let buffer = unsafe {
            device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_DST),
                None,
            )
        }
        .unwrap();
        let req = unsafe { device.get_buffer_memory_requirements(buffer) };
        let mem_props = unsafe {
            self.instance
                .get_physical_device_memory_properties(self.physical)
        };
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let type_index = mem_props
            .memory_types_as_slice()
            .iter()
            .enumerate()
            .position(|(i, t)| {
                req.memory_type_bits & (1 << i) != 0 && t.property_flags.contains(host)
            })
            .unwrap() as u32;
        let memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(type_index),
                None,
            )
        }
        .unwrap();
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.unwrap();
        let mapped = unsafe { device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()) }
            .unwrap()
            .cast::<u8>()
            .cast_const();
        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(self.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .unwrap();
        let cmd = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .command_buffer_count(1),
            )
        }
        .unwrap()[0];
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.unwrap();
        Device {
            device,
            queue,
            swapchain_fn,
            pool,
            cmd,
            fence,
            buffer,
            memory,
            mapped,
        }
    }

    unsafe fn chain(&self, d: &Device, extent: vk::Extent2D, old: vk::SwapchainKHR) -> Chain {
        let caps = unsafe {
            self.surface_fn
                .get_physical_device_surface_capabilities(self.physical, self.surface)
        }
        .unwrap();
        let modes = unsafe {
            self.surface_fn
                .get_physical_device_surface_present_modes(self.physical, self.surface)
        }
        .unwrap();
        let swapchain = unsafe {
            d.swapchain_fn.create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(self.surface)
                    .min_image_count(caps.min_image_count.max(2))
                    .image_format(vk::Format::B8G8R8A8_UNORM)
                    .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
                    .image_extent(extent)
                    .image_array_layers(1)
                    .image_usage(
                        vk::ImageUsageFlags::COLOR_ATTACHMENT
                            | vk::ImageUsageFlags::TRANSFER_SRC
                            | vk::ImageUsageFlags::TRANSFER_DST,
                    )
                    .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    .present_mode(modes[0])
                    .clipped(true)
                    .old_swapchain(old),
                None,
            )
        }
        .unwrap();
        let images = unsafe { d.swapchain_fn.get_swapchain_images(swapchain) }.unwrap();
        let rendered = images
            .iter()
            .map(|_| {
                unsafe {
                    d.device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                }
                .unwrap()
            })
            .collect();
        Chain {
            swapchain,
            seen: vec![false; images.len()],
            images,
            rendered,
            extent,
        }
    }
}

impl Device {
    /// Presents one black frame. Returns what this image held when it was last presented
    /// (`None` the first time an image comes round).
    unsafe fn frame(&self, c: &mut Chain) -> Option<Vec<u8>> {
        let d = &self.device;
        let (index, _) = unsafe {
            self.swapchain_fn.acquire_next_image(
                c.swapchain,
                u64::MAX,
                vk::Semaphore::null(),
                self.fence,
            )
        }
        .unwrap();
        unsafe {
            d.wait_for_fences(&[self.fence], true, u64::MAX).unwrap();
            d.reset_fences(&[self.fence]).unwrap();
        }
        let i = index as usize;
        let image = c.images[i];
        let readback = c.seen[i];
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let barrier = |old, new| {
            vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .old_layout(old)
                .new_layout(new)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range)
        };
        let sync = |b: vk::ImageMemoryBarrier<'_>| unsafe {
            d.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[b],
            );
        };
        unsafe {
            d.begin_command_buffer(self.cmd, &vk::CommandBufferBeginInfo::default())
                .unwrap();
            if readback {
                // What the layer left in this image when it was last presented.
                sync(barrier(
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                ));
                d.cmd_copy_image_to_buffer(
                    self.cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    self.buffer,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_extent(vk::Extent3D {
                            width: c.extent.width,
                            height: c.extent.height,
                            depth: 1,
                        })],
                );
                sync(barrier(
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                ));
            } else {
                sync(barrier(
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                ));
            }
            // The game's frame: black.
            d.cmd_clear_color_image(
                self.cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
                &[range],
            );
            sync(barrier(
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
            ));
            d.end_command_buffer(self.cmd).unwrap();
            let cmds = [self.cmd];
            let signal = [c.rendered[i]];
            d.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default()
                    .command_buffers(&cmds)
                    .signal_semaphores(&signal)],
                self.fence,
            )
            .unwrap();
            d.wait_for_fences(&[self.fence], true, u64::MAX).unwrap();
            d.reset_fences(&[self.fence]).unwrap();
        }
        let pixels = readback.then(|| {
            let len = (c.extent.width * c.extent.height * 4) as usize;
            unsafe { std::slice::from_raw_parts(self.mapped, len) }.to_vec()
        });
        c.seen[i] = true;
        let waits = [c.rendered[i]];
        let chains = [c.swapchain];
        let indices = [index];
        unsafe {
            self.swapchain_fn
                .queue_present(
                    self.queue,
                    &vk::PresentInfoKHR::default()
                        .wait_semaphores(&waits)
                        .swapchains(&chains)
                        .image_indices(&indices),
                )
                .unwrap();
        }
        pixels
    }

    unsafe fn drop_chain(&self, c: Chain) {
        unsafe {
            self.device.device_wait_idle().unwrap();
            for s in c.rendered {
                self.device.destroy_semaphore(s, None);
            }
            self.swapchain_fn.destroy_swapchain(c.swapchain, None);
        }
    }

    unsafe fn destroy(self) {
        unsafe {
            self.device.device_wait_idle().unwrap();
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.unmap_memory(self.memory);
            self.device.destroy_buffer(self.buffer, None);
            self.device.free_memory(self.memory, None);
            self.device.destroy_device(None);
        }
    }
}

const BLACK: [u8; 4] = [0, 0, 0, 255];

fn px(frame: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * width + x) * 4) as usize;
    [frame[o], frame[o + 1], frame[o + 2], frame[o + 3]]
}

/// The toast card sits at the top right (12 px margin, 320 px wide at these sizes); its first
/// columns are the accent colour.
fn toast_shown(frame: &[u8], extent: vk::Extent2D) -> bool {
    px(frame, extent.width, extent.width - 12 - 320 + 1, 14) != BLACK
}

/// Presents until a frame shows the toast; checks the rest of that frame is untouched.
unsafe fn until_toast(d: &Device, c: &mut Chain) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < until,
            "no toast reached a presented {}×{} frame",
            c.extent.width,
            c.extent.height
        );
        if let Some(frame) = unsafe { d.frame(c) }
            && toast_shown(&frame, c.extent)
        {
            let (w, h) = (c.extent.width, c.extent.height);
            assert_eq!(px(&frame, w, 5, h - 5), BLACK);
            assert_eq!(px(&frame, w, w / 2 - 40, h / 2), BLACK);
            return;
        }
    }
}

/// Arms the layer's simulated device loss (feature `fault-injection`).
fn fault_device_lost() {
    let lib = unsafe { libloading::Library::new(built_library().unwrap()) }.unwrap();
    let f: libloading::Symbol<'_, unsafe extern "C" fn()> =
        unsafe { lib.get(b"vgames_overlay_fault_device_lost\0") }.unwrap();
    unsafe { f() };
}

unsafe fn run(hello: &mpsc::Receiver<RendererKind>) {
    let entry = unsafe { ash::Entry::load() }.expect("libvulkan");
    // `VGAMES_VK_VALIDATION=1`: the Khronos validation layer below ours checks every call the
    // overlay makes (errors are printed; run with `--nocapture`).
    let validate = std::env::var_os("VGAMES_VK_VALIDATION").is_some();
    let mut layers = vec![c"VK_LAYER_VGAMES_overlay".as_ptr()];
    if validate {
        layers.push(c"VK_LAYER_KHRONOS_validation".as_ptr());
    }
    let mut exts = vec![
        ash::khr::surface::NAME.as_ptr(),
        ash::ext::headless_surface::NAME.as_ptr(),
    ];
    if validate {
        exts.push(ash::ext::debug_utils::NAME.as_ptr());
    }
    let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
    let instance = unsafe {
        entry.create_instance(
            &vk::InstanceCreateInfo::default()
                .application_info(&app)
                .enabled_layer_names(&layers)
                .enabled_extension_names(&exts),
            None,
        )
    }
    .expect("instance with the layer");
    let messenger = validate.then(|| {
        let utils = ash::ext::debug_utils::Instance::new(&entry, &instance);
        let m = unsafe {
            utils.create_debug_utils_messenger(
                &vk::DebugUtilsMessengerCreateInfoEXT::default()
                    .message_severity(
                        vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                            | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                    )
                    .message_type(
                        vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                            | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                    )
                    .pfn_user_callback(Some(on_validation)),
                None,
            )
        }
        .unwrap();
        (utils, m)
    });
    assert_eq!(
        hello.recv_timeout(Duration::from_secs(5)).unwrap(),
        RendererKind::Vulkan
    );

    let physical = unsafe { instance.enumerate_physical_devices() }.unwrap()[0];
    let family = unsafe { instance.get_physical_device_queue_family_properties(physical) }
        .iter()
        .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .unwrap() as u32;
    let headless = ash::ext::headless_surface::Instance::new(&entry, &instance);
    let surface_fn = ash::khr::surface::Instance::new(&entry, &instance);
    let surface = unsafe {
        headless.create_headless_surface(&vk::HeadlessSurfaceCreateInfoEXT::default(), None)
    }
    .unwrap();
    let vk = Vk {
        entry,
        instance,
        physical,
        family,
        surface_fn,
        surface,
    };
    let device = unsafe { vk.device() };
    let mut chain = unsafe {
        vk.chain(
            &device,
            vk::Extent2D {
                width: W,
                height: H,
            },
            vk::SwapchainKHR::null(),
        )
    };

    // The toast lands, then the steady cost over 240 more frames.
    unsafe { until_toast(&device, &mut chain) };
    for _ in 0..240 {
        unsafe { device.frame(&mut chain) };
    }
    // Before teardown: the loader may unload the layer with the instance.
    let (frames, spent) = layer_stats();
    assert!(frames >= 240, "{frames} frames drawn");
    let per_frame = spent / u32::try_from(frames).unwrap();
    eprintln!("overlay draw cost: {per_frame:?} per frame over {frames} frames");
    assert!(
        per_frame < vgames_overlay::vulkan::FRAME_BUDGET,
        "{per_frame:?} per frame"
    );

    // Resize: the game recreates its swapchain bigger, handing over the old one.
    let bigger = vk::Extent2D {
        width: MAX_W,
        height: MAX_H,
    };
    let old = chain.swapchain;
    let mut resized = unsafe { vk.chain(&device, bigger, old) };
    unsafe { device.drop_chain(chain) };
    unsafe { until_toast(&device, &mut resized) };

    // Device loss (simulated inside the layer): this device presents untouched from then on;
    // the device the game creates next gets the overlay again.
    if cfg!(feature = "fault-injection") {
        fault_device_lost();
        // Readbacks show each image's previous present: skip one round of images first.
        let round = resized.images.len() + 1;
        let mut untouched = 0;
        for n in 0..30 {
            let frame = unsafe { device.frame(&mut resized) };
            if let Some(frame) = frame.filter(|_| n >= round) {
                assert!(
                    !toast_shown(&frame, bigger),
                    "drew after the device was lost"
                );
                untouched += 1;
            }
        }
        assert!(untouched > 0);
        unsafe { device.drop_chain(resized) };
        unsafe { device.destroy() };
        let device = unsafe { vk.device() };
        let mut chain = unsafe { vk.chain(&device, bigger, vk::SwapchainKHR::null()) };
        unsafe { until_toast(&device, &mut chain) };
        unsafe { device.drop_chain(chain) };
        unsafe { device.destroy() };
    } else {
        eprintln!("device loss not simulated: build with --features fault-injection");
        unsafe { device.drop_chain(resized) };
        unsafe { device.destroy() };
    }

    unsafe {
        vk.surface_fn.destroy_surface(vk.surface, None);
        if let Some((utils, m)) = messenger {
            utils.destroy_debug_utils_messenger(m, None);
        }
        vk.instance.destroy_instance(None);
    }
    drop(vk.entry);
    assert_eq!(
        VALIDATION_ERRORS.load(Ordering::Relaxed),
        0,
        "validation errors (see output)"
    );
}

static VALIDATION_ERRORS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn on_validation(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _: *mut std::ffi::c_void,
) -> vk::Bool32 {
    let msg = unsafe { data.as_ref().and_then(|d| d.message_as_c_str()) }
        .map(|m| m.to_string_lossy().into_owned())
        .unwrap_or_default();
    eprintln!("[{severity:?}] {msg}");
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        VALIDATION_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    vk::FALSE
}
