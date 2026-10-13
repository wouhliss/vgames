//! The Vulkan layer in a real Vulkan app (A4-T11, GAME-08): Mesa's software driver (lavapipe)
//! with a headless surface, the layer enabled explicitly from a manifest next to the built
//! library, and a fake broker. Checks that the layer connects with the launch token, that a
//! toast lands in the presented images and that the rest of the frame is untouched, the frame
//! cost against 05-social §6.1 (hidden: a present may cost at most [`HIDDEN_BUDGET`] more than
//! on an instance without the layer; panel open: the layer's draw time per frame, and the
//! whole frame's extra time against that instance, at most [`vulkan::FRAME_BUDGET`]; opening
//! the panel at most [`OPENING_BOUND`] in all), that it follows a swapchain resize, and (feature
//! `fault-injection`) that a lost device presents untouched while a new device gets the
//! overlay again. The cost figures are printed as `overlay frame cost:` lines (`--nocapture`).
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
use vgames_overlay::vulkan;

const LVP: &str = "/usr/share/vulkan/icd.d/lvp_icd.json";
const LAYER: &str = "VK_LAYER_VGAMES_overlay";
const W: u32 = 640;
const H: u32 = 480;
/// 05-social §6.1: what the overlay may add to a frame while nothing is visible.
const HIDDEN_BUDGET: Duration = Duration::from_micros(300);
/// The one-off hitch of opening the panel (compose, then each image's staging written once),
/// in all. Local debug builds on lavapipe take 5-6 ms (7-15 ms under synchronization
/// validation; GL: 12-21 ms); this leaves room for a slow hosted runner while still catching a
/// per-open cost that grew several times over.
const OPENING_BOUND: Duration = Duration::from_millis(100);

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

/// One message toast.
fn toast_view() -> View {
    let mut v = View::default();
    v.toasts.push(Toast {
        id: Uuid::now_v7(),
        kind: ToastKind::Message,
        title: "Sam".into(),
        body: "gg".into(),
        ttl_secs: 60,
    });
    v
}

/// Welcomes one renderer, reports its kind and sends a toast; then sends what the test
/// passes on (heartbeats in between) until the test is done.
fn broker(
    token: [u8; TOKEN_LEN],
) -> (
    String,
    mpsc::Receiver<RendererKind>,
    mpsc::Sender<ToRenderer>,
) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    let (send, next) = mpsc::channel();
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
        protocol::write_frame(&mut s, &ToRenderer::View(toast_view())).unwrap();
        loop {
            let msg = match next.recv_timeout(Duration::from_secs(1)) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => ToRenderer::Heartbeat,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if protocol::write_frame(&mut s, &msg).is_err() {
                break;
            }
        }
    });
    (addr, rx, send)
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
    let (endpoint, hello, send) = broker(token);
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
    unsafe { run(&hello, &send) };
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
    /// How long the last `vkQueuePresentKHR` call took.
    presented: Duration,
    /// How long the last whole frame took (acquire to present, readback included).
    took: Duration,
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

type Messenger = (ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT);

impl Vk {
    /// An instance with our layer (`overlay`) or without it, a headless surface and its
    /// first graphics family. `VGAMES_VK_VALIDATION=1`: the Khronos validation layer below
    /// ours checks every call the overlay makes (errors are printed; run with `--nocapture`).
    unsafe fn open(entry: &ash::Entry, overlay: bool) -> (Self, Option<Messenger>) {
        let validate = std::env::var_os("VGAMES_VK_VALIDATION").is_some();
        let mut layers = Vec::new();
        if overlay {
            layers.push(c"VK_LAYER_VGAMES_overlay".as_ptr());
        }
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
        .expect("an instance");
        let messenger = validate.then(|| {
            let utils = ash::ext::debug_utils::Instance::new(entry, &instance);
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
        let physical = unsafe { instance.enumerate_physical_devices() }.unwrap()[0];
        let family = unsafe { instance.get_physical_device_queue_family_properties(physical) }
            .iter()
            .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .unwrap() as u32;
        let headless = ash::ext::headless_surface::Instance::new(entry, &instance);
        let surface_fn = ash::khr::surface::Instance::new(entry, &instance);
        let surface = unsafe {
            headless.create_headless_surface(&vk::HeadlessSurfaceCreateInfoEXT::default(), None)
        }
        .unwrap();
        let vk = Self {
            entry: entry.clone(),
            instance,
            physical,
            family,
            surface_fn,
            surface,
        };
        (vk, messenger)
    }

    unsafe fn close(self, messenger: Option<Messenger>) {
        unsafe {
            self.surface_fn.destroy_surface(self.surface, None);
            if let Some((utils, m)) = messenger {
                utils.destroy_debug_utils_messenger(m, None);
            }
            self.instance.destroy_instance(None);
        }
        drop(self.entry);
    }

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
            presented: Duration::ZERO,
            took: Duration::ZERO,
        }
    }
}

