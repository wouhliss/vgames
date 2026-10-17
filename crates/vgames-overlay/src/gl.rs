//! Linux OpenGL hooks (05-social §6.1, GAME-08), loaded with `LD_PRELOAD` into native Linux
//! games (Proton games are covered by the Vulkan layer). On `glXSwapBuffers` /
//! `eglSwapBuffers`, when something is visible, the cards from [`crate::draw`] are uploaded
//! to textures and copied onto the frame being swapped with `glBlitFramebuffer`; every piece
//! of GL state touched is saved first and restored after. No shader, program, vertex state or
//! game object is used.
//!
//! How the hooks are reached (MangoHud, MIT, does the same):
//! - games that link libGL/libEGL call our exported `glXSwapBuffers`/`eglSwapBuffers` and
//!   `*GetProcAddress*` first (preload order);
//! - games that `dlopen` GL and look entry points up by handle (SDL and most engines) go
//!   through our `dlsym` (x86_64): a naked trampoline that hands out the hooks for those five
//!   names on a real handle and otherwise tail-jumps to glibc's `dlsym`, so glibc still sees
//!   the game's own return address (`RTLD_NEXT` keeps meaning "after the caller").
//!
//! Rules: one atomic load per swap while hidden; no allocation unless visible; a panic or a
//! frame over [`FRAME_BUDGET`] on average turns the overlay off (the first frame in a window
//! that creates our GL objects is a warm-up, left out of the average); the GL hooks stand down for
//! good once the Vulkan layer draws in this process ([`guard::vulkan_draws`]). Only GL 3.0+
//! and OpenGL ES 3.0+ contexts are drawn into; older ones are left alone.
//!
//! Everything here is FFI with the dynamic linker and the GL driver, hence the module-wide
//! `unsafe_code` allowance (granted in `lib.rs`); each `unsafe` block states what it relies on.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::draw::{self, Region};
use crate::guard;
use crate::link::{self, Link};
use crate::protocol::RendererKind;

/// Drawing may take this long per frame on average before the overlay turns itself off.
pub const FRAME_BUDGET: Duration = Duration::from_millis(1);
const BUDGET_WINDOW: u32 = 120;
static DRAWN_FRAMES: AtomicU64 = AtomicU64::new(0);
static DRAW_NANOS: AtomicU64 = AtomicU64::new(0);

/// Frames the GL hooks drew into so far, and the CPU time they spent (for benchmarks).
///
/// # Safety
/// Both pointers must be valid for a `u64` write (null pointers are skipped).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vgames_overlay_gl_draw_stats(frames: *mut u64, nanos: *mut u64) {
    // SAFETY: the caller passes writable pointers or null.
    unsafe {
        if let Some(p) = frames.as_mut() {
            *p = DRAWN_FRAMES.load(Ordering::Relaxed);
        }
        if let Some(p) = nanos.as_mut() {
            *p = DRAW_NANOS.load(Ordering::Relaxed);
        }
    }
}

// ---- the real entry points -------------------------------------------------------------------

type DlsymFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type GlxSwapFn = unsafe extern "C" fn(*mut c_void, c_ulong);
type EglSwapFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
type GetProcFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;

/// glibc's `dlsym` (read by the trampoline, which jumps to it).
static REAL_DLSYM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[derive(Clone, Copy)]
enum Hooked {
    GlxSwap,
    EglSwap,
    GlxGetProc,
    GlxGetProcArb,
    EglGetProc,
}

const HOOKED: [(&[u8], Hooked); 5] = [
    (b"glXSwapBuffers", Hooked::GlxSwap),
    (b"eglSwapBuffers", Hooked::EglSwap),
    (b"glXGetProcAddress", Hooked::GlxGetProc),
    (b"glXGetProcAddressARB", Hooked::GlxGetProcArb),
    (b"eglGetProcAddress", Hooked::EglGetProc),
];

/// The real function behind each hook (first one found wins).
static REAL_GLX_SWAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_EGL_SWAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_GLX_GET_PROC: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_GLX_GET_PROC_ARB: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_EGL_GET_PROC: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn slot(h: Hooked) -> &'static AtomicPtr<c_void> {
    match h {
        Hooked::GlxSwap => &REAL_GLX_SWAP,
        Hooked::EglSwap => &REAL_EGL_SWAP,
        Hooked::GlxGetProc => &REAL_GLX_GET_PROC,
        Hooked::GlxGetProcArb => &REAL_GLX_GET_PROC_ARB,
        Hooked::EglGetProc => &REAL_EGL_GET_PROC,
    }
}

