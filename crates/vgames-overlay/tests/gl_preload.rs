//! The GL hooks in a real GL app (GAME-08): `vgames-gl-testapp` started with the built
//! library in `LD_PRELOAD`, Mesa's software rasteriser (llvmpipe) and a fake broker. Checks
//! that the hooks connect as an OpenGL renderer, that a toast lands in the swapped frame and
//! that the rest of the frame is untouched, for EGL (surfaceless, reached through `dlsym`)
//! and GLX (Xvfb, reached through `glXGetProcAddressARB`).
//!
//! Skipped (with a message) when Mesa's EGL/GL or Xvfb is missing, unless
//! `VGAMES_REQUIRE_GL=1`.
#![cfg(all(feature = "renderer", target_os = "linux", target_arch = "x86_64"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use uuid::Uuid;
use vgames_overlay::protocol::{
    self, PROTOCOL_VERSION, RendererKind, TOKEN_LEN, ToBroker, ToRenderer, Toast, ToastKind, View,
};

const APP: &str = env!("CARGO_BIN_EXE_vgames-gl-testapp");

fn built_library() -> PathBuf {
    let app = PathBuf::from(APP);
    let dir = app.parent().unwrap();
    [
        dir.join("deps/libvgames_overlay.so"),
        dir.join("libvgames_overlay.so"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .expect("the overlay library next to the test app")
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

fn required() -> bool {
    std::env::var_os("VGAMES_REQUIRE_GL").is_some()
}

/// Runs the app with the overlay preloaded; `None` when it found no GL to run on.
fn run_app(mode: &str, extra: &[(&str, &str)]) -> Option<RendererKind> {
    let token = [7u8; TOKEN_LEN];
    let (endpoint, hello) = broker(token);
    let mut cmd = Command::new(APP);
    cmd.arg(mode)
        .env("LD_PRELOAD", built_library())
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env(protocol::env::ENABLED, "1")
        .env(protocol::env::ENDPOINT, &endpoint)
        .env(protocol::env::TOKEN, protocol::token_hex(&token))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    match out.status.code() {
        Some(0) => Some({
            eprint!("{mode}: {stdout}");
            hello
                .recv_timeout(Duration::from_secs(1))
                .expect("the hooks said hello")
        }),
        Some(2) => {
            assert!(!required(), "{mode}: no GL to run on\n{stdout}{stderr}");
            eprintln!("skipped {mode}: no GL to run on");
            None
        }
        code => panic!("{mode}: exit {code:?}\n{stdout}{stderr}"),
    }
}

#[test]
fn egl_frames_get_the_toast() {
    if let Some(kind) = run_app("egl", &[("EGL_PLATFORM", "surfaceless")]) {
        assert_eq!(kind, RendererKind::OpenGl);
    }
}

#[test]
fn glx_frames_get_the_toast() {
    let Some(display) = Xvfb::start() else {
        assert!(!required(), "Xvfb is not installed");
        eprintln!("skipped glx: Xvfb is not installed");
        return;
    };
    if let Some(kind) = run_app("glx", &[("DISPLAY", &display.name)]) {
        assert_eq!(kind, RendererKind::OpenGl);
    }
}

struct Xvfb {
    child: std::process::Child,
    name: String,
}

impl Xvfb {
    fn start() -> Option<Self> {
        for n in 90..110 {
            if std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() {
                continue;
            }
            let name = format!(":{n}");
            let child = Command::new("Xvfb")
                .args([
                    name.as_str(),
                    "-screen",
                    "0",
                    "640x480x24",
                    "-nolisten",
                    "tcp",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            for _ in 0..100 {
                if std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() {
                    return Some(Self { child, name });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            return None;
        }
        None
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}
