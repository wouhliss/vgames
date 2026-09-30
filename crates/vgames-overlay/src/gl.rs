//! Linux OpenGL hooks (05-social §6.1, A4-T11): the overlay library loaded with `LD_PRELOAD`
//! draws into the frame of GL games just before `glXSwapBuffers` / `eglSwapBuffers`.
//! MangoHud (MIT) is the reference for the interposition techniques.
//!
//! How it gets in: the library exports those swap functions (and `glXGetProcAddress[ARB]`,
//! `eglGetProcAddress`) so the dynamic linker binds the game to them first, and `dlsym`, so a
//! game that opens `libGL`/`libEGL` itself with `RTLD_LOCAL` (SDL, GLFW) and looks the
//! functions up by handle gets ours too. The real functions are remembered where they were
//! found and called after drawing. Everything else passes through untouched.
//!
//! How it draws: the cards from [`crate::draw`] are opaque, so a pixel copy is enough and no
//! shader, vertex array or blend state of the game is touched: the cards live in one RGBA
//! texture attached to a private framebuffer, and `glBlitFramebuffer` copies each card into the
//! default framebuffer (GL 3.0 / ES 3.0, core and compatibility profiles). The few pieces of
//! state the copy depends on (framebuffer bindings, the texture binding, pixel-unpack state,
//! scissor test, sRGB conversion) are saved and restored. The GL error queue is not touched,
//! so the game's own `glGetError` logic sees exactly what it did before.
//!
//! Cost: while nothing is visible a swap costs two atomic loads. While visible the texture is
//! re-uploaded only when the cards change, and the hooks turn themselves off if drawing keeps
//! exceeding [`FRAME_BUDGET`]. Every hook catches panics (see [`crate::guard`]); after one, or
//! when the Vulkan layer is active in the process, the hooks only forward.
//!
//! Input: none. On Linux the panel is reached with the hotkey or the Guide button, and "reply"
//! opens the launcher.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::draw::{self, Region};
use crate::guard;
use crate::link::{Endpoint, Link};
use crate::protocol::RendererKind;

/// Drawing may take this long per frame on average before the hooks turn themselves off.
pub const FRAME_BUDGET: Duration = Duration::from_millis(1);
/// Frames averaged for the budget.
const BUDGET_WINDOW: u32 = 120;

static DRAWN_FRAMES: AtomicU64 = AtomicU64::new(0);
static DRAW_NANOS: AtomicU64 = AtomicU64::new(0);
static WINDOW_FRAMES: AtomicU32 = AtomicU32::new(0);
static WINDOW_NANOS: AtomicU64 = AtomicU64::new(0);

/// Frames the GL hooks drew into so far and the CPU time they spent (for benchmarks).
pub fn draw_stats() -> (u64, Duration) {
    (
        DRAWN_FRAMES.load(Ordering::Relaxed),
        Duration::from_nanos(DRAW_NANOS.load(Ordering::Relaxed)),
    )
}

/// [`draw_stats`] for a harness that loaded the library.
///
/// # Safety
/// Both pointers must be valid for a `u64` write (null pointers are skipped).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vgames_overlay_gl_draw_stats(frames: *mut u64, nanos: *mut u64) {
    let (f, d) = draw_stats();
    // SAFETY: the caller passes writable pointers or null.
    unsafe {
        if !frames.is_null() {
            *frames = f;
        }
        if !nanos.is_null() {
            *nanos = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        }
    }
}

/// Records one drawn frame; turns the overlay off when the average is over budget.
fn record(took: Duration) {
    let nanos = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
    DRAWN_FRAMES.fetch_add(1, Ordering::Relaxed);
    DRAW_NANOS.fetch_add(nanos, Ordering::Relaxed);
    let spent = WINDOW_NANOS
        .fetch_add(nanos, Ordering::Relaxed)
        .saturating_add(nanos);
    let frames = WINDOW_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if frames >= BUDGET_WINDOW {
        WINDOW_FRAMES.store(0, Ordering::Relaxed);
        WINDOW_NANOS.store(0, Ordering::Relaxed);
        if Duration::from_nanos(spent / u64::from(frames)) > FRAME_BUDGET {
            guard::disable();
        }
    }
}

// ---- the C runtime -----------------------------------------------------------------------------

type DlsymFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;

unsafe extern "C" {
    fn dlvsym(handle: *mut c_void, name: *const c_char, version: *const c_char) -> *mut c_void;
}

/// `RTLD_NEXT`: the next object after the caller in the search order.
const RTLD_NEXT: *mut c_void = -1isize as *mut c_void;
/// `RTLD_DEFAULT`.
const RTLD_DEFAULT: *mut c_void = ptr::null_mut();

