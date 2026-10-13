//! A native Linux GL "game" for `tests/gl_preload.rs` (GAME-08): clears a 640×480 surface to
//! red every frame and swaps. It loads GL like most engines do, with `dlopen` and lookups by
//! handle, so the preloaded overlay is reached through its `dlsym` (EGL) and
//! `glXGetProcAddressARB` (GLX) hooks. It never links the overlay itself.
//!
//! Phases, each ending with a `cost …` line on stdout that the test's broker acts on:
//! 1. `toast`: swaps until the pixel under the first toast is not red (the overlay drew),
//!    checks a corner far from the cards is still red, then the overlay's own draw time
//!    (`vgames_overlay_gl_draw_stats`) over [`FRAMES`] more frames.
//! 2. `hidden`: once the broker emptied the view and the toast is gone, times the swap call
//!    alone over [`HIDDEN_SWAPS`] frames (median) and checks the overlay drew none of them.
//!    It also times the swap with a `glFinish` (the frame's raster work), for the open phase.
//! 3. `opening` / `open`: once the broker opened the panel, the frames until it shows
//!    (compose and upload), then its draw time over [`FRAMES`] more frames, each timed like
//!    the hidden ones with `glFinish`, and checks the panel is still drawn.
//!
//! `vgames-gl-testapp egl|glx [baseline]`: `baseline` runs phase 2 only, for a run without
//! the overlay to compare with. Exit codes: 0 every phase passed, 2 no GL here (skip),
//! 3 never drawn, 4 drew outside the cards, 5 the toast stayed after the view emptied,
//! 6 drew while nothing was visible, 7 the panel never drawn, 8 the overlay stopped drawing
//! during a phase (its safety valve turned it off).
#![allow(unsafe_code, clippy::print_stdout, clippy::print_stderr)]

use std::ffi::{CStr, c_int, c_uint, c_void};
use std::time::{Duration, Instant};

const W: c_int = 640;
const H: c_int = 480;

/// Frames over which a visible phase's draw time is averaged.
const FRAMES: usize = 120;
/// Swaps timed while hidden.
const HIDDEN_SWAPS: usize = 600;

