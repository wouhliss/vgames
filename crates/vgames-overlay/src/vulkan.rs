//! Vulkan implicit layer (05-social §6.1, A4-T11): draws the overlay into the game's
//! swapchain images just before they are presented. Covers native Vulkan games and every
//! Proton game (DXVK / VKD3D-Proton present through Vulkan).
//!
//! How it draws: on `vkQueuePresentKHR`, when something is visible, the cards from
//! [`crate::draw`] are copied from a host-visible staging buffer into the image being
//! presented (`vkCmdCopyBufferToImage` between two layout barriers), on the presenting queue,
//! waiting on the game's semaphores and signalling our own, which the real present then waits
//! on. No shader, pipeline, descriptor or render pass of the game is touched. The layer adds
//! `TRANSFER_DST` to the swapchain usage when the surface supports it; swapchains that cannot
//! take it, or whose format is not 8-bit BGRA/RGBA, are left alone.
//!
//! Cost: while nothing is visible a present costs one map lookup and one atomic load. While
//! visible, staging buffers are rewritten only when the cards change, and the layer turns
//! itself off if drawing keeps exceeding [`FRAME_BUDGET`]. Every entry point catches panics:
//! a bug here turns the overlay off, never crashes the game.
//!
//! Everything here is FFI with the Vulkan loader, hence the module-wide `unsafe_code` allowance
//! (granted in `lib.rs`); each `unsafe` block states what it relies on.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use ash::vk::{self, Handle as _};

use crate::draw::{self, Region};
use crate::guard;
use crate::link::{Endpoint, Link};
use crate::protocol::{RendererKind, View};

/// Drawing may take this long per frame on average before the layer turns itself off.
pub const FRAME_BUDGET: Duration = Duration::from_millis(1);
/// Frames averaged for the budget.
const BUDGET_WINDOW: u32 = 120;
/// Set once the layer has been part of an instance of this process: the GL hooks stand down
/// (a game draws through one API; Zink and friends present through Vulkan).
pub(crate) static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DRAWN_FRAMES: AtomicU64 = AtomicU64::new(0);
static DRAW_NANOS: AtomicU64 = AtomicU64::new(0);

/// Frames the layer drew into so far, and the CPU time it spent on them (for benchmarks).
pub fn draw_stats() -> (u64, Duration) {
    (
        DRAWN_FRAMES.load(Ordering::Relaxed),
        Duration::from_nanos(DRAW_NANOS.load(Ordering::Relaxed)),
    )
}

/// [`draw_stats`] for a harness that loaded the layer library (its statics are not the
/// ones of a statically linked copy of this crate).
///
/// # Safety
/// Both pointers must be valid for a `u64` write (null pointers are skipped).
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vgames_overlay_draw_stats(frames: *mut u64, nanos: *mut u64) {
    let (f, d) = draw_stats();
    // SAFETY: the caller passes writable pointers or null.
    unsafe {
        if let Some(p) = frames.as_mut() {
            *p = f;
        }
        if let Some(p) = nanos.as_mut() {
            *p = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        }
    }
}

/// How long a present waits for our previous copy into the same image before skipping.
const SLOT_WAIT_NS: u64 = 20_000_000;

// ---- loader interface (vk_layer.h) ---------------------------------------------------------

const LAYER_NEGOTIATE_INTERFACE_STRUCT: u32 = 1;
const VK_LAYER_LINK_INFO: u32 = 0;
const VK_LOADER_DATA_CALLBACK: u32 = 1;

type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

#[repr(C)]
pub struct NegotiateLayerInterface {
    s_type: u32,
    p_next: *mut c_void,
    loader_layer_interface_version: u32,
    pfn_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pfn_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
    pfn_get_physical_device_proc_addr: *const c_void,
}

#[repr(C)]
struct LayerInstanceLink {
    p_next: *mut LayerInstanceLink,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_physical_device_proc_addr: *const c_void,
}

#[repr(C)]
struct LayerDeviceLink {
    p_next: *mut LayerDeviceLink,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

/// `VkLayerInstanceCreateInfo` / `VkLayerDeviceCreateInfo`: the union is read through `u`.
#[repr(C)]
struct LayerCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32,
    u: *mut c_void,
}

