// Command lists shared with the crate (see the file for the rules).
include!("src/commands/names.rs");

fn main() {
    // Tauri's Windows resource is attached to the app binary, but Cargo's
    // library unit-test executable also needs Common Controls v6. Cargo's
    // `rustc-link-arg-tests` does not cover library unit tests, so embed the
    // same manifest in every executable target through the linker. Suppress
    // Tauri's resource manifest below to avoid embedding it twice.
    let windows_msvc = matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("windows")
    ) && matches!(std::env::var("CARGO_CFG_TARGET_ENV").as_deref(), Ok("msvc"));
    if windows_msvc {
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        println!("cargo:rustc-link-arg=/WX");
    }

    // Declaring the app's commands makes Tauri generate one `allow-<command>`
    // permission per command and deny any command a window's capability does
    // not grant. That is what confines the overlay window to `overlay_*`.
    let commands: Vec<&'static str> = MAIN_WINDOW_COMMANDS
        .iter()
        .chain(OVERLAY_WINDOW_COMMANDS)
        .copied()
        .collect();
    let commands: &'static [&'static str] = Box::leak(commands.into_boxed_slice());

    embed_runtime_catalog_key();
    embed_third_party_notices();
    link_clang_runtime_on_macos();

    let mut attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(commands));
    if windows_msvc {
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    if let Err(error) = tauri_build::try_build(attributes) {
        // Build scripts report failures by exiting non-zero with a message.
        eprintln!("tauri-build failed: {error:#}");
        std::process::exit(1);
    }
}

/// Build tooling reads installed package notices; the app only serves the
/// embedded result. A generation failure fails the build rather than shipping
/// empty or stale notices.
fn embed_third_party_notices() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for input in [
        "scripts/third-party-notices.mjs",
        "Cargo.lock",
        "Cargo.toml",
        "pnpm-lock.yaml",
        "apps/desktop/package.json",
        "apps/desktop/src-tauri/Cargo.toml",
    ] {
        println!("cargo:rerun-if-changed={}", repo.join(input).display());
    }
    let out = std::env::var_os("OUT_DIR").map(std::path::PathBuf::from);
    let target = std::env::var("TARGET");
    let (Some(out), Ok(target)) = (out, target) else {
        eprintln!("notice generation requires Cargo's OUT_DIR and TARGET");
        std::process::exit(1);
    };
    let result = std::process::Command::new("node")
        .arg(repo.join("scripts/third-party-notices.mjs"))
        .arg("--out")
        .arg(out.join("third-party-notices.txt"))
        .arg("--target")
        .arg(target)
        .status();
    if !result.is_ok_and(|status| status.success()) {
        eprintln!("cannot generate third-party notices; install the locked npm dependencies first");
        std::process::exit(1);
    }
}

/// SDL3 is built from source, and its Cocoa code uses Objective-C `@available`, which calls
/// `___isPlatformVersionAtLeast` from clang's runtime library. rustc links with `-nodefaultlibs`,
/// so the app binary (which starts SDL) fails to link on macOS unless that archive is named.
fn link_clang_runtime_on_macos() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let ask = |flag: &str| {
        std::process::Command::new("clang")
            .arg(flag)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let candidates = [
        ask("--print-runtime-dir").map(std::path::PathBuf::from),
        ask("--print-resource-dir").map(|dir| std::path::Path::new(&dir).join("lib/darwin")),
    ];
    let Some(archive) = candidates
        .into_iter()
        .flatten()
        .map(|dir| dir.join("libclang_rt.osx.a"))
        .find(|archive| archive.is_file())
    else {
        eprintln!("cannot find clang's libclang_rt.osx.a (needed to link SDL3 on macOS)");
        std::process::exit(1);
    };
    println!("cargo:rustc-link-arg={}", archive.display());
}

/// Copies `runtimes/runtime-catalog.pub` (INT-08) into `OUT_DIR` for
/// `compat::catalog`, or an empty file when it does not exist yet: the runtime
/// manager is then off (09-compatibility §5).
fn embed_runtime_catalog_key() {
    let runtimes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../runtimes");
    // Watching the directory notices the key file appearing.
    println!("cargo:rerun-if-changed={}", runtimes.display());
    let key = std::fs::read_to_string(runtimes.join("runtime-catalog.pub")).unwrap_or_default();
    let Some(out) = std::env::var_os("OUT_DIR") else {
        eprintln!("OUT_DIR is not set");
        std::process::exit(1);
    };
    let target = std::path::Path::new(&out).join("runtime-catalog.pub");
    if let Err(error) = std::fs::write(&target, key) {
        eprintln!("cannot write {}: {error}", target.display());
        std::process::exit(1);
    }
}
