//! The GL hooks in a real GL app (GAME-08): `vgames-gl-testapp` started with the built
//! library in `LD_PRELOAD`, Mesa's software rasteriser (llvmpipe) and a fake broker. Checks
//! that the hooks connect as an OpenGL renderer, that a toast lands in the swapped frame and
//! that the rest of the frame is untouched, for EGL (surfaceless, reached through `dlsym`)
//! and GLX (Xvfb, reached through `glXGetProcAddressARB`). Then the frame cost against
//! 05-social §6.1, each figure compared with the same app run without the overlay:
//! - hidden (the broker emptied the view): the swap call may cost at most [`HIDDEN_BUDGET`]
//!   more (median), and the overlay draws nothing;
//! - panel open: the overlay's own draw time at most [`gl::FRAME_BUDGET`] per frame, and the
//!   swap with a `glFinish` (llvmpipe's raster work for our blits included) at most
//!   [`gl::FRAME_BUDGET`] more (median), over [`FRAMES`] frames all drawn;
//! - opening the panel (compose and upload, once): at most [`OPENING_BOUND`] in all, a bound
//!   on the hitch rather than a §6.1 budget.
//!
//! The figures are printed as `overlay frame cost:` lines (`--nocapture`).
//!
//! Skipped (with a message) when Mesa's EGL/GL or Xvfb is missing, unless
//! `VGAMES_REQUIRE_GL=1`.
#![cfg(all(feature = "renderer", target_os = "linux", target_arch = "x86_64"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::Duration;

use uuid::Uuid;
use vgames_overlay::gl;
use vgames_overlay::protocol::{
    self, PROTOCOL_VERSION, RendererKind, TOKEN_LEN, ToBroker, ToRenderer, Toast, ToastKind, View,
};

const APP: &str = env!("CARGO_BIN_EXE_vgames-gl-testapp");

/// 05-social §6.1: what the overlay may add to a frame while nothing is visible.
const HIDDEN_BUDGET: Duration = Duration::from_micros(300);
/// The frames the app measures a visible phase over (its `FRAMES`).
const FRAMES: u64 = 120;
/// The one-off hitch of opening the panel (compose and upload of a full-height card). Local
/// debug builds on llvmpipe take 12-21 ms (one frame); this leaves room for a slow hosted
/// runner while still catching a per-open cost that grew several times over.
const OPENING_BOUND: Duration = Duration::from_millis(100);

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

/// Welcomes one renderer, reports its kind and sends a toast; then sends what the test
/// passes on (heartbeats in between) until the test or the app is done.
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
            client_nonce,
            renderer,
            version,
        } = protocol::read_frame::<ToBroker>(&mut s).unwrap()
        else {
            panic!("expected Hello");
        };
        assert_eq!(version, PROTOCOL_VERSION);
        let server_nonce = protocol::fresh_nonce().unwrap();
        let proof = protocol::auth_proof(
            &token,
            protocol::AuthRole::Broker,
            version,
            renderer,
            &client_nonce,
            &server_nonce,
        )
        .unwrap();
        protocol::write_frame(
            &mut s,
            &ToRenderer::Challenge {
                server_nonce,
                proof,
            },
        )
        .unwrap();
        let ToBroker::Authenticate { proof } = protocol::read_frame(&mut s).unwrap() else {
            panic!("expected renderer proof");
        };
        assert!(protocol::verify_auth(
            &token,
            protocol::AuthRole::Renderer,
            version,
            renderer,
            &client_nonce,
            &server_nonce,
            &proof
        ));
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

fn required() -> bool {
    std::env::var_os("VGAMES_REQUIRE_GL").is_some()
}

/// `key=value` from one of the app's `cost …` lines.
fn field(line: &str, key: &str) -> u64 {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {line:?}"))
}

/// Median swap call, and median swap with `glFinish`, from a `cost hidden` line.
fn hidden_figures(line: &str) -> (Duration, Duration) {
    (
        Duration::from_nanos(field(line, "median_ns")),
        Duration::from_nanos(field(line, "frame_ns")),
    )
}

/// What the app measured with the overlay.
#[derive(Default)]
struct Costs {
    toast: Duration,
    toast_frames: u64,
    hidden: (Duration, Duration),
    /// In all, over the frames until the panel showed.
    opening: Duration,
    open: Duration,
    open_frames: u64,
    /// Median swap with `glFinish`.
    open_frame: Duration,
}

/// The same app without the overlay: [`hidden_figures`]; `None` when it found no GL.
fn run_baseline(mode: &str, extra: &[(&str, &str)]) -> Option<(Duration, Duration)> {
    let mut cmd = Command::new(APP);
    cmd.args([mode, "baseline"])
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env_remove("LD_PRELOAD");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    match out.status.code() {
        Some(0) => {
            let line = stdout
                .lines()
                .find(|l| l.starts_with("cost hidden"))
                .unwrap();
            Some(hidden_figures(line))
        }
        Some(2) => {
            assert!(!required(), "{mode}: no GL to run on\n{stdout}{stderr}");
            eprintln!("skipped {mode}: no GL to run on");
            None
        }
        code => panic!("{mode} baseline: exit {code:?}\n{stdout}{stderr}"),
    }
}