/// Finds the loader's create info of `s_type` / `function` in a `pNext` chain.
///
/// # Safety
/// `p_next` must be a valid Vulkan `pNext` chain.
unsafe fn find_layer_info(
    mut p_next: *const c_void,
    s_type: vk::StructureType,
    function: u32,
) -> *mut LayerCreateInfo {
    while !p_next.is_null() {
        // SAFETY: every chain element starts with sType/pNext (VkBaseInStructure).
        let base = unsafe { &*p_next.cast::<vk::BaseInStructure>() };
        if base.s_type == s_type {
            let info = p_next.cast::<LayerCreateInfo>().cast_mut();
            // SAFETY: the loader's structures of these types have the layout above.
            if unsafe { (*info).function } == function {
                return info;
            }
        }
        p_next = base.p_next.cast();
    }
    std::ptr::null_mut()
}

/// The loader's dispatch pointer: the same for an instance and its physical devices, and
/// for a device, its queues and command buffers.
///
/// # Safety
/// `handle` must be a live dispatchable Vulkan handle.
unsafe fn key<T: vk::Handle>(handle: T) -> usize {
    let raw = handle.as_raw() as usize as *const usize;
    if raw.is_null() {
        return 0;
    }
    // SAFETY: dispatchable handles point at the loader's dispatch table pointer.
    unsafe { *raw }
}

// ---- state ---------------------------------------------------------------------------------

struct InstanceData {
    next_gipa: vk::PFN_vkGetInstanceProcAddr,
    instance: ash::Instance,
    surface: ash::khr::surface::InstanceFn,
}

struct DeviceData {
    handle: vk::Device,
    instance_key: usize,
    physical: vk::PhysicalDevice,
    next_gdpa: vk::PFN_vkGetDeviceProcAddr,
    device: ash::Device,
    swapchain: ash::khr::swapchain::DeviceFn,
    set_loader_data: Option<SetDeviceLoaderData>,
    families: Vec<vk::QueueFamilyProperties>,
    /// The queues the game created, with their family.
    queues: Vec<(vk::Queue, u32)>,
    memory: vk::PhysicalDeviceMemoryProperties,
    state: Mutex<DeviceState>,
}

#[derive(Default)]
struct DeviceState {
    swapchains: HashMap<vk::SwapchainKHR, Swapchain>,
    pools: HashMap<u32, vk::CommandPool>,
    /// Drawing failed on this device (out of memory, device lost…): pass-through only.
    broken: bool,
    budget: Budget,
}

struct Swapchain {
    images: Vec<vk::Image>,
    extent: vk::Extent2D,
    /// `true` for BGRA order, `false` for RGBA.
    bgra: bool,
    usable: bool,
    slots: HashMap<u32, Slot>,
    cards: Cards,
}

#[derive(Default)]
struct Cards {
    key: Option<CardsKey>,
    generation: u64,
    regions: Vec<Region>,
}

#[derive(PartialEq)]
struct CardsKey {
    view: usize,
    alive: Vec<bool>,
    panel: bool,
}

/// Per swapchain image: our command buffer, sync objects and staging buffer.
struct Slot {
    family: u32,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    done: vk::Semaphore,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    size: u64,
    generation: u64,
}

// SAFETY: `mapped` is only touched with the device state lock held.
#[allow(unsafe_code)]
unsafe impl Send for Slot {}

#[derive(Default)]
struct Budget {
    frames: u32,
    spent: Duration,
}

impl Budget {
    /// Adds one frame; `false` once the average over the window is above budget.
    fn record(&mut self, took: Duration) -> bool {
        self.frames += 1;
        self.spent += took;
        if self.frames < BUDGET_WINDOW {
            return true;
        }
        let ok = self.spent / self.frames <= FRAME_BUDGET;
        *self = Self::default();
        ok
    }
}

type Map<T> = RwLock<HashMap<usize, Arc<T>>>;

fn instances() -> &'static Map<InstanceData> {
    static M: OnceLock<Map<InstanceData>> = OnceLock::new();
    M.get_or_init(Default::default)
}

fn devices() -> &'static Map<DeviceData> {
    static M: OnceLock<Map<DeviceData>> = OnceLock::new();
    M.get_or_init(Default::default)
}

fn get<T>(map: &Map<T>, key: usize) -> Option<Arc<T>> {
    map.read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .cloned()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The broker link, started with the first instance (none without the launch environment).
fn link() -> Option<&'static Arc<Link>> {
    static LINK: OnceLock<Option<Arc<Link>>> = OnceLock::new();
    LINK.get_or_init(|| Endpoint::from_env().map(|e| Link::start(e, RendererKind::Vulkan)))
        .as_ref()
}

/// Runs an entry point body; a panic turns the overlay off and returns `fallback`.
fn entry<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => {
            guard::disable();
            fallback
        }
    }
}

