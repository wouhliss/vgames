//! A stand-in for a GL game, the child process of `tests/gl_preload.rs`: its own X window and
//! GLX context, `libGL` opened `RTLD_LOCAL` (the way SDL does), a few frames cleared to blue, then
//! the window read back from the X server. It does not link the overlay crate, so without
//! `LD_PRELOAD` nothing of the overlay is in this process.
//!
//! Environment: `VGAMES_GL_LOOKUP=handle|gpa` (how the swap function is found), `VGAMES_GL_LIB`
//! (the overlay library, to read its draw counter when it is preloaded).
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::time::{Duration, Instant};

const W: u32 = 640;
const H: u32 = 480;
/// Inside the first toast (scale 1 at this height), clear of the accent bar and the text.
const PROBE: (u32, u32) = (W - 12 - 3, 12 + 3);

type Display = *mut c_void;

#[repr(C)]
struct XVisualInfo {
    visual: *mut c_void,
    visualid: c_ulong,
    screen: c_int,
    depth: c_int,
    class: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
    colormap_size: c_int,
    bits_per_rgb: c_int,
}

#[repr(C)]
#[derive(Default)]
struct XSetWindowAttributes {
    background_pixmap: c_ulong,
    background_pixel: c_ulong,
    border_pixmap: c_ulong,
    border_pixel: c_ulong,
    bit_gravity: c_int,
    win_gravity: c_int,
    backing_store: c_int,
    backing_planes: c_ulong,
    backing_pixel: c_ulong,
    save_under: c_int,
    event_mask: c_long,
    do_not_propagate_mask: c_long,
    override_redirect: c_int,
    colormap: c_ulong,
    cursor: c_ulong,
}