/// The C library's own `dlsym` (ours replaces it for the whole process when preloaded).
fn real_dlsym() -> Option<DlsymFn> {
    static REAL: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    let mut p = REAL.load(Ordering::Acquire);
    if p.is_null() {
        // glibc 2.34 moved `dlsym` into libc under a new version; older versions, and other
        // architectures, carry it under their base version.
        for version in [c"GLIBC_2.34", c"GLIBC_2.2.5", c"GLIBC_2.17"] {
            // SAFETY: a versioned lookup in the objects after this one; nothing is called.
            p = unsafe { dlvsym(RTLD_NEXT, c"dlsym".as_ptr(), version.as_ptr()) };
            if !p.is_null() {
                REAL.store(p, Ordering::Release);
                break;
            }
        }
    }
    if p.is_null() {
        None
    } else {
        // SAFETY: `p` is glibc's `dlsym`, which has exactly this signature.
        Some(unsafe { std::mem::transmute::<*mut c_void, DlsymFn>(p) })
    }
}

// ---- the hooked functions ------------------------------------------------------------------------

type Slot = AtomicPtr<c_void>;

static REAL_GLX_SWAP: Slot = AtomicPtr::new(ptr::null_mut());
static REAL_GLX_GPA: Slot = AtomicPtr::new(ptr::null_mut());
static REAL_GLX_GPA_ARB: Slot = AtomicPtr::new(ptr::null_mut());
static REAL_EGL_SWAP: Slot = AtomicPtr::new(ptr::null_mut());
static REAL_EGL_SWAP_KHR: Slot = AtomicPtr::new(ptr::null_mut());
static REAL_EGL_SWAP_EXT: Slot = AtomicPtr::new(ptr::null_mut());
static REAL_EGL_GPA: Slot = AtomicPtr::new(ptr::null_mut());

struct Hook {
    name: &'static CStr,
    ours: *mut c_void,
    real: &'static Slot,
}

fn hooks() -> [Hook; 7] {
    [
        Hook {
            name: c"glXSwapBuffers",
            ours: glXSwapBuffers as *mut c_void,
            real: &REAL_GLX_SWAP,
        },
        Hook {
            name: c"glXGetProcAddress",
            ours: glXGetProcAddress as *mut c_void,
            real: &REAL_GLX_GPA,
        },
        Hook {
            name: c"glXGetProcAddressARB",
            ours: glXGetProcAddressARB as *mut c_void,
            real: &REAL_GLX_GPA_ARB,
        },
        Hook {
            name: c"eglSwapBuffers",
            ours: eglSwapBuffers as *mut c_void,
            real: &REAL_EGL_SWAP,
        },
        Hook {
            name: c"eglSwapBuffersWithDamageKHR",
            ours: eglSwapBuffersWithDamageKHR as *mut c_void,
            real: &REAL_EGL_SWAP_KHR,
        },
        Hook {
            name: c"eglSwapBuffersWithDamageEXT",
            ours: eglSwapBuffersWithDamageEXT as *mut c_void,
            real: &REAL_EGL_SWAP_EXT,
        },
        Hook {
            name: c"eglGetProcAddress",
            ours: eglGetProcAddress as *mut c_void,
            real: &REAL_EGL_GPA,
        },
    ]
}