// ---- negotiation and proc addresses --------------------------------------------------------

/// The loader's entry point into the layer (loader–layer interface version 2).
///
/// # Safety
/// Called by the Vulkan loader with a valid `VkNegotiateLayerInterface`.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vgames_overlay_vkNegotiateLoaderLayerInterfaceVersion(
    p: *mut NegotiateLayerInterface,
) -> vk::Result {
    if p.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    // SAFETY: the loader passes a valid, writable struct.
    let n = unsafe { &mut *p };
    if n.s_type != LAYER_NEGOTIATE_INTERFACE_STRUCT || n.loader_layer_interface_version < 2 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    n.loader_layer_interface_version = 2;
    n.pfn_get_instance_proc_addr = Some(get_instance_proc_addr as vk::PFN_vkGetInstanceProcAddr);
    n.pfn_get_device_proc_addr = Some(get_device_proc_addr as vk::PFN_vkGetDeviceProcAddr);
    n.pfn_get_physical_device_proc_addr = std::ptr::null();
    vk::Result::SUCCESS
}

/// Our own entry points by name.
fn intercept(name: &CStr) -> vk::PFN_vkVoidFunction {
    // SAFETY (all transmutes): each function has the signature Vulkan specifies for `name`,
    // and the loader casts it back to exactly that type.
    #[allow(unsafe_code)]
    unsafe {
        use std::mem::transmute as t;
        type Void = unsafe extern "system" fn();
        Some(match name.to_bytes() {
            b"vkGetInstanceProcAddr" => {
                t::<vk::PFN_vkGetInstanceProcAddr, Void>(get_instance_proc_addr)
            }
            b"vkGetDeviceProcAddr" => t::<vk::PFN_vkGetDeviceProcAddr, Void>(get_device_proc_addr),
            b"vkCreateInstance" => t::<vk::PFN_vkCreateInstance, Void>(create_instance),
            b"vkDestroyInstance" => t::<vk::PFN_vkDestroyInstance, Void>(destroy_instance),
            b"vkCreateDevice" => t::<vk::PFN_vkCreateDevice, Void>(create_device),
            b"vkDestroyDevice" => t::<vk::PFN_vkDestroyDevice, Void>(destroy_device),
            b"vkCreateSwapchainKHR" => t::<vk::PFN_vkCreateSwapchainKHR, Void>(create_swapchain),
            b"vkDestroySwapchainKHR" => t::<vk::PFN_vkDestroySwapchainKHR, Void>(destroy_swapchain),
            b"vkQueuePresentKHR" => t::<vk::PFN_vkQueuePresentKHR, Void>(queue_present),
            _ => return None,
        })
    }
}

#[allow(unsafe_code)]
unsafe extern "system" fn get_instance_proc_addr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    entry(None, || {
        if name.is_null() {
            return None;
        }
        // SAFETY: the caller passes a NUL-terminated name.
        let n = unsafe { CStr::from_ptr(name) };
        if let Some(f) = intercept(n) {
            return Some(f);
        }
        // SAFETY: `instance` is live when non-null.
        let data = get(instances(), unsafe { key(instance) })?;
        // SAFETY: forwarding to the next layer with the caller's arguments.
        unsafe { (data.next_gipa)(instance, name) }
    })
}

#[allow(unsafe_code)]
unsafe extern "system" fn get_device_proc_addr(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    entry(None, || {
        if name.is_null() {
            return None;
        }
        // SAFETY: the caller passes a NUL-terminated name.
        let n = unsafe { CStr::from_ptr(name) };
        if matches!(
            n.to_bytes(),
            b"vkGetDeviceProcAddr"
                | b"vkDestroyDevice"
                | b"vkCreateSwapchainKHR"
                | b"vkDestroySwapchainKHR"
                | b"vkQueuePresentKHR"
        ) {
            return intercept(n);
        }
        // SAFETY: `device` is live when non-null.
        let data = get(devices(), unsafe { key(device) })?;
        // SAFETY: forwarding to the next layer with the caller's arguments.
        unsafe { (data.next_gdpa)(device, name) }
    })
}

// ---- instance ------------------------------------------------------------------------------