impl Device {
    /// Presents one black frame. Returns what this image held when it was last presented
    /// (`None` the first time an image comes round).
    unsafe fn frame(&self, c: &mut Chain) -> Option<Vec<u8>> {
        let frame_started = Instant::now();
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
        let started = Instant::now();
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
        c.presented = started.elapsed();
        c.took = frame_started.elapsed();
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

/// The median of `samples`.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// (with, without) medians.
type Medians = (Duration, Duration);

/// `n` frames on each side, alternating so both see the same machine: (with, without)
/// medians of the present call and of the whole frame, and the last readback of `c`.
unsafe fn alternate(
    d: &Device,
    c: &mut Chain,
    plain: &Device,
    plain_c: &mut Chain,
    n: usize,
) -> (Medians, Medians, Option<Vec<u8>>) {
    let (mut present, mut whole) = ((Vec::new(), Vec::new()), (Vec::new(), Vec::new()));
    let mut last = None;
    for _ in 0..n {
        last = unsafe { d.frame(c) }.or(last);
        unsafe { plain.frame(plain_c) };
        present.0.push(c.presented);
        present.1.push(plain_c.presented);
        whole.0.push(c.took);
        whole.1.push(plain_c.took);
    }
    let pair = |(a, b): (Vec<Duration>, Vec<Duration>)| (median(a), median(b));
    (pair(present), pair(whole), last)
}

/// Presents until a frame shows the open panel (a full-height column on the right, where no
/// toast reaches); checks the rest of that frame is untouched.
unsafe fn until_panel(d: &Device, c: &mut Chain) {
    let until = Instant::now() + Duration::from_secs(10);
    let (w, h) = (c.extent.width, c.extent.height);
    loop {
        assert!(
            Instant::now() < until,
            "the panel never reached a presented frame"
        );
        if let Some(frame) = unsafe { d.frame(c) }
            && px(&frame, w, w - 40, h - 20) != BLACK
        {
            assert_eq!(px(&frame, w, 5, h - 5), BLACK);
            return;
        }
    }
}

unsafe fn run(hello: &mpsc::Receiver<RendererKind>, broker: &mpsc::Sender<ToRenderer>) {
    let entry = unsafe { ash::Entry::load() }.expect("libvulkan");
    let (vk, messenger) = unsafe { Vk::open(&entry, true) };
    assert_eq!(
        hello.recv_timeout(Duration::from_secs(5)).unwrap(),
        RendererKind::Vulkan
    );
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
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let validated = std::env::var_os("VGAMES_VK_VALIDATION").is_some();
    let note = if validated { ", under validation" } else { "" };
    eprintln!(
        "overlay frame cost: vulkan toast {:.3} ms per frame over {frames} frames (budget {:.1} \
         ms){note}",
        ms(per_frame),
        ms(vulkan::FRAME_BUDGET),
    );
    assert!(per_frame < vulkan::FRAME_BUDGET, "{per_frame:?} per frame");

    // Hidden: the broker empties the view. Presents through the layer (connected, nothing to
    // draw) and on an instance without it, alternating frame by frame so both see the same
    // machine; each side's median present call.
    broker.send(ToRenderer::View(View::default())).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while unsafe { device.frame(&mut chain) }.is_none_or(|f| toast_shown(&f, chain.extent)) {
        assert!(
            Instant::now() < until,
            "the toast stayed after the view emptied"
        );
    }
    let (plain, plain_messenger) = unsafe { Vk::open(&entry, false) };
    let plain_device = unsafe { plain.device() };
    let mut plain_chain =
        unsafe { plain.chain(&plain_device, chain.extent, vk::SwapchainKHR::null()) };
    // Warm both up.
    unsafe { alternate(&device, &mut chain, &plain_device, &mut plain_chain, 30) };
    let (drawn_before, _) = layer_stats();
    let ((with, without), (with_frame, without_frame), _) =
        unsafe { alternate(&device, &mut chain, &plain_device, &mut plain_chain, 600) };
    let (drawn_after, _) = layer_stats();
    assert_eq!(drawn_before, drawn_after, "drew while nothing was visible");
    eprintln!(
        "overlay frame cost: vulkan hidden {:+.4} ms per present ({:.4} ms with the layer, \
         {:.4} ms without; budget {:.1} ms); whole frame {:.4} vs {:.4} ms{note}",
        ms(with) - ms(without),
        ms(with),
        ms(without),
        ms(HIDDEN_BUDGET),
        ms(with_frame),
        ms(without_frame),
    );
    assert!(
        with.saturating_sub(without) <= HIDDEN_BUDGET,
        "hidden present {with:?} vs {without:?} without the layer"
    );

    // Panel open: the layer's own draw time once the panel shows, and the whole frame (our
    // copy runs on the game's queue: lavapipe's work for it lands in the next frame's waits)
    // against the instance without the layer, alternating as above. Under validation the
    // layer's time is mostly the validation layer's (synchronization validation tracks each
    // row the copy writes: ~0.6-0.85 ms per frame for this full-height panel on lavapipe,
    // 0.04-0.15 ms without it; the whole frame then grows by 0.65-1.15 ms), so a validated run
    // checks a few frames' calls and prints the figures; the budgets are asserted in a run
    // without validation. The opening hitch is bounded in both.
    let open_frames = if validated {
        2 * chain.images.len()
    } else {
        240
    };
    broker.send(ToRenderer::Panel { open: true }).unwrap();
    let hidden = layer_stats();
    unsafe { until_panel(&device, &mut chain) };
    // Opening: until every image got the panel's staging once.
    while layer_stats().0 - hidden.0 < chain.images.len() as u64 {
        unsafe { device.frame(&mut chain) };
    }
    let opened = layer_stats();
    let (_, (open_frame, plain_frame), last) = unsafe {
        alternate(
            &device,
            &mut chain,
            &plain_device,
            &mut plain_chain,
            open_frames,
        )
    };
    let (frames, spent) = layer_stats();
    unsafe { plain_device.drop_chain(plain_chain) };
    unsafe { plain_device.destroy() };
    unsafe { plain.close(plain_messenger) };
    let drawn = frames - opened.0;
    let open = (spent - opened.1) / u32::try_from(drawn).unwrap();
    let opening = opened.1 - hidden.1;
    eprintln!(
        "overlay frame cost: vulkan panel open {:.3} ms per frame in the layer, {:+.3} ms per \
         whole frame ({:.3} vs {:.3} ms without the layer) over {drawn} frames (budget {:.1} \
         ms){note}",
        ms(open),
        ms(open_frame) - ms(plain_frame),
        ms(open_frame),
        ms(plain_frame),
        ms(vulkan::FRAME_BUDGET),
    );
    eprintln!(
        "overlay frame cost: vulkan opening the panel {:.3} ms in all over {} frames (bound {:.0} \
         ms){note}",
        ms(opening),
        opened.0 - hidden.0,
        ms(OPENING_BOUND),
    );
    assert_eq!(drawn, open_frames as u64, "panel frames drawn");
    let (w, h) = (chain.extent.width, chain.extent.height);
    assert_ne!(
        px(&last.unwrap(), w, w - 40, h - 20),
        BLACK,
        "the panel went away"
    );
    assert!(
        opening <= OPENING_BOUND,
        "opening the panel took {opening:?}"
    );
    if !validated {
        assert!(
            open <= vulkan::FRAME_BUDGET,
            "panel open {open:?} per frame"
        );
        assert!(
            open_frame.saturating_sub(plain_frame) <= vulkan::FRAME_BUDGET,
            "panel open frame {open_frame:?} vs {plain_frame:?} without the layer"
        );
    }
    // Closed, and the toast again for what follows.
    broker.send(ToRenderer::View(toast_view())).unwrap();
    broker.send(ToRenderer::Panel { open: false }).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let (w, h) = (W, H);
    while unsafe { device.frame(&mut chain) }.is_none_or(|f| px(&f, w, w - 40, h - 20) != BLACK) {
        assert!(Instant::now() < until, "the panel stayed open");
    }
    unsafe { until_toast(&device, &mut chain) };

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

    unsafe { vk.close(messenger) };
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