/// Behaves like a game: its own window and GL context, `libGL` opened `RTLD_LOCAL`, a few
/// frames with a toast on top, then reads the window back from the X server.
fn main() {
    let lookup = std::env::var("VGAMES_GL_LOOKUP").unwrap_or_else(|_| "handle".into());
    // SAFETY: Xlib/GLX calls in their documented order; nothing is used after it is released.
    unsafe {
        let x11 = libloading::Library::new("libX11.so.6").unwrap();
        let gl = libloading::Library::new("libGL.so.1").unwrap();
        macro_rules! sym {
            ($lib:expr, $name:literal, $ty:ty) => {
                *$lib.get::<$ty>(concat!($name, "\0").as_bytes()).unwrap()
            };
        }
        let open: unsafe extern "C" fn(*const c_char) -> Display = sym!(x11, "XOpenDisplay", _);
        let default_screen: unsafe extern "C" fn(Display) -> c_int = sym!(x11, "XDefaultScreen", _);
        let root: unsafe extern "C" fn(Display, c_int) -> c_ulong = sym!(x11, "XRootWindow", _);
        let create_colormap: unsafe extern "C" fn(Display, c_ulong, *mut c_void, c_int) -> c_ulong =
            sym!(x11, "XCreateColormap", _);
        let create_window: unsafe extern "C" fn(
            Display,
            c_ulong,
            c_int,
            c_int,
            c_uint,
            c_uint,
            c_uint,
            c_int,
            c_uint,
            *mut c_void,
            c_ulong,
            *mut XSetWindowAttributes,
        ) -> c_ulong = sym!(x11, "XCreateWindow", _);
        let map_window: unsafe extern "C" fn(Display, c_ulong) -> c_int =
            sym!(x11, "XMapWindow", _);
        let sync: unsafe extern "C" fn(Display, c_int) -> c_int = sym!(x11, "XSync", _);
        let get_image: unsafe extern "C" fn(
            Display,
            c_ulong,
            c_int,
            c_int,
            c_uint,
            c_uint,
            c_ulong,
            c_int,
        ) -> *mut c_void = sym!(x11, "XGetImage", _);
        let get_pixel: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_ulong =
            sym!(x11, "XGetPixel", _);

        let dpy = open(std::ptr::null());
        assert!(!dpy.is_null(), "cannot open the X display");
        let screen = default_screen(dpy);
        // GLX_RGBA, GLX_DOUBLEBUFFER, GLX_DEPTH_SIZE 16, None.
        let choose_visual: unsafe extern "C" fn(Display, c_int, *mut c_int) -> *mut XVisualInfo =
            sym!(gl, "glXChooseVisual", _);
        let mut attribs = [4, 5, 12, 16, 0];
        let vi = choose_visual(dpy, screen, attribs.as_mut_ptr());
        assert!(!vi.is_null(), "no GLX visual");
        let mut attr = XSetWindowAttributes {
            colormap: create_colormap(dpy, root(dpy, screen), (*vi).visual, 0),
            ..Default::default()
        };
        // CWBorderPixel | CWColormap.
        let win = create_window(
            dpy,
            root(dpy, screen),
            0,
            0,
            W,
            H,
            0,
            (*vi).depth,
            1,
            (*vi).visual,
            (1 << 3) | (1 << 13),
            &mut attr,
        );
        map_window(dpy, win);
        sync(dpy, 0);

        let create_context: unsafe extern "C" fn(
            Display,
            *mut XVisualInfo,
            *mut c_void,
            c_int,
        ) -> *mut c_void = sym!(gl, "glXCreateContext", _);
        let make_current: unsafe extern "C" fn(Display, c_ulong, *mut c_void) -> c_int =
            sym!(gl, "glXMakeCurrent", _);
        let ctx = create_context(dpy, vi, std::ptr::null_mut(), 1);
        assert!(!ctx.is_null(), "no GLX context");
        assert_ne!(make_current(dpy, win, ctx), 0);

        // The way the game finds its functions: by the library handle (SDL) or through
        // `glXGetProcAddress` (GLFW).
        let (clear_color, clear, swap): (
            unsafe extern "C" fn(f32, f32, f32, f32),
            unsafe extern "C" fn(c_uint),
            unsafe extern "C" fn(Display, c_ulong),
        ) = if lookup == "gpa" {
            let gpa: unsafe extern "C" fn(*const c_char) -> *mut c_void =
                sym!(gl, "glXGetProcAddressARB", _);
            let f = |n: &CStr| gpa(n.as_ptr());
            (
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn(f32, f32, f32, f32)>(f(
                    c"glClearColor",
                )),
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn(c_uint)>(f(c"glClear")),
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn(Display, c_ulong)>(f(
                    c"glXSwapBuffers",
                )),
            )
        } else {
            (
                sym!(gl, "glClearColor", _),
                sym!(gl, "glClear", _),
                sym!(gl, "glXSwapBuffers", _),
            )
        };

        let started = Instant::now();
        let mut shown = 0;
        while started.elapsed() < Duration::from_secs(6) && shown < 20 {
            clear_color(0.0, 0.0, 1.0, 1.0);
            clear(0x4000);
            swap(dpy, win);
            sync(dpy, 0);
            std::thread::sleep(Duration::from_millis(50));
            if started.elapsed() > Duration::from_millis(1500) {
                shown += 1;
            }
        }
        let image = get_image(dpy, win, 0, 0, W, H, c_ulong::MAX, 2);
        assert!(!image.is_null(), "XGetImage failed");
        let rgb = |x: u32, y: u32| (get_pixel(image, x as c_int, y as c_int) & 0x00FF_FFFF) as u32;
        println!("PROBE={:x}", rgb(PROBE.0, PROBE.1));
        println!("CORNER={:x}", rgb(10, H - 10));

        let frames = match std::env::var("VGAMES_GL_LIB") {
            Ok(path) if std::env::var_os("LD_PRELOAD").is_some() => {
                let lib = libloading::Library::new(path).unwrap();
                let f: libloading::Symbol<'_, unsafe extern "C" fn(*mut u64, *mut u64)> =
                    lib.get(b"vgames_overlay_gl_draw_stats\0").unwrap();
                let (mut n, mut ns) = (0u64, 0u64);
                f(&mut n, &mut ns);
                println!("NANOS={ns:x}");
                n
            }
            _ => 0,
        };
        println!("FRAMES={frames:x}");
    }
}