#[allow(unsafe_code)]
unsafe extern "system" fn create_instance(
    info: *const vk::InstanceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Instance,
) -> vk::Result {
    entry(vk::Result::ERROR_INITIALIZATION_FAILED, || {
        // SAFETY: `info` is the application's valid create info.
        let chain = unsafe {
            find_layer_info(
                (*info).p_next,
                vk::StructureType::LOADER_INSTANCE_CREATE_INFO,
                VK_LAYER_LINK_INFO,
            )
        };
        if chain.is_null() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        // SAFETY: the loader's link info; we advance it for the next layer, as required.
        let next_gipa = unsafe {
            let link = (*chain).u.cast::<LayerInstanceLink>();
            if link.is_null() {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            }
            (*chain).u = (*link).p_next.cast();
            (*link).pfn_next_get_instance_proc_addr
        };
        // SAFETY: a global command looked up through the next layer.
        let Some(create) =
            (unsafe { next_gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()) })
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        // SAFETY: `create` is vkCreateInstance of the next layer.
        let create: vk::PFN_vkCreateInstance = unsafe { std::mem::transmute(create) };
        // SAFETY: forwarding the caller's arguments.
        let result = unsafe { create(info, alloc, out) };
        if result != vk::Result::SUCCESS {
            return result;
        }
        ACTIVE.store(true, Ordering::Relaxed);
        // SAFETY: `out` now holds the new instance.
        let handle = unsafe { *out };
        let load = |name: &CStr| {
            // SAFETY: instance-level lookups through the next layer.
            unsafe {
                std::mem::transmute::<vk::PFN_vkVoidFunction, *const c_void>(next_gipa(
                    handle,
                    name.as_ptr(),
                ))
            }
        };
        // SAFETY: loading function pointers for a live instance.
        let instance = unsafe { ash::Instance::load_with(load, handle) };
        let surface = ash::khr::surface::InstanceFn::load(load);
        instances()
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            // SAFETY: `handle` is live.
            .insert(
                unsafe { key(handle) },
                Arc::new(InstanceData {
                    next_gipa,
                    instance,
                    surface,
                }),
            );
        let _ = link();
        vk::Result::SUCCESS
    })
}

#[allow(unsafe_code)]
unsafe extern "system" fn destroy_instance(
    instance: vk::Instance,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    entry((), || {
        // SAFETY: `instance` is live until the call below.
        let k = unsafe { key(instance) };
        let data = instances()
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&k);
        if let Some(data) = data {
            // SAFETY: forwarding the caller's arguments.
            unsafe { (data.instance.fp_v1_0().destroy_instance)(instance, alloc) };
        }
    });
}

// ---- device --------------------------------------------------------------------------------

#[allow(unsafe_code)]
unsafe extern "system" fn create_device(
    physical: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    entry(vk::Result::ERROR_INITIALIZATION_FAILED, || {
        // SAFETY: `physical` is live; its key is its instance's.
        let instance_key = unsafe { key(physical) };
        let Some(inst) = get(instances(), instance_key) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        // SAFETY: `info` is the application's valid create info.
        let (chain, loader_data) = unsafe {
            (
                find_layer_info(
                    (*info).p_next,
                    vk::StructureType::LOADER_DEVICE_CREATE_INFO,
                    VK_LAYER_LINK_INFO,
                ),
                find_layer_info(
                    (*info).p_next,
                    vk::StructureType::LOADER_DEVICE_CREATE_INFO,
                    VK_LOADER_DATA_CALLBACK,
                ),
            )
        };
        if chain.is_null() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        // SAFETY: the loader's link info; advanced for the next layer.
        let (next_gipa, next_gdpa) = unsafe {
            let link = (*chain).u.cast::<LayerDeviceLink>();
            if link.is_null() {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            }
            (*chain).u = (*link).p_next.cast();
            (
                (*link).pfn_next_get_instance_proc_addr,
                (*link).pfn_next_get_device_proc_addr,
            )
        };
        let set_loader_data = if loader_data.is_null() {
            None
        } else {
            // SAFETY: for VK_LOADER_DATA_CALLBACK the union holds pfnSetDeviceLoaderData.
            Some(unsafe {
                std::mem::transmute::<*mut c_void, SetDeviceLoaderData>((*loader_data).u)
            })
        };
        // SAFETY: looked up through the next layer for this instance.
        let Some(create) =
            (unsafe { next_gipa(inst.instance.handle(), c"vkCreateDevice".as_ptr()) })
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        // SAFETY: `create` is vkCreateDevice of the next layer.
        let create: vk::PFN_vkCreateDevice = unsafe { std::mem::transmute(create) };
        // SAFETY: forwarding the caller's arguments.
        let result = unsafe { create(physical, info, alloc, out) };
        if result != vk::Result::SUCCESS {
            return result;
        }
        // SAFETY: `out` now holds the new device.
        let handle = unsafe { *out };
        let load = |name: &CStr| {
            // SAFETY: device-level lookups through the next layer.
            unsafe {
                std::mem::transmute::<vk::PFN_vkVoidFunction, *const c_void>(next_gdpa(
                    handle,
                    name.as_ptr(),
                ))
            }
        };
        // SAFETY: loading function pointers for a live device.
        let device = unsafe { ash::Device::load_with(load, handle) };
        let swapchain = ash::khr::swapchain::DeviceFn::load(load);
        // SAFETY: queries on a live physical device.
        let (families, memory) = unsafe {
            (
                inst.instance
                    .get_physical_device_queue_family_properties(physical),
                inst.instance
                    .get_physical_device_memory_properties(physical),
            )
        };
        // SAFETY: the application's queue create infos (`queueCreateInfoCount` elements).
        let requested: &[vk::DeviceQueueCreateInfo<'_>] = unsafe {
            if (*info).p_queue_create_infos.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(
                    (*info).p_queue_create_infos,
                    (*info).queue_create_info_count as usize,
                )
            }
        };
        let mut queues = Vec::new();
        for q in requested.iter().filter(|q| q.flags.is_empty()) {
            for i in 0..q.queue_count {
                // SAFETY: a queue the application created with this device.
                queues.push((
                    unsafe { device.get_device_queue(q.queue_family_index, i) },
                    q.queue_family_index,
                ));
            }
        }
        let data = DeviceData {
            handle,
            queues,
            instance_key,
            physical,
            next_gdpa,
            device,
            swapchain,
            set_loader_data,
            families,
            memory,
            state: Mutex::default(),
        };
        devices()
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            // SAFETY: `handle` is live.
            .insert(unsafe { key(handle) }, Arc::new(data));
        vk::Result::SUCCESS
    })
}