/// What to hand the application for `name` when the real lookup found `found`: ours when it is
/// a function we hook (remembering the real one), `found` otherwise. Allocation-free and
/// panic-free: it runs inside every `dlsym` of the process.
fn substitute(name: *const c_char, found: *mut c_void) -> *mut c_void {
    if name.is_null() || found.is_null() {
        return found;
    }
    // SAFETY: the caller passed a NUL-terminated name (it is what `dlsym` requires).
    let name = unsafe { CStr::from_ptr(name) };
    for h in hooks() {
        if h.name == name {
            if found != h.ours {
                let _ = h.real.compare_exchange(
                    ptr::null_mut(),
                    found,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            return h.ours;
        }
    }
    found
}

/// The real function behind `slot`, looked up after this library when nobody looked it up
/// through a handle or `glXGetProcAddress` yet.
fn real_fn(slot: &Slot, name: &CStr) -> *mut c_void {
    let p = slot.load(Ordering::Acquire);
    if !p.is_null() {
        return p;
    }
    let Some(d) = real_dlsym() else {
        return ptr::null_mut();
    };
    // SAFETY: `name` is NUL-terminated.
    let p = unsafe { d(RTLD_NEXT, name.as_ptr()) };
    if !p.is_null() {
        let _ = slot.compare_exchange(ptr::null_mut(), p, Ordering::AcqRel, Ordering::Acquire);
    }
    p
}

/// `dlsym` for the whole process (preloaded): functions we hook come back as ours.
///
/// # Safety
/// As `dlsym`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    let Some(real) = real_dlsym() else {
        return ptr::null_mut();
    };
    // SAFETY: forwarding the caller's arguments to the C library's `dlsym`.
    let found = unsafe { real(handle, name) };
    let ours = substitute(name, found);
    if ours != found && handle != RTLD_NEXT && handle != RTLD_DEFAULT {
        // A library opened with `RTLD_LOCAL` is only reachable through its handle, and the
        // GL functions are found through its `GetProcAddress`: take that one along.
        // SAFETY: `name` is NUL-terminated (checked by `substitute`), `handle` is the
        // caller's own.
        let gl_name = unsafe { CStr::from_ptr(name) }.to_bytes();
        let sibling = if gl_name.starts_with(b"glX") {
            Some((&REAL_GLX_GPA_ARB, c"glXGetProcAddressARB"))
        } else if gl_name.starts_with(b"egl") {
            Some((&REAL_EGL_GPA, c"eglGetProcAddress"))
        } else {
            None
        };
        if let Some((slot, sibling)) = sibling {
            // SAFETY: as above.
            let p = unsafe { real(handle, sibling.as_ptr()) };
            if !p.is_null() && p != ours {
                let _ =
                    slot.compare_exchange(ptr::null_mut(), p, Ordering::AcqRel, Ordering::Acquire);
            }
        }
    }
    ours
}

type GpaFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;

unsafe fn proc_address(slot: &Slot, own_name: &CStr, name: *const c_char) -> *mut c_void {
    let real = real_fn(slot, own_name);
    if real.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the real `glXGetProcAddress[ARB]` / `eglGetProcAddress`: one C string in, one
    // function pointer out.
    let found = unsafe { std::mem::transmute::<*mut c_void, GpaFn>(real)(name) };
    substitute(name, found)
}

/// # Safety
/// As `glXGetProcAddress`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddress(name: *const c_char) -> *mut c_void {
    // SAFETY: forwarded as is.
    unsafe { proc_address(&REAL_GLX_GPA, c"glXGetProcAddress", name) }
}

/// # Safety
/// As `glXGetProcAddressARB`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddressARB(name: *const c_char) -> *mut c_void {
    // SAFETY: forwarded as is.
    unsafe { proc_address(&REAL_GLX_GPA_ARB, c"glXGetProcAddressARB", name) }
}

/// # Safety
/// As `eglGetProcAddress`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetProcAddress(name: *const c_char) -> *mut c_void {
    // SAFETY: forwarded as is.
    unsafe { proc_address(&REAL_EGL_GPA, c"eglGetProcAddress", name) }
}

/// The GL functions of the backend that is in use are found through its own
/// `GetProcAddress`, which is also how the game finds them.
fn gpa_for(backend: Backend) -> Option<GpaFn> {
    let slots: &[(&Slot, &CStr)] = match backend {
        Backend::Glx => &[
            (&REAL_GLX_GPA_ARB, c"glXGetProcAddressARB"),
            (&REAL_GLX_GPA, c"glXGetProcAddress"),
        ],
        Backend::Egl => &[(&REAL_EGL_GPA, c"eglGetProcAddress")],
    };
    slots.iter().find_map(|(slot, name)| {
        let p = real_fn(slot, name);
        // SAFETY: a `GetProcAddress` function of the GL library (see above).
        (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, GpaFn>(p) })
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Glx,
    Egl,
}

// ---- the hooks ----------------------------------------------------------------------------------

/// The broker link, started with the first swap (none without the launch environment).
fn link() -> Option<&'static Arc<Link>> {
    static LINK: OnceLock<Option<Arc<Link>>> = OnceLock::new();
    LINK.get_or_init(|| Endpoint::from_env().map(|e| Link::start(e, RendererKind::OpenGl)))
        .as_ref()
}

/// The per-frame check: one atomic per condition, nothing else while hidden.
#[inline]
fn visible() -> Option<&'static Arc<Link>> {
    if guard::is_disabled() || crate::vulkan::ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    link().filter(|l| l.should_draw())
}

type GlxSwapFn = unsafe extern "C" fn(*mut c_void, c_ulong);

