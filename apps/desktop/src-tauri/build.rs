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