#[allow(unsafe_code)]
unsafe extern "system" fn destroy_device(
    device: vk::Device,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    entry((), || {
        // SAFETY: `device` is live until the call below.
        let k = unsafe { key(device) };
        let data = devices()
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&k);
        if let Some(data) = data {
            let mut st = lock(&data.state);
            let swapchains: Vec<Swapchain> = st.swapchains.drain().map(|(_, s)| s).collect();
            for s in swapchains {
                data.free_slots(&st.pools, s.slots);
            }
            for (_, pool) in st.pools.drain() {
                // SAFETY: our pool; its command buffers finished (fences waited above).
                unsafe { data.device.destroy_command_pool(pool, None) };
            }
            drop(st);
            // SAFETY: forwarding the caller's arguments.
            unsafe { (data.device.fp_v1_0().destroy_device)(device, alloc) };
        }
    });
}

// ---- swapchain -----------------------------------------------------------------------------

#[allow(unsafe_code)]
unsafe extern "system" fn create_swapchain(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    alloc: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    entry(vk::Result::ERROR_INITIALIZATION_FAILED, || {
        // SAFETY: `device` is live.
        let Some(data) = get(devices(), unsafe { key(device) }) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        // SAFETY: the application's valid create info, copied so we can add a usage flag.
        let mut ours = unsafe { *info };
        let format_ok = matches!(
            ours.image_format,
            vk::Format::B8G8R8A8_UNORM
                | vk::Format::B8G8R8A8_SRGB
                | vk::Format::R8G8B8A8_UNORM
                | vk::Format::R8G8B8A8_SRGB
        );
        let transfer =
            format_ok && !guard::is_disabled() && data.surface_takes_transfer(ours.surface);
        if transfer {
            ours.image_usage |= vk::ImageUsageFlags::TRANSFER_DST;
        }
        // SAFETY: forwarding (possibly with the extra usage bit) to the next layer.
        let result = unsafe { (data.swapchain.create_swapchain_khr)(device, &ours, alloc, out) };
        if result != vk::Result::SUCCESS {
            return result;
        }
        // SAFETY: `out` now holds the new swapchain.
        let handle = unsafe { *out };
        let images = data.swapchain_images(handle).unwrap_or_default();
        let bgra = matches!(
            ours.image_format,
            vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB
        );
        lock(&data.state).swapchains.insert(
            handle,
            Swapchain {
                usable: transfer && !images.is_empty(),
                images,
                extent: ours.image_extent,
                bgra,
                slots: HashMap::new(),
                cards: Cards::default(),
            },
        );
        vk::Result::SUCCESS
    })
}

