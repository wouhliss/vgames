//! The Linux GL preload in a real GL app (A4-T11): Xvfb + Mesa's software rasteriser, the
//! built library in `LD_PRELOAD`, a fake broker, and a child process that behaves like a game
//! (opens `libGL` with `RTLD_LOCAL`, the way SDL does, and swaps buffers). The child reads the
//! window back from the X server: the toast must be in the frame, the rest of it untouched,
//! and without the preload there is no toast.
//!
//! Skipped (with a message) when Xvfb or libGL is missing, unless `VGAMES_REQUIRE_GL=1`.
#![cfg(all(feature = "renderer", target_os = "linux"))]
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use uuid::Uuid;
use vgames_overlay::protocol::{
    self, PROTOCOL_VERSION, RendererKind, TOKEN_LEN, ToBroker, ToRenderer, Toast, ToastKind, View,
};

const CARD_BG: u32 = 0x001C_1D24;
const GAME_BLUE: u32 = 0x0000_00FF;

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

struct Xvfb {
    child: Child,
    display: String,
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_xvfb() -> Option<Xvfb> {
    let mut child = Command::new("Xvfb")
        .args([
            "-screen",
            "0",
            "1024x768x24",
            "-nolisten",
            "tcp",
            "-displayfd",
            "1",
            "+extension",
            "GLX",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut line)
        .ok()?;
    let n = line.trim().parse::<u32>().ok()?;
    Some(Xvfb {
        child,
        display: format!(":{n}"),
    })
}

fn skip(why: &str) -> bool {
    assert!(
        std::env::var_os("VGAMES_REQUIRE_GL").is_none(),
        "required but not available: {why}"
    );
    eprintln!("skipped: {why}");
    true
}

struct Run {
    probe: u32,
    corner: u32,
    resized: u32,
    resized_corner: u32,
    frames: u64,
    nanos: u64,
    hello: Option<RendererKind>,
}

/// Runs the child game; `preload` puts the overlay library in `LD_PRELOAD`.
fn run_game(display: &str, lookup: &str, preload: bool) -> Run {
    let token = [9u8; TOKEN_LEN];
    let (endpoint, hello) = broker(token);
    let mut cmd = Command::new(game());
    cmd.env("VGAMES_GL_LOOKUP", lookup)
        .env("DISPLAY", display)
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env("GALLIUM_DRIVER", "llvmpipe")
        .env("VGAMES_GL_LIB", built_library().unwrap())
        .env(protocol::env::ENABLED, "1")
        .env(protocol::env::ENDPOINT, &endpoint)
        .env(protocol::env::TOKEN, protocol::token_hex(&token))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if preload {
        cmd.env("LD_PRELOAD", built_library().unwrap());
    }
    let out = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "child failed:\n{text}");
    let field = |name: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
            .unwrap_or_else(|| panic!("no {name} in child output:\n{text}"))
    };
    Run {
        probe: field("PROBE=") as u32,
        corner: field("CORNER=") as u32,
        resized: field("RESIZED=") as u32,
        resized_corner: field("RESIZED_CORNER=") as u32,
        frames: field("FRAMES="),
        nanos: if preload { field("NANOS=") } else { 0 },
        hello: hello.recv_timeout(Duration::from_millis(200)).ok(),
    }
}

/// The stand-in game (`examples/gl_game.rs`; `cargo test` builds examples next to the tests).
fn game() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let path = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/gl_game");
    assert!(
        path.is_file(),
        "{} is missing: build it with `cargo build -p vgames-overlay --features renderer --examples`",
        path.display()
    );
    path
}

fn environment_ready() -> Option<Xvfb> {
    if built_library().is_none() {
        panic!("the overlay library next to the test binary");
    }
    if Command::new("Xvfb").arg("-version").output().is_err() {
        skip("Xvfb is not installed");
        return None;
    }
    if !std::path::Path::new("/usr/lib/x86_64-linux-gnu/libGL.so.1").exists()
        && !std::path::Path::new("/usr/lib/libGL.so.1").exists()
        && !std::path::Path::new("/usr/lib64/libGL.so.1").exists()
        && !std::path::Path::new("/usr/lib/aarch64-linux-gnu/libGL.so.1").exists()
    {
        skip("libGL is not installed");
        return None;
    }
    let x = start_xvfb();
    if x.is_none() {
        skip("Xvfb did not start");
    }
    x
}

#[test]
fn the_preload_draws_a_toast_into_the_frame_of_a_game_that_loads_libgl_itself() {
    let Some(x) = environment_ready() else { return };
    // Without the preload the game's frame is all its own (and no broker is contacted).
    let plain = run_game(&x.display, "handle", false);
    assert_eq!(plain.probe, GAME_BLUE);
    assert_eq!(plain.frames, 0);
    assert!(plain.hello.is_none());

    // SDL style (`dlsym` on the `libGL` handle) and GLFW style (`glXGetProcAddress`).
    for lookup in ["handle", "gpa"] {
        let r = run_game(&x.display, lookup, true);
        assert_eq!(r.hello, Some(RendererKind::OpenGl), "lookup {lookup}");
        assert_eq!(r.probe, CARD_BG, "the toast is in the frame ({lookup})");
        assert_eq!(r.corner, GAME_BLUE, "the rest is the game's ({lookup})");
        // After the game changes its resolution the cards sit at the new top right.
        assert_eq!(r.resized, CARD_BG, "toast after a resize ({lookup})");
        assert_eq!(
            r.resized_corner, GAME_BLUE,
            "rest after a resize ({lookup})"
        );
        assert!(r.frames > 0, "frames drawn ({lookup})");
        // Debug build on the software rasteriser, where the copy itself runs on the CPU and the
        // first frame creates the objects: a loose bound here; the layer turns itself off above
        // 1 ms on average (A4-T11 budget), and release numbers are in the status file.
        let per_frame = Duration::from_nanos(r.nanos / r.frames);
        eprintln!(
            "GL preload ({lookup}): {} frames, {per_frame:?} per frame",
            r.frames
        );
        assert!(
            per_frame < Duration::from_millis(10),
            "{per_frame:?} per frame ({lookup})"
        );
    }
}