/// # Safety
/// As `glXSwapBuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXSwapBuffers(dpy: *mut c_void, drawable: c_ulong) {
    let real = real_fn(&REAL_GLX_SWAP, c"glXSwapBuffers");
    if real.is_null() {
        return;
    }
    if let Some(l) = visible() {
        let _ = guard::guarded(|| {
            let started = Instant::now();
            // SAFETY: `dpy`/`drawable` are the application's own arguments of this call.
            if unsafe { draw_glx(l, dpy, drawable) } {
                record(started.elapsed());
            }
        });
    }
    // SAFETY: the real `glXSwapBuffers` with the application's arguments.
    unsafe { std::mem::transmute::<*mut c_void, GlxSwapFn>(real)(dpy, drawable) }
}

type EglSwapFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
type EglSwapDamageFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_int, c_int) -> c_uint;

/// # Safety
/// As `eglSwapBuffers`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffers(dpy: *mut c_void, surface: *mut c_void) -> c_uint {
    let real = real_fn(&REAL_EGL_SWAP, c"eglSwapBuffers");
    if real.is_null() {
        return 0;
    }
    // SAFETY: the application's own arguments.
    unsafe { egl_before_swap(dpy, surface) };
    // SAFETY: the real `eglSwapBuffers` with the application's arguments.
    unsafe { std::mem::transmute::<*mut c_void, EglSwapFn>(real)(dpy, surface) }
}

/// # Safety
/// As `eglSwapBuffersWithDamageKHR`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffersWithDamageKHR(
    dpy: *mut c_void,
    surface: *mut c_void,
    rects: *mut c_int,
    n: c_int,
) -> c_uint {
    let real = real_fn(&REAL_EGL_SWAP_KHR, c"eglSwapBuffersWithDamageKHR");
    if real.is_null() {
        return 0;
    }
    // SAFETY: the application's own arguments.
    unsafe { egl_before_swap(dpy, surface) };
    // The cards may lie outside the damage the game reports: a full swap is the safe choice
    // while they are visible.
    // SAFETY: the real function with the application's arguments.
    unsafe { std::mem::transmute::<*mut c_void, EglSwapDamageFn>(real)(dpy, surface, rects, n) }
}

/// # Safety
/// As `eglSwapBuffersWithDamageEXT`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffersWithDamageEXT(
    dpy: *mut c_void,
    surface: *mut c_void,
    rects: *mut c_int,
    n: c_int,
) -> c_uint {
    let real = real_fn(&REAL_EGL_SWAP_EXT, c"eglSwapBuffersWithDamageEXT");
    if real.is_null() {
        return 0;
    }
    // SAFETY: the application's own arguments.
    unsafe { egl_before_swap(dpy, surface) };
    // SAFETY: the real function with the application's arguments.
    unsafe { std::mem::transmute::<*mut c_void, EglSwapDamageFn>(real)(dpy, surface, rects, n) }
}

unsafe fn egl_before_swap(dpy: *mut c_void, surface: *mut c_void) {
    if let Some(l) = visible() {
        let _ = guard::guarded(|| {
            let started = Instant::now();
            // SAFETY: the application's own arguments of the swap call.
            if unsafe { draw_egl(l, dpy, surface) } {
                record(started.elapsed());
            }
        });
    }
}

// ---- finding the drawable's size and the current context -------------------------------------------

/// Looks up `name` first in the libraries after this one, then through the backend's
/// `GetProcAddress` (a library the game opened with `RTLD_LOCAL` is only reachable that way).
fn lookup(backend: Backend, name: &CStr) -> *mut c_void {
    if let Some(d) = real_dlsym() {
        // SAFETY: `name` is NUL-terminated.
        let p = unsafe { d(RTLD_NEXT, name.as_ptr()) };
        if !p.is_null() {
            return p;
        }
        // SAFETY: as above.
        let p = unsafe { d(RTLD_DEFAULT, name.as_ptr()) };
        if !p.is_null()
            && p != hooks()
                .iter()
                .find(|h| h.name == name)
                .map_or(ptr::null_mut(), |h| h.ours)
        {
            return p;
        }
    }
    match gpa_for(backend) {
        // SAFETY: the backend's real `GetProcAddress`.
        Some(gpa) => unsafe { gpa(name.as_ptr()) },
        None => ptr::null_mut(),
    }
}