#[allow(unsafe_code)]
unsafe extern "system" fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    alloc: *const vk::AllocationCallbacks<'_>,
) {
    entry((), || {
        // SAFETY: `device` is live.
        let Some(data) = get(devices(), unsafe { key(device) }) else {
            return;
        };
        let mut st = lock(&data.state);
        if let Some(s) = st.swapchains.remove(&swapchain) {
            data.free_slots(&st.pools, s.slots);
        }
        drop(st);
        // SAFETY: forwarding the caller's arguments.
        unsafe { (data.swapchain.destroy_swapchain_khr)(device, swapchain, alloc) };
    });
}

// ---- present -------------------------------------------------------------------------------

#[allow(unsafe_code)]
unsafe extern "system" fn queue_present(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    // The game's present must happen even if something below panics: the fallback path
    // re-reads what it needs without our state.
    // SAFETY: `queue` is live; its key is its device's.
    let Some(data) = get(devices(), unsafe { key(queue) }) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let mut wait = [vk::Semaphore::null()];
    // SAFETY: the application's valid present info, copied to replace the wait semaphores.
    let mut ours = unsafe { *info };
    let visible = !guard::is_disabled() && link().is_some_and(|l| l.should_draw());
    if visible
        && let Some(Some(done)) = guard::guarded(|| {
            // SAFETY: `info` stays valid for the duration of this call.
            data.draw(queue, unsafe { &*info })
        })
    {
        wait[0] = done;
        ours.wait_semaphore_count = 1;
        ours.p_wait_semaphores = wait.as_ptr();
    }
    // SAFETY: forwarding to the next layer; `wait` outlives the call.
    unsafe { (data.swapchain.queue_present_khr)(queue, &ours) }
}

impl DeviceData {
    fn instance(&self) -> Option<Arc<InstanceData>> {
        get(instances(), self.instance_key)
    }

    #[allow(unsafe_code)]
    fn surface_takes_transfer(&self, surface: vk::SurfaceKHR) -> bool {
        let Some(inst) = self.instance() else {
            return false;
        };
        let mut caps = vk::SurfaceCapabilitiesKHR::default();
        // SAFETY: a query on live handles with a valid output pointer.
        let r = unsafe {
            (inst.surface.get_physical_device_surface_capabilities_khr)(
                self.physical,
                surface,
                &mut caps,
            )
        };
        r == vk::Result::SUCCESS
            && caps
                .supported_usage_flags
                .contains(vk::ImageUsageFlags::TRANSFER_DST)
    }

    #[allow(unsafe_code)]
    fn swapchain_images(&self, swapchain: vk::SwapchainKHR) -> Option<Vec<vk::Image>> {
        let mut count = 0u32;
        let f = self.swapchain.get_swapchain_images_khr;
        // SAFETY: the two-call idiom on a live swapchain.
        unsafe {
            if f(self.handle, swapchain, &mut count, std::ptr::null_mut()) != vk::Result::SUCCESS {
                return None;
            }
            let mut images = vec![vk::Image::null(); count as usize];
            if f(self.handle, swapchain, &mut count, images.as_mut_ptr()) != vk::Result::SUCCESS {
                return None;
            }
            images.truncate(count as usize);
            Some(images)
        }
    }