fn main() {
    let baseline = std::env::args().nth(2).as_deref() == Some("baseline");
    let code = match std::env::args().nth(1).as_deref() {
        #[cfg(target_os = "linux")]
        Some("egl") => linux::run(linux::Api::Egl, baseline),
        #[cfg(target_os = "linux")]
        Some("glx") => linux::run(linux::Api::Glx, baseline),
        _ => {
            eprintln!("usage: vgames-gl-testapp egl|glx [baseline] (Linux only)");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    pub enum Api {
        Egl,
        Glx,
    }

    struct Lib(*mut c_void);

    impl Lib {
        fn open(name: &CStr) -> Option<Self> {
            // SAFETY: a NUL-terminated library name.
            let h = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            (!h.is_null()).then_some(Self(h))
        }

        /// # Safety
        /// `T` must be the C signature of `name`.
        unsafe fn get<T: Copy>(&self, name: &CStr) -> Option<T> {
            // SAFETY: a live handle and a NUL-terminated name.
            let p = unsafe { libc::dlsym(self.0, name.as_ptr()) };
            // SAFETY: the caller states `T` is the function's type (pointer-sized).
            (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
        }
    }

    type GetProc = unsafe extern "C" fn(*const i8) -> *mut c_void;
    type ClearColor = unsafe extern "C" fn(f32, f32, f32, f32);
    type Clear = unsafe extern "C" fn(c_uint);
    type ReadPixels = unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_uint, c_uint, *mut c_void);
    type Finish = unsafe extern "C" fn();

    struct Gl {
        clear_color: ClearColor,
        clear: Clear,
        read_pixels: ReadPixels,
        finish: Finish,
    }

    impl Gl {
        fn load(gpa: GetProc) -> Option<Self> {
            let f = |n: &CStr| {
                // SAFETY: a NUL-terminated name.
                let p = unsafe { gpa(n.as_ptr()) };
                (!p.is_null()).then_some(p)
            };
            // SAFETY: each pointer is the GL function named, with the signature given.
            unsafe {
                Some(Self {
                    clear_color: std::mem::transmute::<*mut c_void, ClearColor>(f(
                        c"glClearColor",
                    )?),
                    clear: std::mem::transmute::<*mut c_void, Clear>(f(c"glClear")?),
                    read_pixels: std::mem::transmute::<*mut c_void, ReadPixels>(f(
                        c"glReadPixels",
                    )?),
                    finish: std::mem::transmute::<*mut c_void, Finish>(f(c"glFinish")?),
                })
            }
        }

        fn pixel(&self, x: c_int, y_from_top: c_int) -> [u8; 4] {
            let mut px = [0u8; 4];
            // SAFETY: a 1×1 RGBA read into a 4-byte buffer on the current context.
            unsafe {
                (self.finish)();
                (self.read_pixels)(
                    x,
                    H - 1 - y_from_top,
                    1,
                    1,
                    0x1908,
                    0x1401,
                    px.as_mut_ptr().cast(),
                );
            }
            px
        }
    }

    const RED: [u8; 4] = [255, 0, 0, 255];

    /// The preloaded overlay's draw counters (`vgames_overlay_gl_draw_stats`): frames drawn
    /// and nanoseconds spent; zeros without the overlay.
    fn stats() -> (u64, u64) {
        type Stats = unsafe extern "C" fn(*mut u64, *mut u64);
        // SAFETY: a global lookup of the overlay's exported counter function.
        let p =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"vgames_overlay_gl_draw_stats".as_ptr()) };
        let (mut frames, mut nanos) = (0u64, 0u64);
        if !p.is_null() {
            // SAFETY: the function has this signature and writes two u64s.
            unsafe { std::mem::transmute::<*mut c_void, Stats>(p)(&mut frames, &mut nanos) };
        }
        (frames, nanos)
    }

    /// Prints the overlay's draw time between two [`stats`] readings.
    /// `more` is appended to the line. Returns the frames drawn.
    fn cost(phase: &str, before: (u64, u64), after: (u64, u64), more: &str) -> u64 {
        let frames = after.0.saturating_sub(before.0);
        let nanos = after.1.saturating_sub(before.1);
        let avg = nanos.checked_div(frames).unwrap_or(0);
        println!("cost {phase} frames={frames} avg_ns={avg}{more}");
        frames
    }

    /// The game's drawing: the whole frame red.
    fn clear(gl: &Gl) {
        // SAFETY: GL calls on the current context.
        unsafe {
            (gl.clear_color)(1.0, 0.0, 0.0, 1.0);
            (gl.clear)(0x4000);
        }
    }

    /// One game frame.
    fn frame(gl: &Gl, swap: &mut impl FnMut()) {
        clear(gl);
        swap();
    }

    /// Swaps until the pixel at (`x`, `y` from the top) is red (`red`) or not; that pixel,
    /// or `None` after 15 s.
    fn until(gl: &Gl, swap: &mut impl FnMut(), x: c_int, y: c_int, red: bool) -> Option<[u8; 4]> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            frame(gl, swap);
            let px = gl.pixel(x, y);
            if (px == RED) == red {
                return Some(px);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// The median time over `n` cleared frames of the swap call alone, or (`finish`) of the
    /// swap and a `glFinish`: the frame's raster work, the overlay's blits included, lands
    /// there (an EGL pbuffer swap does none of it).
    fn timed(gl: &Gl, swap: &mut impl FnMut(), n: usize, finish: bool) -> u128 {
        let mut took: Vec<u128> = (0..n)
            .map(|_| {
                clear(gl);
                let started = Instant::now();
                swap();
                if finish {
                    // SAFETY: a GL call on the current context.
                    unsafe { (gl.finish)() };
                }
                started.elapsed().as_nanos()
            })
            .collect();
        took.sort_unstable();
        took.get(took.len() / 2).copied().unwrap_or(0)
    }

    /// The hidden figures over [`HIDDEN_SWAPS`] frames each (after a few to warm up), as the
    /// `median_ns=… frame_ns=…` of a `cost hidden` line.
    fn hidden(gl: &Gl, swap: &mut impl FnMut()) -> String {
        for _ in 0..30 {
            frame(gl, swap);
        }
        let alone = timed(gl, swap, HIDDEN_SWAPS, false);
        let whole = timed(gl, swap, HIDDEN_SWAPS, true);
        format!("median_ns={alone} frame_ns={whole}")
    }

    /// The three phases (module docs); the exit code.
    fn frames(gl: &Gl, mut swap: impl FnMut(), baseline: bool) -> i32 {
        let swap = &mut swap;
        if baseline {
            println!("cost hidden swaps={HIDDEN_SWAPS} {}", hidden(gl, swap));
            return 0;
        }
        // The first toast: top right, inside its card.
        let (toast_x, toast_y) = (W - 40, 20);
        let Some(toast) = until(gl, swap, toast_x, toast_y, false) else {
            println!("the overlay never drew");
            return 3;
        };
        let corner = gl.pixel(10, H - 10);
        println!("toast={toast:?} corner={corner:?}");
        if corner != RED {
            return 4;
        }
        let shown = stats();
        for _ in 0..FRAMES {
            frame(gl, swap);
        }
        if cost("toast", shown, stats(), "") != FRAMES as u64 {
            println!("the overlay stopped drawing");
            return 8;
        }

        // The broker empties the view on that line.
        if until(gl, swap, toast_x, toast_y, true).is_none() {
            println!("the toast stayed");
            return 5;
        }
        let before = stats();
        let figures = hidden(gl, swap);
        let drawn = stats().0.saturating_sub(before.0);
        println!("cost hidden swaps={HIDDEN_SWAPS} {figures} drawn={drawn}");
        if drawn != 0 {
            return 6;
        }

        // The broker opens the panel on that line: a full-height column on the right.
        let before = stats();
        let Some(panel) = until(gl, swap, W - 40, H - 20, false) else {
            println!("the panel never drew");
            return 7;
        };
        let corner = gl.pixel(10, H - 10);
        println!("panel={panel:?} corner={corner:?}");
        if corner != RED {
            return 4;
        }
        let opened = stats();
        cost("opening", before, opened, "");
        let whole = timed(gl, swap, FRAMES, true);
        let drawn = cost("open", opened, stats(), &format!(" frame_ns={whole}"));
        let panel = gl.pixel(W - 40, H - 20);
        if drawn != FRAMES as u64 || panel == RED {
            println!("the overlay stopped drawing (panel={panel:?})");
            return 8;
        }
        0
    }

    pub fn run(api: Api, baseline: bool) -> i32 {
        match api {
            Api::Egl => egl(baseline).unwrap_or(2),
            Api::Glx => glx(baseline).unwrap_or(2),
        }
    }

    fn egl(baseline: bool) -> Option<i32> {
        type GetDisplay = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
        type Initialize = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_uint;
        type ChooseConfig = unsafe extern "C" fn(
            *mut c_void,
            *const c_int,
            *mut *mut c_void,
            c_int,
            *mut c_int,
        ) -> c_uint;
        type BindApi = unsafe extern "C" fn(c_uint) -> c_uint;
        type CreatePbuffer =
            unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int) -> *mut c_void;
        type CreateContext = unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *const c_int,
        ) -> *mut c_void;
        type MakeCurrent =
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> c_uint;
        type Swap = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
        let lib = Lib::open(c"libEGL.so.1")?;
        // SAFETY: EGL 1.4 entry points with their C signatures, called in the documented order.
        unsafe {
            let get_display: GetDisplay = lib.get(c"eglGetDisplay")?;
            let initialize: Initialize = lib.get(c"eglInitialize")?;
            let choose: ChooseConfig = lib.get(c"eglChooseConfig")?;
            let bind: BindApi = lib.get(c"eglBindAPI")?;
            let pbuffer: CreatePbuffer = lib.get(c"eglCreatePbufferSurface")?;
            let context: CreateContext = lib.get(c"eglCreateContext")?;
            let make_current: MakeCurrent = lib.get(c"eglMakeCurrent")?;
            let swap: Swap = lib.get(c"eglSwapBuffers")?;
            let gpa: GetProc = lib.get(c"eglGetProcAddress")?;

            let dpy = get_display(std::ptr::null_mut());
            if dpy.is_null() || initialize(dpy, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                return None;
            }
            // EGL_SURFACE_TYPE PBUFFER, EGL_RENDERABLE_TYPE OPENGL, 8-bit RGBA.
            let attrs = [
                0x3033, 0x0001, 0x3040, 0x0008, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3038,
            ];
            let mut config = std::ptr::null_mut();
            let mut n = 0;
            if choose(dpy, attrs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 {
                return None;
            }
            bind(0x30A2); // EGL_OPENGL_API
            let surface = pbuffer(dpy, config, [0x3057, W, 0x3056, H, 0x3038].as_ptr());
            let ctx = context(dpy, config, std::ptr::null_mut(), [0x3038].as_ptr());
            if surface.is_null() || ctx.is_null() || make_current(dpy, surface, surface, ctx) == 0 {
                return None;
            }
            let gl = Gl::load(gpa)?;
            Some(frames(
                &gl,
                || {
                    swap(dpy, surface);
                },
                baseline,
            ))
        }
    }

    fn glx(baseline: bool) -> Option<i32> {
        type OpenDisplay = unsafe extern "C" fn(*const i8) -> *mut c_void;
        type DefaultScreen = unsafe extern "C" fn(*mut c_void) -> c_int;
        type ChooseFbConfig =
            unsafe extern "C" fn(*mut c_void, c_int, *const c_int, *mut c_int) -> *mut *mut c_void;
        type CreatePbuffer =
            unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int) -> std::ffi::c_ulong;
        type CreateNewContext = unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            c_int,
            *mut c_void,
            c_int,
        ) -> *mut c_void;
        type MakeContextCurrent = unsafe extern "C" fn(
            *mut c_void,
            std::ffi::c_ulong,
            std::ffi::c_ulong,
            *mut c_void,
        ) -> c_int;
        type Swap = unsafe extern "C" fn(*mut c_void, std::ffi::c_ulong);
        let x11 = Lib::open(c"libX11.so.6")?;
        let glx = Lib::open(c"libGL.so.1")?;
        // SAFETY: Xlib and GLX 1.3 entry points with their C signatures.
        unsafe {
            let open: OpenDisplay = x11.get(c"XOpenDisplay")?;
            let screen: DefaultScreen = x11.get(c"XDefaultScreen")?;
            let gpa: GetProc = glx.get(c"glXGetProcAddressARB")?;
            let choose: ChooseFbConfig = std::mem::transmute(gpa(c"glXChooseFBConfig".as_ptr()));
            let pbuffer: CreatePbuffer = std::mem::transmute(gpa(c"glXCreatePbuffer".as_ptr()));
            let context: CreateNewContext =
                std::mem::transmute(gpa(c"glXCreateNewContext".as_ptr()));
            let make_current: MakeContextCurrent =
                std::mem::transmute(gpa(c"glXMakeContextCurrent".as_ptr()));
            // The swap through glXGetProcAddressARB, as SDL and GLEW do.
            let swap_p = gpa(c"glXSwapBuffers".as_ptr());
            if swap_p.is_null() {
                return None;
            }
            let swap: Swap = std::mem::transmute(swap_p);

            let dpy = open(std::ptr::null());
            if dpy.is_null() {
                return None;
            }
            // GLX_DRAWABLE_TYPE PBUFFER, GLX_RENDER_TYPE RGBA, 8-bit RGB (the screen's depth).
            let attrs = [0x8010, 0x0004, 0x8011, 0x0001, 8, 8, 9, 8, 10, 8, 0];
            let mut n = 0;
            let configs = choose(dpy, screen(dpy), attrs.as_ptr(), &mut n);
            if configs.is_null() || n < 1 {
                return None;
            }
            let config = *configs;
            let surface = pbuffer(dpy, config, [0x8041, W, 0x8040, H, 0].as_ptr());
            let ctx = context(dpy, config, 0x8014, std::ptr::null_mut(), 1);
            if surface == 0 || ctx.is_null() || make_current(dpy, surface, surface, ctx) == 0 {
                return None;
            }
            let gl = Gl::load(gpa)?;
            Some(frames(&gl, || swap(dpy, surface), baseline))
        }
    }
}