fn hooked(name: &[u8]) -> Option<Hooked> {
    HOOKED.iter().find(|(n, _)| *n == name).map(|(_, h)| *h)
}

fn ours(h: Hooked) -> *mut c_void {
    match h {
        Hooked::GlxSwap => glXSwapBuffers as *mut c_void,
        Hooked::EglSwap => eglSwapBuffers as *mut c_void,
        Hooked::GlxGetProc => glXGetProcAddress as *mut c_void,
        Hooked::GlxGetProcArb => glXGetProcAddressARB as *mut c_void,
        Hooked::EglGetProc => eglGetProcAddress as *mut c_void,
    }
}

fn name_of(h: Hooked) -> &'static CStr {
    match h {
        Hooked::GlxSwap => c"glXSwapBuffers",
        Hooked::EglSwap => c"eglSwapBuffers",
        Hooked::GlxGetProc => c"glXGetProcAddress",
        Hooked::GlxGetProcArb => c"glXGetProcAddressARB",
        Hooked::EglGetProc => c"eglGetProcAddress",
    }
}

fn real_dlsym() -> Option<DlsymFn> {
    let mut p = REAL_DLSYM.load(Ordering::Acquire);
    if p.is_null() {
        for version in [c"GLIBC_2.34", c"GLIBC_2.2.5", c"GLIBC_2.17"] {
            // SAFETY: `dlvsym` is not interposed by us; both strings are NUL-terminated.
            p = unsafe { libc::dlvsym(libc::RTLD_NEXT, c"dlsym".as_ptr(), version.as_ptr()) };
            if !p.is_null() {
                break;
            }
        }
        if p.is_null() {
            return None;
        }
        REAL_DLSYM.store(p, Ordering::Release);
    }
    // SAFETY: `p` is glibc's `dlsym`, which has this signature.
    Some(unsafe { std::mem::transmute::<*mut c_void, DlsymFn>(p) })
}

/// The real entry point for `h`: recorded from the game's own lookup, else the next
/// definition after this library, else the already loaded GL libraries.
fn real(h: Hooked) -> *mut c_void {
    let slot = slot(h);
    let p = slot.load(Ordering::Acquire);
    if !p.is_null() {
        return p;
    }
    let Some(dlsym) = real_dlsym() else {
        return std::ptr::null_mut();
    };
    let name = name_of(h);
    // SAFETY: real `dlsym` with a pseudo-handle and a NUL-terminated name.
    let mut found = unsafe { dlsym(libc::RTLD_NEXT, name.as_ptr()) };
    if found.is_null() || found == ours(h) {
        found = std::ptr::null_mut();
        let libs: &[&CStr] = match h {
            Hooked::EglSwap | Hooked::EglGetProc => &[c"libEGL.so.1"],
            _ => &[c"libGLX.so.0", c"libGL.so.1"],
        };
        for lib in libs {
            // SAFETY: RTLD_NOLOAD only returns a library that is already loaded.
            let handle = unsafe { libc::dlopen(lib.as_ptr(), libc::RTLD_NOLOAD | libc::RTLD_LAZY) };
            if handle.is_null() {
                continue;
            }
            // SAFETY: a live handle and a NUL-terminated name.
            let p = unsafe { dlsym(handle, name.as_ptr()) };
            if !p.is_null() && p != ours(h) {
                found = p;
                break;
            }
        }
    }
    if !found.is_null() {
        slot.store(found, Ordering::Release);
    }
    found
}

