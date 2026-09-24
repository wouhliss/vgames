// Command lists shared with the crate (see the file for the rules).
include!("src/commands/names.rs");

fn main() {
    // Declaring the app's commands makes Tauri generate one `allow-<command>`
    // permission per command and deny any command a window's capability does
    // not grant. That is what confines the overlay window to `overlay_*`.
    let commands: Vec<&'static str> = MAIN_WINDOW_COMMANDS
        .iter()
        .chain(OVERLAY_WINDOW_COMMANDS)
        .copied()
        .collect();
    let commands: &'static [&'static str] = Box::leak(commands.into_boxed_slice());

    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(commands));
    if let Err(error) = tauri_build::try_build(attributes) {
        // Build scripts report failures by exiting non-zero with a message.
        eprintln!("tauri-build failed: {error:#}");
        std::process::exit(1);
    }
}
