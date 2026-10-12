//! A native Linux GL "game" for `tests/gl_preload.rs` (GAME-08): clears a 640×480 surface to
//! red and swaps until the pixel under the first toast is not red (the overlay drew), then
//! checks a corner far from the cards is still red. It loads GL like most engines do, with
//! `dlopen` and lookups by handle, so the preloaded overlay is reached through its `dlsym`
//! (EGL) and `glXGetProcAddressARB` (GLX) hooks. It never links the overlay itself.
//!
//! `vgames-gl-testapp egl|glx`. Exit codes: 0 drawn, 2 no GL here (skip), 3 never drawn,
//! 4 drew outside the cards.
#![allow(unsafe_code, clippy::print_stdout, clippy::print_stderr)]

use std::ffi::{CStr, c_int, c_uint, c_void};
use std::time::{Duration, Instant};

const W: c_int = 640;
const H: c_int = 480;

fn main() {
    let code = match std::env::args().nth(1).as_deref() {
        #[cfg(target_os = "linux")]
        Some("egl") => linux::run(linux::Api::Egl),
        #[cfg(target_os = "linux")]
        Some("glx") => linux::run(linux::Api::Glx),
        _ => {
            eprintln!("usage: vgames-gl-testapp egl|glx (Linux only)");
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

    /// The preloaded overlay's draw counters (`vgames_overlay_gl_draw_stats`).
    fn stats() {
        type Stats = unsafe extern "C" fn(*mut u64, *mut u64);
        // SAFETY: a global lookup of the overlay's exported counter function.
        let p =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"vgames_overlay_gl_draw_stats".as_ptr()) };
        if p.is_null() {
            return;
        }
        let (mut frames, mut nanos) = (0u64, 0u64);
        // SAFETY: the function has this signature and writes two u64s.
        unsafe { std::mem::transmute::<*mut c_void, Stats>(p)(&mut frames, &mut nanos) };
        if let Some(avg) = nanos.checked_div(frames) {
            println!("drawn={frames} avg_us={}", avg / 1000);
        }
    }

    /// Swaps until the toast shows; checks a corner stays untouched.
    fn frames(gl: &Gl, mut swap: impl FnMut()) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            // SAFETY: GL calls on the current context.
            unsafe {
                (gl.clear_color)(1.0, 0.0, 0.0, 1.0);
                (gl.clear)(0x4000);
            }
            swap();
            // The first toast: top right, inside its card.
            let toast = gl.pixel(W - 40, 20);
            if toast != RED {
                let corner = gl.pixel(10, H - 10);
                println!("toast={toast:?} corner={corner:?}");
                // Keep drawing a while for the cost figures, then print them.
                for _ in 0..120 {
                    swap();
                }
                stats();
                return if corner == RED { 0 } else { 4 };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("the overlay never drew");
        3
    }

    pub fn run(api: Api) -> i32 {
        match api {
            Api::Egl => egl().unwrap_or(2),
            Api::Glx => glx().unwrap_or(2),
        }
    }

    fn egl() -> Option<i32> {
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
            Some(frames(&gl, || {
                swap(dpy, surface);
            }))
        }
    }

    fn glx() -> Option<i32> {
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
            Some(frames(&gl, || swap(dpy, surface)))
        }
    }
}