unsafe fn draw_glx(link: &Arc<Link>, dpy: *mut c_void, drawable: c_ulong) -> bool {
    static GL: OnceLock<Option<Gl>> = OnceLock::new();
    static QUERY: OnceLock<usize> = OnceLock::new();
    static CURRENT: OnceLock<usize> = OnceLock::new();
    let Some(gl) = GL.get_or_init(|| Gl::load(Backend::Glx)).as_ref() else {
        return false;
    };
    let query = *QUERY.get_or_init(|| lookup(Backend::Glx, c"glXQueryDrawable") as usize);
    let current = *CURRENT.get_or_init(|| lookup(Backend::Glx, c"glXGetCurrentContext") as usize);
    if query == 0 {
        return false;
    }
    // SAFETY: `glXQueryDrawable(Display*, GLXDrawable, int, unsigned*)` (GLX 1.3).
    let query: unsafe extern "C" fn(*mut c_void, c_ulong, c_int, *mut c_uint) =
        unsafe { std::mem::transmute(query) };
    let (mut w, mut h) = (0 as c_uint, 0 as c_uint);
    // SAFETY: GLX_WIDTH / GLX_HEIGHT of the drawable being swapped.
    unsafe {
        query(dpy, drawable, 0x801D, &mut w);
        query(dpy, drawable, 0x801E, &mut h);
    }
    let ctx = if current == 0 {
        0
    } else {
        // SAFETY: `glXGetCurrentContext() -> GLXContext`.
        let f: unsafe extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(current) };
        unsafe { f() as usize }
    };
    // SAFETY: a context is current on this thread: the application is about to swap.
    unsafe { draw_frame(gl, link, ctx, w, h) }
}

unsafe fn draw_egl(link: &Arc<Link>, dpy: *mut c_void, surface: *mut c_void) -> bool {
    static GL: OnceLock<Option<Gl>> = OnceLock::new();
    static QUERY: OnceLock<usize> = OnceLock::new();
    static CURRENT: OnceLock<usize> = OnceLock::new();
    let Some(gl) = GL.get_or_init(|| Gl::load(Backend::Egl)).as_ref() else {
        return false;
    };
    let query = *QUERY.get_or_init(|| lookup(Backend::Egl, c"eglQuerySurface") as usize);
    let current = *CURRENT.get_or_init(|| lookup(Backend::Egl, c"eglGetCurrentContext") as usize);
    if query == 0 {
        return false;
    }
    // SAFETY: `eglQuerySurface(EGLDisplay, EGLSurface, EGLint, EGLint*) -> EGLBoolean`.
    let query: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint =
        unsafe { std::mem::transmute(query) };
    let (mut w, mut h) = (0 as c_int, 0 as c_int);
    // SAFETY: EGL_WIDTH / EGL_HEIGHT of the surface being swapped.
    unsafe {
        query(dpy, surface, 0x3057, &mut w);
        query(dpy, surface, 0x3056, &mut h);
    }
    let ctx = if current == 0 {
        0
    } else {
        // SAFETY: `eglGetCurrentContext() -> EGLContext`.
        let f: unsafe extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(current) };
        unsafe { f() as usize }
    };
    // SAFETY: a context is current on this thread: the application is about to swap.
    unsafe {
        draw_frame(
            gl,
            link,
            ctx,
            u32::try_from(w).unwrap_or(0),
            u32::try_from(h).unwrap_or(0),
        )
    }
}

// ---- GL ---------------------------------------------------------------------------------------------

const GL_DRAW_FRAMEBUFFER: c_uint = 0x8CA9;
const GL_READ_FRAMEBUFFER: c_uint = 0x8CA8;
const GL_DRAW_FRAMEBUFFER_BINDING: c_uint = 0x8CA6;
const GL_READ_FRAMEBUFFER_BINDING: c_uint = 0x8CAA;
const GL_FRAMEBUFFER_COMPLETE: c_uint = 0x8CD5;
const GL_COLOR_ATTACHMENT0: c_uint = 0x8CE0;
const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_TEXTURE_BINDING_2D: c_uint = 0x8069;
const GL_RGBA: c_uint = 0x1908;
const GL_RGBA8: c_int = 0x8058;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_NEAREST: c_uint = 0x2600;
const GL_COLOR_BUFFER_BIT: c_uint = 0x4000;
const GL_SCISSOR_TEST: c_uint = 0x0C11;
const GL_FRAMEBUFFER_SRGB: c_uint = 0x8DB9;
const GL_PIXEL_UNPACK_BUFFER: c_uint = 0x88EC;
const GL_PIXEL_UNPACK_BUFFER_BINDING: c_uint = 0x88EF;
const GL_UNPACK_ROW_LENGTH: c_uint = 0x0CF2;
const GL_UNPACK_SKIP_ROWS: c_uint = 0x0CF3;
const GL_UNPACK_SKIP_PIXELS: c_uint = 0x0CF4;
const GL_UNPACK_ALIGNMENT: c_uint = 0x0CF5;
const GL_VERSION: c_uint = 0x1F02;