    /// Copies the cards into the image being presented; returns the semaphore the present
    /// must wait on instead of the game's, or `None` to present untouched.
    fn draw(&self, queue: vk::Queue, info: &vk::PresentInfoKHR<'_>) -> Option<vk::Semaphore> {
        let started = Instant::now();
        let link = link()?;
        let mut st = lock(&self.state);
        if st.broken || info.swapchain_count == 0 {
            return None;
        }
        #[allow(unsafe_code)]
        // SAFETY: the present info's arrays have `swapchain_count` elements.
        let (swapchain, index) = unsafe { (*info.p_swapchains, *info.p_image_indices) };
        let family = self.family_of(queue)?;
        let result = self.copy_cards(&mut st, link, queue, family, swapchain, index, info);
        let result = match result {
            Ok(r) => r,
            Err(_) => {
                st.broken = true;
                None
            }
        };
        let took = started.elapsed();
        if result.is_some() {
            DRAWN_FRAMES.fetch_add(1, Ordering::Relaxed);
            DRAW_NANOS.fetch_add(
                u64::try_from(took.as_nanos()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        if !st.budget.record(took) {
            guard::disable();
        }
        result
    }

    /// The family of `queue`, if it is one the game created and it can run transfers.
    fn family_of(&self, queue: vk::Queue) -> Option<u32> {
        let family = self.queues.iter().find(|(q, _)| *q == queue)?.1;
        let transfer =
            vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER;
        self.families
            .get(family as usize)
            .is_some_and(|p| p.queue_flags.intersects(transfer))
            .then_some(family)
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(unsafe_code)]
    fn copy_cards(
        &self,
        st: &mut DeviceState,
        link: &Link,
        queue: vk::Queue,
        family: u32,
        swapchain: vk::SwapchainKHR,
        index: u32,
        info: &vk::PresentInfoKHR<'_>,
    ) -> Result<Option<vk::Semaphore>, vk::Result> {
        let pool = match st.pools.get(&family) {
            Some(p) => *p,
            None => {
                let ci = vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(family);
                // SAFETY: a valid create info on a live device.
                let p = unsafe { self.device.create_command_pool(&ci, None)? };
                st.pools.insert(family, p);
                p
            }
        };
        let pools = st.pools.clone();
        let Some(sc) = st.swapchains.get_mut(&swapchain) else {
            return Ok(None);
        };
        if !sc.usable {
            return Ok(None);
        }
        let Some(image) = sc.images.get(index as usize).copied() else {
            return Ok(None);
        };
        // Recompose only when what is visible changed.
        let (view, alive): (Arc<View>, Vec<bool>) = link.view();
        let key = CardsKey {
            view: Arc::as_ptr(&view) as usize,
            alive,
            panel: link.panel_open(),
        };
        if sc.cards.key.as_ref() != Some(&key) {
            sc.cards.regions = draw::compose(
                &view,
                &key.alive,
                key.panel,
                sc.extent.width,
                sc.extent.height,
            );
            sc.cards.generation += 1;
            sc.cards.key = Some(key);
        }
        if sc.cards.regions.is_empty() {
            return Ok(None);
        }
        let bytes: u64 = sc
            .cards
            .regions
            .iter()
            .map(|r| r.pixels.len() as u64 * 4)
            .sum();

        // A slot whose previous copy is still running: skip this frame rather than stall.
        if let Some(slot) = sc.slots.get(&index) {
            // SAFETY: our fence on a live device.
            match unsafe {
                self.device
                    .wait_for_fences(&[slot.fence], true, SLOT_WAIT_NS)
            } {
                Ok(()) => {}
                Err(vk::Result::TIMEOUT) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        let stale = sc
            .slots
            .get(&index)
            .is_some_and(|s| s.family != family || s.size < bytes);
        if stale && let Some(old) = sc.slots.remove(&index) {
            self.free_slots(&pools, HashMap::from([(index, old)]));
        }
        let slot = match sc.slots.entry(index) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(self.new_slot(pool, family, bytes)?)
            }
        };

        // Staging: rewritten only when the cards changed since this image last got them.
        let mut copies = Vec::with_capacity(sc.cards.regions.len());
        let mut offset = 0u64;
        for r in &sc.cards.regions {
            copies.push(
                vk::BufferImageCopy::default()
                    .buffer_offset(offset)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_offset(vk::Offset3D {
                        x: r.x as i32,
                        y: r.y as i32,
                        z: 0,
                    })
                    .image_extent(vk::Extent3D {
                        width: r.width,
                        height: r.height,
                        depth: 1,
                    }),
            );
            if slot.generation != sc.cards.generation {
                let len = r.pixels.len() * 4;
                // SAFETY: `mapped` covers `size >= bytes` bytes; regions are laid out back to back.
                let dst = unsafe {
                    std::slice::from_raw_parts_mut(slot.mapped.add(offset as usize), len)
                };
                for (px, out) in r.pixels.iter().zip(dst.as_chunks_mut::<4>().0) {
                    let [b, g, rr, a] = px.to_le_bytes();
                    let v = if sc.bgra {
                        [b, g, rr, a]
                    } else {
                        [rr, g, b, a]
                    };
                    out.copy_from_slice(&v);
                }
            }
            offset += r.pixels.len() as u64 * 4;
        }
        slot.generation = sc.cards.generation;

        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let to_dst = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range);
        let to_present = to_dst
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR);
        // SAFETY: recording and submitting our own command buffer, whose previous submission
        // finished (fence waited above), against live handles.
        unsafe {
            self.device
                .reset_command_buffer(slot.cmd, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                slot.cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.device.cmd_pipeline_barrier(
                slot.cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_dst],
            );
            self.device.cmd_copy_buffer_to_image(
                slot.cmd,
                slot.buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &copies,
            );
            self.device.cmd_pipeline_barrier(
                slot.cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            self.device.end_command_buffer(slot.cmd)?;
            self.device.reset_fences(&[slot.fence])?;
            let waits: &[vk::Semaphore] =
                if info.wait_semaphore_count == 0 || info.p_wait_semaphores.is_null() {
                    &[]
                } else {
                    std::slice::from_raw_parts(
                        info.p_wait_semaphores,
                        info.wait_semaphore_count as usize,
                    )
                };
            let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; waits.len()];
            let cmds = [slot.cmd];
            let signal = [slot.done];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&cmds)
                .signal_semaphores(&signal);
            self.device.queue_submit(queue, &[submit], slot.fence)?;
        }
        Ok(Some(slot.done))
    }

