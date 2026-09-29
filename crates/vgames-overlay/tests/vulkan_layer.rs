//! The Vulkan layer in a real Vulkan app (A4-T11): Mesa's software driver (lavapipe) with a
//! headless surface, the layer enabled explicitly from a manifest next to the built
//! library, and a fake broker. Checks that the layer connects with the launch token, that a
//! toast lands in the presented images and that the rest of the frame is untouched.
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
    let prio = [1.0];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(family)
        .queue_priorities(&prio)];
    let dexts = [ash::khr::swapchain::NAME.as_ptr()];
    let device = unsafe {
        instance.create_device(
            physical,
            &vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&dexts),
            None,
        )
    }
    .unwrap();
    let queue = unsafe { device.get_device_queue(family, 0) };

    let headless = ash::ext::headless_surface::Instance::new(&entry, &instance);
    let surface_fn = ash::khr::surface::Instance::new(&entry, &instance);
    let surface = unsafe {
        headless.create_headless_surface(&vk::HeadlessSurfaceCreateInfoEXT::default(), None)
    }
    .unwrap();
    let caps =
        unsafe { surface_fn.get_physical_device_surface_capabilities(physical, surface) }.unwrap();
    let modes =
        unsafe { surface_fn.get_physical_device_surface_present_modes(physical, surface) }.unwrap();
    let swapchain_fn = ash::khr::swapchain::Device::new(&instance, &device);
    let swapchain = unsafe {
        swapchain_fn.create_swapchain(
            &vk::SwapchainCreateInfoKHR::default()
                .surface(surface)
                .min_image_count(caps.min_image_count.max(2))
                .image_format(vk::Format::B8G8R8A8_UNORM)
                .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
                .image_extent(vk::Extent2D {
                    width: W,
                    height: H,
                })
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
                .clipped(true),
            None,
        )
    }
    .unwrap();
    let images = unsafe { swapchain_fn.get_swapchain_images(swapchain) }.unwrap();

    // Readback buffer for one frame.
    let size = u64::from(W * H * 4);
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
    let mem_props = unsafe { instance.get_physical_device_memory_properties(physical) };
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let type_index = mem_props
        .memory_types_as_slice()
        .iter()
        .enumerate()
        .position(|(i, t)| req.memory_type_bits & (1 << i) != 0 && t.property_flags.contains(host))
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
        .cast::<u8>();

    let pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(family)
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
    let rendered: Vec<vk::Semaphore> = images
        .iter()
        .map(|_| {
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.unwrap()
        })
        .collect();

    let range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    let barrier = |image, old, new| {
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
    let sync = |cmd, b: vk::ImageMemoryBarrier<'_>| unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[b],
        );
    };

    let mut seen = vec![false; images.len()];
    let until = Instant::now() + Duration::from_secs(10);
    let mut drawn = false;
    // After the first toast shows: keep presenting to measure the steady cost.
    let mut extra = 0u32;
    while !drawn || extra < 240 {
        if drawn {
            extra += 1;
        }
        assert!(Instant::now() < until, "no toast reached a presented frame");
        let (index, _) = unsafe {
            swapchain_fn.acquire_next_image(swapchain, u64::MAX, vk::Semaphore::null(), fence)
        }
        .unwrap();
        unsafe {
            device.wait_for_fences(&[fence], true, u64::MAX).unwrap();
            device.reset_fences(&[fence]).unwrap();
        }
        let i = index as usize;
        let image = images[i];
        let readback = seen[i];
        unsafe {
            device
                .begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default())
                .unwrap();
            if readback {
                // What the layer left in this image when it was last presented.
                sync(
                    cmd,
                    barrier(
                        image,
                        vk::ImageLayout::PRESENT_SRC_KHR,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    ),
                );
                device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_extent(vk::Extent3D {
                            width: W,
                            height: H,
                            depth: 1,
                        })],
                );
                sync(
                    cmd,
                    barrier(
                        image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    ),
                );
            } else {
                sync(
                    cmd,
                    barrier(
                        image,
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    ),
                );
            }
            // The game's frame: black.
            device.cmd_clear_color_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
                &[range],
            );
            sync(
                cmd,
                barrier(
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                ),
            );
            device.end_command_buffer(cmd).unwrap();
            let cmds = [cmd];
            let signal = [rendered[i]];
            device
                .queue_submit(
                    queue,
                    &[vk::SubmitInfo::default()
                        .command_buffers(&cmds)
                        .signal_semaphores(&signal)],
                    fence,
                )
                .unwrap();
            device.wait_for_fences(&[fence], true, u64::MAX).unwrap();
            device.reset_fences(&[fence]).unwrap();
        }
        if readback {
            let px = |x: u32, y: u32| -> [u8; 4] {
                let o = ((y * W + x) * 4) as usize;
                let s = unsafe { std::slice::from_raw_parts(mapped.add(o), 4) };
                [s[0], s[1], s[2], s[3]]
            };
            // The toast card sits at the top right (12 px margin, 320 px wide at this size);
            // its first columns are the accent colour.
            let card = px(W - 12 - 320 + 1, 14);
            if !drawn && card != [0, 0, 0, 255] {
                drawn = true;
                // Far from the card the game's frame is untouched.
                assert_eq!(px(5, H - 5), [0, 0, 0, 255]);
                assert_eq!(px(W / 2 - 40, H / 2), [0, 0, 0, 255]);
            }
        }
        seen[i] = true;
        let waits = [rendered[i]];
        let chains = [swapchain];
        let indices = [index];
        unsafe {
            swapchain_fn
                .queue_present(
                    queue,
                    &vk::PresentInfoKHR::default()
                        .wait_semaphores(&waits)
                        .swapchains(&chains)
                        .image_indices(&indices),
                )
                .unwrap();
        }
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
    unsafe {
        device.device_wait_idle().unwrap();
        for s in rendered {
            device.destroy_semaphore(s, None);
        }
        device.destroy_fence(fence, None);
        device.destroy_command_pool(pool, None);
        device.unmap_memory(memory);
        device.destroy_buffer(buffer, None);
        device.free_memory(memory, None);
        swapchain_fn.destroy_swapchain(swapchain, None);
        surface_fn.destroy_surface(surface, None);
        device.destroy_device(None);
        if let Some((utils, m)) = messenger {
            utils.destroy_debug_utils_messenger(m, None);
        }
        instance.destroy_instance(None);
    }
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