/// The GL entry points the copy needs (all present in GL 3.0 and ES 3.0).
struct Gl {
    get_integerv: unsafe extern "C" fn(c_uint, *mut c_int),
    get_string: unsafe extern "C" fn(c_uint) -> *const c_char,
    is_enabled: unsafe extern "C" fn(c_uint) -> u8,
    enable: unsafe extern "C" fn(c_uint),
    disable: unsafe extern "C" fn(c_uint),
    gen_textures: unsafe extern "C" fn(c_int, *mut c_uint),
    bind_texture: unsafe extern "C" fn(c_uint, c_uint),
    is_texture: unsafe extern "C" fn(c_uint) -> u8,
    tex_image_2d: unsafe extern "C" fn(
        c_uint,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
        *const c_void,
    ),
    tex_sub_image_2d: unsafe extern "C" fn(
        c_uint,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
        *const c_void,
    ),
    pixel_storei: unsafe extern "C" fn(c_uint, c_int),
    gen_framebuffers: unsafe extern "C" fn(c_int, *mut c_uint),
    bind_framebuffer: unsafe extern "C" fn(c_uint, c_uint),
    is_framebuffer: unsafe extern "C" fn(c_uint) -> u8,
    framebuffer_texture_2d: unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint, c_int),
    check_framebuffer_status: unsafe extern "C" fn(c_uint) -> c_uint,
    blit_framebuffer: unsafe extern "C" fn(
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
    ),
    bind_buffer: unsafe extern "C" fn(c_uint, c_uint),
}

/// A function pointer from its address.
///
/// # Safety
/// `p` must be the address of a function with the signature `T`.
unsafe fn cast<T: Copy>(p: *mut c_void) -> T {
    // SAFETY: `T` is a function pointer type, the same size as `*mut c_void` (checked below).
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) }
}

impl Gl {
    fn load(backend: Backend) -> Option<Self> {
        let gpa = gpa_for(backend)?;
        let get = |name: &CStr| -> Option<*mut c_void> {
            // SAFETY: the backend's real `GetProcAddress` with a NUL-terminated name.
            let p = unsafe { gpa(name.as_ptr()) };
            (!p.is_null()).then_some(p)
        };
        macro_rules! f {
            ($name:literal) => {
                // SAFETY: the entry point has the signature of the field it is stored in
                // (OpenGL 3.0 / OpenGL ES 3.0 specifications).
                unsafe { cast(get($name)?) }
            };
        }
        Some(Self {
            get_integerv: f!(c"glGetIntegerv"),
            get_string: f!(c"glGetString"),
            is_enabled: f!(c"glIsEnabled"),
            enable: f!(c"glEnable"),
            disable: f!(c"glDisable"),
            gen_textures: f!(c"glGenTextures"),
            bind_texture: f!(c"glBindTexture"),
            is_texture: f!(c"glIsTexture"),
            tex_image_2d: f!(c"glTexImage2D"),
            tex_sub_image_2d: f!(c"glTexSubImage2D"),
            pixel_storei: f!(c"glPixelStorei"),
            gen_framebuffers: f!(c"glGenFramebuffers"),
            bind_framebuffer: f!(c"glBindFramebuffer"),
            is_framebuffer: f!(c"glIsFramebuffer"),
            framebuffer_texture_2d: f!(c"glFramebufferTexture2D"),
            check_framebuffer_status: f!(c"glCheckFramebufferStatus"),
            blit_framebuffer: f!(c"glBlitFramebuffer"),
            bind_buffer: f!(c"glBindBuffer"),
        })
    }

    unsafe fn int(&self, pname: c_uint) -> c_int {
        let mut v = 0;
        // SAFETY: one integer for a single-valued state query.
        unsafe { (self.get_integerv)(pname, &mut v) };
        v
    }
}

/// What the copy needs, per GL context (objects are not shared across unrelated contexts).
#[derive(Default)]
struct Res {
    tex: c_uint,
    fbo: c_uint,
    /// Size of the texture's storage.
    tex_w: u32,
    tex_h: u32,
    /// OpenGL ES: no sRGB toggle to save.
    es: bool,
    /// Incomplete framebuffer or similar: this context gets no overlay.
    broken: bool,
    key: Option<Key>,
    regions: Vec<Region>,
    /// The texture holds `regions`.
    uploaded: bool,
}

#[derive(PartialEq)]
struct Key {
    view: usize,
    alive: Vec<bool>,
    panel: bool,
    width: u32,
    height: u32,
}

fn contexts() -> &'static Mutex<HashMap<usize, Res>> {
    static M: OnceLock<Mutex<HashMap<usize, Res>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// The state the copy touches, restored afterwards.