/// Called by the `dlsym` trampoline: our hook for the five GL names looked up on a real
/// handle, else null (the trampoline then jumps to glibc's `dlsym`).
extern "C" fn dlsym_hook(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    let Some(dlsym) = real_dlsym() else {
        return std::ptr::null_mut();
    };
    if name.is_null() || handle.is_null() || handle == libc::RTLD_NEXT || guard::is_disabled() {
        return std::ptr::null_mut();
    }
    // SAFETY: `dlsym` callers pass a NUL-terminated name.
    let Some(h) = hooked(unsafe { CStr::from_ptr(name) }.to_bytes()) else {
        return std::ptr::null_mut();
    };
    // SAFETY: real `dlsym` on the caller's handle (not a pseudo-handle: no caller dependence).
    let found = unsafe { dlsym(handle, name) };
    if found.is_null() || found == ours(h) {
        return std::ptr::null_mut();
    }
    let _ = slot(h).compare_exchange(
        std::ptr::null_mut(),
        found,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
    ours(h)
}

/// `dlsym`, interposed: see the module docs. Tail-jumps to glibc so that `RTLD_NEXT` lookups
/// resolve relative to the real caller.
///
/// # Safety
/// The C `dlsym` contract.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    // Entry: rsp ≡ 8 (mod 16). Three pushes align it for the call.
    core::arch::naked_asm!(
        "push rdi",
        "push rsi",
        "push rdi",
        "call {hook}",
        "pop rdi",
        "pop rsi",
        "pop rdi",
        "test rax, rax",
        "jz 2f",
        "ret",
        "2:",
        "mov rax, qword ptr [rip + {real}]",
        "test rax, rax",
        "jz 3f",
        "jmp rax",
        "3:",
        "xor eax, eax",
        "ret",
        hook = sym dlsym_hook,
        real = sym REAL_DLSYM,
    )
}

// ---- exported hooks ---------------------------------------------------------------------------

/// Whether the GL hooks draw at all: not once the overlay is off or the Vulkan layer draws.
#[inline]
fn wants_frames() -> bool {
    !guard::is_disabled() && !guard::vulkan_draws()
}

/// # Safety
/// The GLX contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXSwapBuffers(dpy: *mut c_void, drawable: c_ulong) {
    let real = real(Hooked::GlxSwap);
    if wants_frames() {
        guard::guarded(|| draw_before_swap(Api::Glx, dpy, drawable as usize));
    }
    if !real.is_null() {
        // SAFETY: the real glXSwapBuffers.
        unsafe { std::mem::transmute::<*mut c_void, GlxSwapFn>(real)(dpy, drawable) }
    }
}

/// # Safety
/// The EGL contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffers(dpy: *mut c_void, surface: *mut c_void) -> c_uint {
    let real = real(Hooked::EglSwap);
    if wants_frames() {
        guard::guarded(|| draw_before_swap(Api::Egl, dpy, surface as usize));
    }
    if real.is_null() {
        return 0;
    }
    // SAFETY: the real eglSwapBuffers.
    unsafe { std::mem::transmute::<*mut c_void, EglSwapFn>(real)(dpy, surface) }
}

fn get_proc(h: Hooked, name: *const c_char) -> *mut c_void {
    let real = real(h);
    if !name.is_null() && !guard::is_disabled() {
        // SAFETY: GetProcAddress callers pass a NUL-terminated name.
        let wanted = hooked(unsafe { CStr::from_ptr(name) }.to_bytes());
        if let Some(swap @ (Hooked::GlxSwap | Hooked::EglSwap)) = wanted
            && !real.is_null()
        {
            // Record the real swap from the same loader, then hand out ours.
            // SAFETY: the real GetProcAddress with the caller's name.
            let p = unsafe { std::mem::transmute::<*mut c_void, GetProcFn>(real)(name) };
            if !p.is_null() && p != ours(swap) {
                let _ = slot(swap).compare_exchange(
                    std::ptr::null_mut(),
                    p,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                return ours(swap);
            }
        }
    }
    if real.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the real GetProcAddress.
    unsafe { std::mem::transmute::<*mut c_void, GetProcFn>(real)(name) }
}

/// # Safety
/// The GLX contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddress(name: *const c_char) -> *mut c_void {
    get_proc(Hooked::GlxGetProc, name)
}

/// # Safety
/// The GLX contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddressARB(name: *const c_char) -> *mut c_void {
    get_proc(Hooked::GlxGetProcArb, name)
}

/// # Safety
/// The EGL contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetProcAddress(name: *const c_char) -> *mut c_void {
    get_proc(Hooked::EglGetProc, name)
}

// ---- drawing ----------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Api {
    Glx,
    Egl,
}

type GetIntegerv = unsafe extern "C" fn(c_uint, *mut c_int);
type GetString = unsafe extern "C" fn(c_uint) -> *const u8;
type IsEnabled = unsafe extern "C" fn(c_uint) -> u8;
type Cap = unsafe extern "C" fn(c_uint);
type Gen = unsafe extern "C" fn(c_int, *mut c_uint);
type Bind = unsafe extern "C" fn(c_uint, c_uint);
type IsName = unsafe extern "C" fn(c_uint) -> u8;
type PixelStorei = unsafe extern "C" fn(c_uint, c_int);
type TexParameteri = unsafe extern "C" fn(c_uint, c_uint, c_int);
type TexImage2D =
    unsafe extern "C" fn(c_uint, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint, *const c_void);