/// Runs the app with the overlay preloaded, driving the broker through the app's phases;
/// `None` when it found no GL to run on.
fn run_app(mode: &str, extra: &[(&str, &str)]) -> Option<(RendererKind, Costs)> {
    let token = [7u8; TOKEN_LEN];
    let (endpoint, hello, send) = broker(token);
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
    let mut child = cmd.spawn().unwrap();
    let mut err = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        let mut s = String::new();
        err.read_to_string(&mut s).ok();
        s
    });
    let mut stdout = String::new();
    let mut costs = Costs::default();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        eprintln!("{mode}: {line}");
        let avg = || Duration::from_nanos(field(&line, "avg_ns"));
        match line.split_whitespace().take(2).collect::<Vec<_>>()[..] {
            ["cost", "toast"] => {
                costs.toast = avg();
                costs.toast_frames = field(&line, "frames");
                send.send(ToRenderer::View(View::default())).unwrap();
            }
            ["cost", "hidden"] => {
                costs.hidden = hidden_figures(&line);
                send.send(ToRenderer::Panel { open: true }).unwrap();
            }
            ["cost", "opening"] => {
                costs.opening = avg() * u32::try_from(field(&line, "frames")).unwrap();
            }
            ["cost", "open"] => {
                costs.open = avg();
                costs.open_frames = field(&line, "frames");
                costs.open_frame = Duration::from_nanos(field(&line, "frame_ns"));
            }
            _ => {}
        }
        stdout.push_str(&line);
        stdout.push('\n');
    }
    let status = child.wait().unwrap();
    let stderr = stderr.join().unwrap();
    match status.code() {
        Some(0) => Some((
            hello
                .recv_timeout(Duration::from_secs(1))
                .expect("the hooks said hello"),
            costs,
        )),
        Some(2) => {
            assert!(!required(), "{mode}: no GL to run on\n{stdout}{stderr}");
            eprintln!("skipped {mode}: no GL to run on");
            None
        }
        code => panic!("{mode}: exit {code:?}\n{stdout}{stderr}"),
    }
}

/// The toast, the panel and their cost (module docs), for one GL API.
fn check(mode: &str, extra: &[(&str, &str)]) {
    // One API at a time: the timings would otherwise share the CPU with the other run.
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let Some((without, without_frame)) = run_baseline(mode, extra) else {
        return;
    };
    let Some((kind, costs)) = run_app(mode, extra) else {
        return;
    };
    assert_eq!(kind, RendererKind::OpenGl);
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let (hidden, hidden_frame) = costs.hidden;
    eprintln!(
        "overlay frame cost: {mode} hidden {:+.4} ms per swap ({:.4} ms with the overlay, {:.4} \
         ms without; budget {:.1} ms); with glFinish {:.4} vs {:.4} ms",
        ms(hidden) - ms(without),
        ms(hidden),
        ms(without),
        ms(HIDDEN_BUDGET),
        ms(hidden_frame),
        ms(without_frame),
    );
    eprintln!(
        "overlay frame cost: {mode} panel open {:.3} ms per frame in the hooks, {:+.3} ms per swap \
         with glFinish ({:.3} vs {:.3} ms without the overlay) over {} frames (budget {:.1} ms); \
         toast {:.3} ms per frame",
        ms(costs.open),
        ms(costs.open_frame) - ms(without_frame),
        ms(costs.open_frame),
        ms(without_frame),
        costs.open_frames,
        ms(gl::FRAME_BUDGET),
        ms(costs.toast),
    );
    eprintln!(
        "overlay frame cost: {mode} opening the panel {:.3} ms in all (bound {:.0} ms)",
        ms(costs.opening),
        ms(OPENING_BOUND),
    );
    assert!(
        hidden.saturating_sub(without) <= HIDDEN_BUDGET,
        "{mode}: hidden swap {hidden:?} vs {without:?} without the overlay"
    );
    assert_eq!(costs.toast_frames, FRAMES, "{mode}: toast frames drawn");
    assert_eq!(costs.open_frames, FRAMES, "{mode}: panel frames drawn");
    assert!(
        costs.toast <= gl::FRAME_BUDGET,
        "{mode}: toast {:?}",
        costs.toast
    );
    assert!(
        costs.open <= gl::FRAME_BUDGET,
        "{mode}: panel open {:?}",
        costs.open
    );
    assert!(
        costs.open_frame.saturating_sub(without_frame) <= gl::FRAME_BUDGET,
        "{mode}: panel open swap {:?} vs {without_frame:?} without the overlay",
        costs.open_frame
    );
    assert!(
        costs.opening <= OPENING_BOUND,
        "{mode}: opening the panel took {:?}",
        costs.opening
    );
}

#[test]
fn egl_frames_get_the_toast_and_the_panel_within_budget() {
    check("egl", &[("EGL_PLATFORM", "surfaceless")]);
}

#[test]
fn glx_frames_get_the_toast_and_the_panel_within_budget() {
    let Some(display) = Xvfb::start() else {
        assert!(!required(), "Xvfb is not installed");
        eprintln!("skipped glx: Xvfb is not installed");
        return;
    };
    check("glx", &[("DISPLAY", &display.name)]);
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