struct Saved {
    read_fb: c_int,
    draw_fb: c_int,
    tex: c_int,
    unpack_buffer: c_int,
    alignment: c_int,
    row_length: c_int,
    skip_rows: c_int,
    skip_pixels: c_int,
    scissor: bool,
    srgb: bool,
}

impl Saved {
    unsafe fn capture(gl: &Gl, es: bool) -> Self {
        // SAFETY: plain state queries on the current context.
        unsafe {
            Self {
                read_fb: gl.int(GL_READ_FRAMEBUFFER_BINDING),
                draw_fb: gl.int(GL_DRAW_FRAMEBUFFER_BINDING),
                tex: gl.int(GL_TEXTURE_BINDING_2D),
                unpack_buffer: gl.int(GL_PIXEL_UNPACK_BUFFER_BINDING),
                alignment: gl.int(GL_UNPACK_ALIGNMENT),
                row_length: gl.int(GL_UNPACK_ROW_LENGTH),
                skip_rows: gl.int(GL_UNPACK_SKIP_ROWS),
                skip_pixels: gl.int(GL_UNPACK_SKIP_PIXELS),
                scissor: (gl.is_enabled)(GL_SCISSOR_TEST) != 0,
                srgb: !es && (gl.is_enabled)(GL_FRAMEBUFFER_SRGB) != 0,
            }
        }
    }

    unsafe fn restore(&self, gl: &Gl, es: bool) {
        // SAFETY: state calls on the current context with values it reported.
        unsafe {
            (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, self.read_fb as c_uint);
            (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.draw_fb as c_uint);
            (gl.bind_texture)(GL_TEXTURE_2D, self.tex as c_uint);
            (gl.pixel_storei)(GL_UNPACK_ALIGNMENT, self.alignment);
            (gl.pixel_storei)(GL_UNPACK_ROW_LENGTH, self.row_length);
            (gl.pixel_storei)(GL_UNPACK_SKIP_ROWS, self.skip_rows);
            (gl.pixel_storei)(GL_UNPACK_SKIP_PIXELS, self.skip_pixels);
            if self.unpack_buffer != 0 {
                (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, self.unpack_buffer as c_uint);
            }
            if self.scissor {
                (gl.enable)(GL_SCISSOR_TEST);
            }
            if !es && self.srgb {
                (gl.enable)(GL_FRAMEBUFFER_SRGB);
            }
        }
    }
}

/// Draws the visible cards into the default framebuffer of the current context. `false` when
/// nothing was drawn (no cards, unsupported frame, broken context).
unsafe fn draw_frame(gl: &Gl, link: &Arc<Link>, ctx: usize, width: u32, height: u32) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    let (view, alive) = link.view();
    let panel = link.panel_open();
    let mut map = contexts().lock().unwrap_or_else(PoisonError::into_inner);
    let res = map.entry(ctx).or_insert_with(|| Res {
        es: false,
        ..Res::default()
    });
    if res.broken {
        return false;
    }
    let key = Key {
        view: Arc::as_ptr(&view) as usize,
        alive,
        panel,
        width,
        height,
    };
    if res.key.as_ref() != Some(&key) {
        res.regions = draw::compose(&view, &key.alive, panel, width, height);
        res.key = Some(key);
        res.uploaded = false;
    }
    if res.regions.is_empty() {
        return false;
    }
    if res.tex == 0 && res.fbo == 0 {
        // SAFETY: a version query on the current context.
        let v = unsafe { (gl.get_string)(GL_VERSION) };
        // SAFETY: a non-null result is a NUL-terminated string owned by the driver.
        res.es = !v.is_null()
            && unsafe { CStr::from_ptr(v) }
                .to_bytes()
                .starts_with(b"OpenGL ES");
    }
    // SAFETY: state queries on the current context.
    let saved = unsafe { Saved::capture(gl, res.es) };
    // SAFETY: GL calls on the current context, with state restored below.
    let ok = unsafe { copy_cards(gl, res, height) };
    // SAFETY: restoring what `capture` read.
    unsafe { saved.restore(gl, res.es) };
    if !ok {
        res.broken = true;
    }
    ok
}