type FramebufferTexture2D = unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint, c_int);
type BlitFramebuffer =
    unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_int, c_int, c_int, c_int, c_uint, c_uint);
type CurrentPtr = unsafe extern "C" fn() -> *mut c_void;
type CurrentDrawable = unsafe extern "C" fn() -> c_ulong;
type QueryDrawable = unsafe extern "C" fn(*mut c_void, c_ulong, c_int, *mut c_uint) -> c_int;
type EglCurrentSurface = unsafe extern "C" fn(c_int) -> *mut c_void;
type EglQuerySurface = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint;

/// The GL and window-system functions the drawing needs, from one GetProcAddress.
struct Gl {
    get_integerv: GetIntegerv,
    get_string: GetString,
    is_enabled: IsEnabled,
    enable: Cap,
    disable: Cap,
    gen_textures: Gen,
    gen_framebuffers: Gen,
    bind_texture: Bind,
    bind_framebuffer: Bind,
    bind_buffer: Bind,
    is_texture: IsName,
    is_framebuffer: IsName,
    pixel_storei: PixelStorei,
    tex_parameteri: TexParameteri,
    tex_image_2d: TexImage2D,
    framebuffer_texture_2d: FramebufferTexture2D,
    blit_framebuffer: BlitFramebuffer,
    current_context: CurrentPtr,
    size: Size,
}

enum Size {
    Glx {
        current_drawable: CurrentDrawable,
        query_drawable: QueryDrawable,
    },
    Egl {
        current_surface: EglCurrentSurface,
        query_surface: EglQuerySurface,
    },
}

const GL_VERSION: c_uint = 0x1F02;
const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_TEXTURE_BINDING_2D: c_uint = 0x8069;
const GL_READ_FRAMEBUFFER: c_uint = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: c_uint = 0x8CA9;
const GL_READ_FRAMEBUFFER_BINDING: c_uint = 0x8CAA;
const GL_DRAW_FRAMEBUFFER_BINDING: c_uint = 0x8CA6;
const GL_PIXEL_UNPACK_BUFFER: c_uint = 0x88EC;
const GL_PIXEL_UNPACK_BUFFER_BINDING: c_uint = 0x88EF;
const GL_UNPACK_ALIGNMENT: c_uint = 0x0CF5;
const GL_UNPACK_ROW_LENGTH: c_uint = 0x0CF2;
const GL_UNPACK_SKIP_ROWS: c_uint = 0x0CF3;
const GL_UNPACK_SKIP_PIXELS: c_uint = 0x0CF4;
const GL_SCISSOR_TEST: c_uint = 0x0C11;
const GL_FRAMEBUFFER_SRGB: c_uint = 0x8DB9;
const GL_COLOR_ATTACHMENT0: c_uint = 0x8CE0;
const GL_RGBA: c_uint = 0x1908;
const GL_RGBA8: c_int = 0x8058;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_NEAREST: c_uint = 0x2600;
const GL_COLOR_BUFFER_BIT: c_uint = 0x4000;
const GLX_WIDTH: c_int = 0x801D;
const GLX_HEIGHT: c_int = 0x801E;
const EGL_DRAW: c_int = 0x3059;
const EGL_WIDTH: c_int = 0x3057;
const EGL_HEIGHT: c_int = 0x3056;