    #[allow(unsafe_code)]
    fn new_slot(&self, pool: vk::CommandPool, family: u32, bytes: u64) -> Result<Slot, vk::Result> {
        // Round up so small growth does not reallocate every time.
        let size = bytes.next_power_of_two().max(64 * 1024);
        // SAFETY: creating and binding our own objects on a live device; every object made
        // before a failure is destroyed again.
        unsafe {
            let d = &self.device;
            let cmd = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )?
                .first()
                .copied()
                .ok_or(vk::Result::ERROR_OUT_OF_HOST_MEMORY)?;
            // Command buffers made by a layer need the loader's dispatch pointer.
            if let Some(set) = self.set_loader_data {
                let r = set(self.handle, cmd.as_raw() as usize as *mut c_void);
                if r != vk::Result::SUCCESS {
                    d.free_command_buffers(pool, &[cmd]);
                    return Err(r);
                }
            } else {
                *(cmd.as_raw() as usize as *mut usize) = key(self.handle);
            }
            let fence = d.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;
            let done = match d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
                Ok(s) => s,
                Err(e) => {
                    d.destroy_fence(fence, None);
                    d.free_command_buffers(pool, &[cmd]);
                    return Err(e);
                }
            };
            let cleanup = |d: &ash::Device| {
                d.destroy_semaphore(done, None);
                d.destroy_fence(fence, None);
                d.free_command_buffers(pool, &[cmd]);
            };
            let buffer = match d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            ) {
                Ok(b) => b,
                Err(e) => {
                    cleanup(d);
                    return Err(e);
                }
            };
            let req = d.get_buffer_memory_requirements(buffer);
            let wanted =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let Some(type_index) = self
                .memory
                .memory_types_as_slice()
                .iter()
                .enumerate()
                .find(|(i, t)| {
                    req.memory_type_bits & (1 << i) != 0 && t.property_flags.contains(wanted)
                })
                .map(|(i, _)| i as u32)
            else {
                d.destroy_buffer(buffer, None);
                cleanup(d);
                return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
            };
            let memory = match d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(type_index),
                None,
            ) {
                Ok(m) => m,
                Err(e) => {
                    d.destroy_buffer(buffer, None);
                    cleanup(d);
                    return Err(e);
                }
            };
            let mapped = match d
                .bind_buffer_memory(buffer, memory, 0)
                .and_then(|()| d.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()))
            {
                Ok(p) => p.cast::<u8>(),
                Err(e) => {
                    d.free_memory(memory, None);
                    d.destroy_buffer(buffer, None);
                    cleanup(d);
                    return Err(e);
                }
            };
            Ok(Slot {
                family,
                cmd,
                fence,
                done,
                buffer,
                memory,
                mapped,
                size,
                generation: 0,
            })
        }
    }

    #[allow(unsafe_code)]
    fn free_slots(&self, pools: &HashMap<u32, vk::CommandPool>, slots: HashMap<u32, Slot>) {
        for (_, s) in slots {
            // SAFETY: our objects; waiting on the fence first so none is in use.
            unsafe {
                let _ = self.device.wait_for_fences(&[s.fence], true, 1_000_000_000);
                self.device.unmap_memory(s.memory);
                self.device.destroy_buffer(s.buffer, None);
                self.device.free_memory(s.memory, None);
                self.device.destroy_semaphore(s.done, None);
                self.device.destroy_fence(s.fence, None);
                if let Some(pool) = pools.get(&s.family) {
                    self.device.free_command_buffers(*pool, &[s.cmd]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_trips_only_on_a_slow_average() {
        let mut b = Budget::default();
        for _ in 0..BUDGET_WINDOW * 3 {
            assert!(b.record(Duration::from_micros(200)));
        }
        let mut tripped = false;
        for _ in 0..BUDGET_WINDOW {
            tripped |= !b.record(Duration::from_millis(3));
        }
        assert!(tripped);
    }
}