unsafe fn copy_cards(gl: &Gl, res: &mut Res, height: u32) -> bool {
    let atlas_w = res.regions.iter().map(|r| r.width).max().unwrap_or(0);
    let atlas_h: u32 = res.regions.iter().map(|r| r.height).sum();
    let (Ok(aw), Ok(ah)) = (c_int::try_from(atlas_w), c_int::try_from(atlas_h)) else {
        return false;
    };
    // SAFETY: GL calls on the current context; every object used was created here on it.
    unsafe {
        // Objects of a destroyed context whose address was reused do not exist here.
        if res.tex != 0 && (gl.is_texture)(res.tex) == 0 {
            res.tex = 0;
            res.tex_w = 0;
            res.tex_h = 0;
            res.uploaded = false;
        }
        if res.fbo != 0 && (gl.is_framebuffer)(res.fbo) == 0 {
            res.fbo = 0;
        }
        if res.tex == 0 {
            (gl.gen_textures)(1, &mut res.tex);
            res.tex_w = 0;
            res.tex_h = 0;
        }
        (gl.bind_texture)(GL_TEXTURE_2D, res.tex);
        if res.tex_w < atlas_w || res.tex_h < atlas_h {
            (gl.tex_image_2d)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA8,
                aw,
                ah,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
            res.tex_w = atlas_w;
            res.tex_h = atlas_h;
            res.uploaded = false;
            // The storage was replaced: the framebuffer must be attached again.
            res.fbo = 0;
        }
        if res.fbo == 0 {
            (gl.gen_framebuffers)(1, &mut res.fbo);
            (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, res.fbo);
            (gl.framebuffer_texture_2d)(
                GL_READ_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                res.tex,
                0,
            );
            if (gl.check_framebuffer_status)(GL_READ_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE {
                return false;
            }
        }
        if !res.uploaded {
            (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, 0);
            (gl.pixel_storei)(GL_UNPACK_ALIGNMENT, 4);
            (gl.pixel_storei)(GL_UNPACK_ROW_LENGTH, 0);
            (gl.pixel_storei)(GL_UNPACK_SKIP_ROWS, 0);
            (gl.pixel_storei)(GL_UNPACK_SKIP_PIXELS, 0);
            let mut y = 0;
            for r in &res.regions {
                // 0xAARRGGBB → bytes R, G, B, A.
                let rgba: Vec<u32> = r
                    .pixels
                    .iter()
                    .map(|p| (p & 0xFF00_FF00) | ((p & 0xFF) << 16) | ((p >> 16) & 0xFF))
                    .collect();
                let (Ok(w), Ok(h)) = (c_int::try_from(r.width), c_int::try_from(r.height)) else {
                    return false;
                };
                (gl.tex_sub_image_2d)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    y,
                    w,
                    h,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    rgba.as_ptr().cast(),
                );
                y += h;
            }
            res.uploaded = true;
        }
        (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, res.fbo);
        (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
        (gl.disable)(GL_SCISSOR_TEST);
        if !res.es {
            (gl.disable)(GL_FRAMEBUFFER_SRGB);
        }
        let frame_h = height as c_int;
        let mut y = 0;
        for r in &res.regions {
            let (w, h) = (r.width as c_int, r.height as c_int);
            let (x, top) = (r.x as c_int, r.y as c_int);
            // GL's origin is the bottom left: the card's top row goes to `frame_h - top`, and
            // the reversed destination range flips the card the right way up.
            (gl.blit_framebuffer)(
                0,
                y,
                w,
                y + h,
                x,
                frame_h - top,
                x + w,
                frame_h - top - h,
                GL_COLOR_BUFFER_BIT,
                GL_NEAREST,
            );
            y += h;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swizzle_turns_argb_words_into_rgba_bytes() {
        let p: u32 = 0xFF11_2233;
        let w = (p & 0xFF00_FF00) | ((p & 0xFF) << 16) | ((p >> 16) & 0xFF);
        assert_eq!(w.to_le_bytes(), [0x11, 0x22, 0x33, 0xFF]);
    }

    #[test]
    fn hooked_names_are_substituted_and_the_real_function_is_remembered() {
        let real = 0x1000usize as *mut c_void;
        let ours = substitute(c"eglSwapBuffersWithDamageEXT".as_ptr(), real);
        assert_eq!(ours, eglSwapBuffersWithDamageEXT as *mut c_void);
        assert_eq!(REAL_EGL_SWAP_EXT.load(Ordering::Relaxed), real);
        // Unknown names and missing symbols pass through.
        assert_eq!(substitute(c"glClear".as_ptr(), real), real);
        assert!(substitute(c"eglSwapBuffers".as_ptr(), ptr::null_mut()).is_null());
        assert!(substitute(ptr::null(), real) == real);
        REAL_EGL_SWAP_EXT.store(ptr::null_mut(), Ordering::Relaxed);
    }

    #[test]
    fn the_budget_turns_the_hooks_off_only_on_a_slow_average() {
        // A fast window leaves the overlay on.
        for _ in 0..BUDGET_WINDOW {
            record(Duration::from_micros(50));
        }
        assert!(!guard::is_disabled());
    }
}