impl Gl {
    fn load(api: Api) -> Option<Self> {
        let get_proc = real(match api {
            Api::Glx => Hooked::GlxGetProcArb,
            Api::Egl => Hooked::EglGetProc,
        });
        if get_proc.is_null() {
            return None;
        }
        // SAFETY: the real GetProcAddress.
        let gpa = unsafe { std::mem::transmute::<*mut c_void, GetProcFn>(get_proc) };
        let f = |name: &CStr| -> Option<*mut c_void> {
            // SAFETY: a NUL-terminated name.
            let p = unsafe { gpa(name.as_ptr()) };
            (!p.is_null()).then_some(p)
        };
        macro_rules! load {
            ($name:literal as $t:ty) => {
                // SAFETY: GetProcAddress returns the entry point named, which has the GL
                // signature `$t` mirrors.
                unsafe { std::mem::transmute::<*mut c_void, $t>(f($name)?) }
            };
        }
        let size = match api {
            Api::Glx => Size::Glx {
                current_drawable: load!(c"glXGetCurrentDrawable" as CurrentDrawable),
                query_drawable: load!(c"glXQueryDrawable" as QueryDrawable),
            },
            Api::Egl => Size::Egl {
                current_surface: load!(c"eglGetCurrentSurface" as EglCurrentSurface),
                query_surface: load!(c"eglQuerySurface" as EglQuerySurface),
            },
        };
        Some(Self {
            get_integerv: load!(c"glGetIntegerv" as GetIntegerv),
            get_string: load!(c"glGetString" as GetString),
            is_enabled: load!(c"glIsEnabled" as IsEnabled),
            enable: load!(c"glEnable" as Cap),
            disable: load!(c"glDisable" as Cap),
            gen_textures: load!(c"glGenTextures" as Gen),
            gen_framebuffers: load!(c"glGenFramebuffers" as Gen),
            bind_texture: load!(c"glBindTexture" as Bind),
            bind_framebuffer: load!(c"glBindFramebuffer" as Bind),
            bind_buffer: load!(c"glBindBuffer" as Bind),
            is_texture: load!(c"glIsTexture" as IsName),
            is_framebuffer: load!(c"glIsFramebuffer" as IsName),
            pixel_storei: load!(c"glPixelStorei" as PixelStorei),
            tex_parameteri: load!(c"glTexParameteri" as TexParameteri),
            tex_image_2d: load!(c"glTexImage2D" as TexImage2D),
            framebuffer_texture_2d: load!(c"glFramebufferTexture2D" as FramebufferTexture2D),
            blit_framebuffer: load!(c"glBlitFramebuffer" as BlitFramebuffer),
            current_context: match api {
                Api::Glx => load!(c"glXGetCurrentContext" as CurrentPtr),
                Api::Egl => load!(c"eglGetCurrentContext" as CurrentPtr),
            },
            size,
        })
    }

    fn int(&self, pname: c_uint) -> c_int {
        let mut v = 0;
        // SAFETY: one-value query into a local.
        unsafe { (self.get_integerv)(pname, &mut v) };
        v
    }

    /// The size of the surface being swapped, if it is the current draw surface.
    fn frame_size(&self, dpy: *mut c_void, target: usize) -> Option<(u32, u32)> {
        match self.size {
            Size::Glx {
                current_drawable,
                query_drawable,
            } => {
                // SAFETY: GLX queries on the display and drawable the game is swapping.
                unsafe {
                    if current_drawable() as usize != target {
                        return None;
                    }
                    let (mut w, mut h) = (0, 0);
                    query_drawable(dpy, target as c_ulong, GLX_WIDTH, &mut w);
                    query_drawable(dpy, target as c_ulong, GLX_HEIGHT, &mut h);
                    Some((w, h))
                }
            }
            Size::Egl {
                current_surface,
                query_surface,
            } => {
                // SAFETY: EGL queries on the display and surface the game is swapping.
                unsafe {
                    if current_surface(EGL_DRAW) as usize != target {
                        return None;
                    }
                    let (mut w, mut h) = (0, 0);
                    if query_surface(dpy, target as *mut c_void, EGL_WIDTH, &mut w) == 0
                        || query_surface(dpy, target as *mut c_void, EGL_HEIGHT, &mut h) == 0
                    {
                        return None;
                    }
                    Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?))
                }
            }
        }
    }

    /// GL 3.0+ or ES 3.0+ (`glBlitFramebuffer`, sized RGBA8); `(ok, es)`.
    fn version(&self) -> (bool, bool) {
        // SAFETY: a static string owned by the driver, or null.
        let p = unsafe { (self.get_string)(GL_VERSION) };
        if p.is_null() {
            return (false, false);
        }
        // SAFETY: GL returns a NUL-terminated string.
        let v = unsafe { CStr::from_ptr(p.cast()) }.to_bytes();
        parse_version(v)
    }
}

/// `(drawable, es)` from a `GL_VERSION` string.
fn parse_version(v: &[u8]) -> (bool, bool) {
    let es = v.starts_with(b"OpenGL ES");
    let digits = if es {
        v.iter()
            .position(u8::is_ascii_digit)
            .and_then(|i| v.get(i..))
    } else {
        Some(v)
    };
    let major = digits
        .and_then(|d| d.first())
        .filter(|c| c.is_ascii_digit())
        .map_or(0, |c| c - b'0');
    (major >= 3, es)
}

/// One context's textures and what they hold.
#[derive(Default)]
struct Context {
    checked: bool,
    usable: bool,
    es: bool,
    key: Option<Key>,
    regions: Vec<Region>,
    /// (texture, framebuffer) per region.
    targets: Vec<(c_uint, c_uint)>,
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

#[derive(Default)]
struct State {
    contexts: HashMap<usize, Context>,
    budget: Budget,
}

/// The safety valve: the average draw time over windows of [`BUDGET_WINDOW`] drawn frames.
///
/// The first frame in a window that creates our textures and framebuffers is a warm-up and
/// is left out (neither a frame nor time): drivers build their blit path on first use
/// (llvmpipe compiles it, 20-140 ms measured locally, over 370 ms and then over 1.1 s on
/// loaded hosted runners), and that one-off alone would average a window over budget and
/// turn the overlay off for the whole process. By the time it is measured the stall has
/// happened; turning the overlay off would not undo it, and the valve is for the steady cost.
/// Further builds in the same window count in full, so a context rebuilt every frame still
/// reaches the guard.
#[derive(Default)]
struct Budget {
    frames: u32,
    spent: Duration,
    warmed_up: bool,
}

impl Budget {
    /// Adds one drawn frame (`built`: it created our GL objects); `false` once a window
    /// averaged over [`FRAME_BUDGET`].
    fn record(&mut self, took: Duration, built: bool) -> bool {
        if built && !self.warmed_up {
            self.warmed_up = true;
            return true;
        }
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

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn gl(api: Api) -> Option<&'static Gl> {
    static GLX: OnceLock<Option<Gl>> = OnceLock::new();
    static EGL: OnceLock<Option<Gl>> = OnceLock::new();
    match api {
        Api::Glx => GLX.get_or_init(|| Gl::load(Api::Glx)),
        Api::Egl => EGL.get_or_init(|| Gl::load(Api::Egl)),
    }
    .as_ref()
}

fn draw_before_swap(api: Api, dpy: *mut c_void, target: usize) {
    let Some(link) = link::shared(RendererKind::OpenGl) else {
        return;
    };
    if !link.should_draw() {
        return;
    }
    let Some(gl) = gl(api) else {
        return;
    };
    let started = Instant::now();
    // SAFETY: reads the calling thread's current context.
    let ctx = unsafe { (gl.current_context)() } as usize;
    if ctx == 0 {
        return;
    }
    let Some((width, height)) = gl.frame_size(dpy, target) else {
        return;
    };
    let mut st = state().lock().unwrap_or_else(PoisonError::into_inner);
    let c = st.contexts.entry(ctx).or_default();
    // A context destroyed and another created at the same address: none of our names are
    // valid in it, and it may be another GL version. Start over.
    if stale(gl, c) {
        *c = Context::default();
    }
    if !c.checked {
        (c.usable, c.es) = gl.version();
        c.checked = true;
    }
    if !c.usable {
        return;
    }
    if let Some(built) = draw_cards(gl, link, c, width, height) {
        let took = started.elapsed();
        DRAWN_FRAMES.fetch_add(1, Ordering::Relaxed);
        DRAW_NANOS.fetch_add(
            u64::try_from(took.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if !st.budget.record(took, built) {
            guard::disable();
        }
    }
}

/// Whether `c` holds names that are not textures and framebuffers in the current context.
fn stale(gl: &Gl, c: &Context) -> bool {
    // SAFETY: name queries on the current context (any name may be passed).
    c.targets
        .iter()
        .any(|&(t, f)| unsafe { (gl.is_texture)(t) == 0 || (gl.is_framebuffer)(f) == 0 })
}

/// Recomposes when needed, uploads changed cards and blits them. `Some(built)` if anything
/// was drawn, `built` when this frame created textures and framebuffers.
fn draw_cards(gl: &Gl, link: &Link, c: &mut Context, width: u32, height: u32) -> Option<bool> {
    let (view, alive) = link.view();
    let key = Key {
        view: std::sync::Arc::as_ptr(&view) as usize,
        alive,
        panel: link.panel_open(),
        width,
        height,
    };
    if c.key.as_ref() != Some(&key) {
        c.regions = draw::compose(&view, &key.alive, key.panel, width, height);
        c.key = Some(key);
        c.uploaded = false;
    }
    if c.regions.is_empty() {
        return None;
    }
    let frame_h = c_int::try_from(height).ok()?;

    // Save what we touch.
    let read_fb = gl.int(GL_READ_FRAMEBUFFER_BINDING);
    let draw_fb = gl.int(GL_DRAW_FRAMEBUFFER_BINDING);
    let texture = gl.int(GL_TEXTURE_BINDING_2D);
    let unpack_buffer = gl.int(GL_PIXEL_UNPACK_BUFFER_BINDING);
    let unpack = [
        (GL_UNPACK_ALIGNMENT, gl.int(GL_UNPACK_ALIGNMENT)),
        (GL_UNPACK_ROW_LENGTH, gl.int(GL_UNPACK_ROW_LENGTH)),
        (GL_UNPACK_SKIP_ROWS, gl.int(GL_UNPACK_SKIP_ROWS)),
        (GL_UNPACK_SKIP_PIXELS, gl.int(GL_UNPACK_SKIP_PIXELS)),
    ];
    let built = c.targets.len() < c.regions.len();
    // SAFETY (this block and the restore below): GL calls on the current context with
    // names this module created in it or values just read from it.
    unsafe {
        let scissor = (gl.is_enabled)(GL_SCISSOR_TEST) != 0;
        let srgb = !c.es && (gl.is_enabled)(GL_FRAMEBUFFER_SRGB) != 0;

        if built {
            while c.targets.len() < c.regions.len() {
                let (mut t, mut f) = (0, 0);
                (gl.gen_textures)(1, &mut t);
                (gl.gen_framebuffers)(1, &mut f);
                c.targets.push((t, f));
            }
            c.uploaded = false;
        }
        if !c.uploaded {
            (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, 0);
            for (name, _) in unpack {
                (gl.pixel_storei)(name, if name == GL_UNPACK_ALIGNMENT { 4 } else { 0 });
            }
            for (region, &(t, f)) in c.regions.iter().zip(&c.targets) {
                let rgba = to_rgba(region);
                (gl.bind_texture)(GL_TEXTURE_2D, t);
                (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST as c_int);
                (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST as c_int);
                (gl.tex_image_2d)(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA8,
                    region.width as c_int,
                    region.height as c_int,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    rgba.as_ptr().cast(),
                );
                (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, f);
                (gl.framebuffer_texture_2d)(
                    GL_READ_FRAMEBUFFER,
                    GL_COLOR_ATTACHMENT0,
                    GL_TEXTURE_2D,
                    t,
                    0,
                );
            }
            (gl.bind_texture)(GL_TEXTURE_2D, texture as c_uint);
            for (name, value) in unpack {
                (gl.pixel_storei)(name, value);
            }
            (gl.bind_buffer)(GL_PIXEL_UNPACK_BUFFER, unpack_buffer as c_uint);
            c.uploaded = true;
        }

        if scissor {
            (gl.disable)(GL_SCISSOR_TEST);
        }
        if srgb {
            (gl.disable)(GL_FRAMEBUFFER_SRGB);
        }
        (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
        for (region, &(_, f)) in c.regions.iter().zip(&c.targets) {
            let (x, y, w, h) = (
                region.x as c_int,
                region.y as c_int,
                region.width as c_int,
                region.height as c_int,
            );
            (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, f);
            // Rows were uploaded top first: flip onto GL's bottom-up frame.
            (gl.blit_framebuffer)(
                0,
                0,
                w,
                h,
                x,
                frame_h - y,
                x + w,
                frame_h - y - h,
                GL_COLOR_BUFFER_BIT,
                GL_NEAREST,
            );
        }
        (gl.bind_framebuffer)(GL_READ_FRAMEBUFFER, read_fb as c_uint);
        (gl.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, draw_fb as c_uint);
        if srgb {
            (gl.enable)(GL_FRAMEBUFFER_SRGB);
        }
        if scissor {
            (gl.enable)(GL_SCISSOR_TEST);
        }
    }
    Some(built)
}

/// `0xAARRGGBB` pixels as RGBA bytes, rows top first.
fn to_rgba(region: &Region) -> Vec<u8> {
    region
        .pixels
        .iter()
        .flat_map(|p| {
            let [b, g, r, _] = p.to_le_bytes();
            [r, g, b, 0xFF]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gl_hooks_stand_down_once_the_vulkan_layer_draws() {
        let before = wants_frames() || guard::is_disabled();
        assert!(
            before,
            "on at start unless another test turned the overlay off"
        );
        guard::note_vulkan_draws();
        assert!(!wants_frames());
    }

    #[test]
    fn only_gl3_and_es3_contexts_are_drawn_into() {
        assert_eq!(
            parse_version(b"4.6 (Compatibility Profile) Mesa 24.0"),
            (true, false)
        );
        assert_eq!(parse_version(b"3.0 Mesa"), (true, false));
        assert_eq!(parse_version(b"2.1 Mesa 24.0"), (false, false));
        assert_eq!(parse_version(b"OpenGL ES 3.2 Mesa 24.0"), (true, true));
        assert_eq!(parse_version(b"OpenGL ES 2.0 Mesa"), (false, true));
        assert_eq!(parse_version(b""), (false, false));
    }

    #[test]
    fn only_the_five_gl_entry_points_are_hooked() {
        for (name, _) in HOOKED {
            assert!(hooked(name).is_some());
        }
        for other in [
            &b"glXSwapIntervalEXT"[..],
            b"dlsym",
            b"glClear",
            b"eglSwapBuffersWithDamageKHR",
        ] {
            assert!(
                hooked(other).is_none(),
                "{}",
                String::from_utf8_lossy(other)
            );
        }
    }

    const MS: Duration = Duration::from_millis(1);

    /// Records `first` (built or not) then cheap frames to the end of the window (a warm-up
    /// build is not one of its frames).
    fn window(first: Duration, built: bool) -> bool {
        let mut b = Budget::default();
        let mut ok = b.record(first, built);
        let rest = if built {
            BUDGET_WINDOW
        } else {
            BUDGET_WINDOW - 1
        };
        for _ in 0..rest {
            ok = b.record(MS / 5, false);
        }
        ok
    }

    #[test]
    fn a_window_over_budget_turns_the_overlay_off() {
        let mut b = Budget::default();
        let ends: Vec<bool> = (0..BUDGET_WINDOW)
            .map(|_| b.record(2 * MS, false))
            .collect();
        assert!(ends[..ends.len() - 1].iter().all(|ok| *ok));
        assert_eq!(ends.last(), Some(&false));
        // A new window starts clean.
        assert!(b.record(2 * MS, false));
    }

    #[test]
    fn building_our_objects_is_a_warm_up() {
        // 200 ms then 119 × 0.2 ms: 1.86 ms on average if it all counted.
        assert!(window(200 * MS, true));
        // The same stall in a frame that built nothing counts in full.
        assert!(!window(200 * MS, false));
    }

    #[test]
    fn a_warm_up_of_any_length_is_left_out_and_the_window_still_closes() {
        // A hosted runner took over 1.1 s for the first build: it used to count all but 1 s,
        // which alone averaged that window over budget and turned the overlay off.
        assert!(window(1_200 * MS, true));
        assert!(window(10_000 * MS, true));
        // The warm-up is not one of the window's frames: 120 slow frames after it still trip.
        let mut b = Budget::default();
        assert!(b.record(10 * MS, true));
        let ends: Vec<bool> = (0..BUDGET_WINDOW)
            .map(|_| b.record(2 * MS, false))
            .collect();
        assert!(ends[..ends.len() - 1].iter().all(|ok| *ok));
        assert_eq!(ends.last(), Some(&false));
    }

    #[test]
    fn a_slow_first_build_on_a_loaded_machine_is_a_warm_up() {
        // A hosted runner took over 370 ms to build; with 119 cheap frames that window used to
        // average past 1 ms and turned the overlay off for the whole process.
        assert!(window(400 * MS, true));
    }

    #[test]
    fn only_the_first_build_in_a_window_is_a_warm_up() {
        // Two 200 ms builds: the second counts in full, (200 + 23.8) / 120 = 1.86 ms.
        let mut b = Budget::default();
        let mut ok = b.record(200 * MS, true) && b.record(200 * MS, true);
        for _ in 1..BUDGET_WINDOW {
            ok = b.record(MS / 5, false);
        }
        assert!(!ok);
        // The next window gets its own allowance.
        assert!(window(200 * MS, true));
    }

    #[test]
    fn cards_are_uploaded_as_opaque_rgba() {
        let region = Region {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
            pixels: vec![0xFF11_2233, 0x0044_5566],
        };
        assert_eq!(
            to_rgba(&region),
            vec![0x11, 0x22, 0x33, 0xFF, 0x44, 0x55, 0x66, 0xFF]
        );
    }
}
