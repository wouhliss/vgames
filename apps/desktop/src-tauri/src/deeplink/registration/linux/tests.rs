use super::*;
use gio::prelude::{AppInfoExt, AppLaunchContextExt};
use std::os::unix::fs::PermissionsExt;

fn script(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
async fn launched(file: &Path, receipt: &Path, executable: &Path) {
    let info = gio::DesktopAppInfo::from_filename(file).unwrap();
    assert!(info.supports_uris());
    let context = gio::AppLaunchContext::new();
    context.setenv("VGAMES_LINK_RECEIPT", receipt);
    let url = "vgames://launch/01920000-0000-7000-8000-000000000011";
    info.launch_uris(&[url], Some(&context)).unwrap();
    let expected = format!("{}\n{url}\n", executable.display());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        while gio::glib::MainContext::default().pending() {
            gio::glib::MainContext::default().iteration(false);
        }
        if std::fs::read_to_string(receipt).is_ok_and(|value| value == expected) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "registered executable did not receive the original URL"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
const EXECUTABLE: &str = "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > \"$VGAMES_LINK_RECEIPT\"\n";
#[tokio::test]
async fn deleted_registration_and_changed_appimage_path_are_repaired_at_startup() {
    let root = tempfile::tempdir().unwrap();
    let receipt = root.path().join("receipt");
    let tool = root.path().join("tool");
    script(
        &tool,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$VGAMES_REGISTRATION_RECEIPT\"\n",
    );
    let tools = Tools {
        database: tool.clone(),
        mime: tool,
        receipt: Some(receipt.clone()),
    };
    let data = root.path().join("data");
    let first = root.path().join("vgames v1.AppImage");
    let second = root.path().join("vgames v2.AppImage");
    script(&first, EXECUTABLE);
    script(&second, EXECUTABLE);
    let cancel = CancellationToken::new();
    let id = "app.vgames.launcher".to_owned();
    let file = data.join("applications/app.vgames.launcher-handler.desktop");
    repair(data.clone(), first, id.clone(), &cancel, &tools)
        .await
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    repair(data.clone(), second.clone(), id.clone(), &cancel, &tools)
        .await
        .unwrap();
    launched(&file, &root.path().join("launch1"), &second).await;
    // A stale existing handler must also be rewritten after an upgrade.
    std::fs::write(
        &file,
        "[Desktop Entry]\nType=Application\nName=Old\nExec=/old/vgames %u\n",
    )
    .unwrap();
    repair(data, second.clone(), id, &cancel, &tools)
        .await
        .unwrap();
    launched(&file, &root.path().join("launch2"), &second).await;
    let commands = std::fs::read_to_string(receipt).unwrap();
    assert!(commands.contains("x-scheme-handler/vgames"));
    assert!(commands.contains("app.vgames.launcher-handler.desktop"));
}

#[tokio::test]
async fn exec_quoting_keeps_shell_characters_and_percent_field_codes_literal() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("vgames $cash`quote\"percent%.AppImage");
    script(&executable, EXECUTABLE);
    let (dir, name) = write(root.path(), &executable, "app.vgames.launcher").unwrap();
    // Launch through GLib: display metadata does not expand Exec field codes.
    launched(&dir.join(name), &root.path().join("launch"), &executable).await;
    assert!(quoted_exec(Path::new("relative.AppImage")).is_err());
    assert!(quoted_exec(Path::new("/tmp/vgames\n.AppImage")).is_err());
    assert!(write(root.path(), &executable, "../../outside").is_err());
}

#[tokio::test]
async fn cancelled_or_hung_registration_tools_do_not_leave_waiting_tasks() {
    let root = tempfile::tempdir().unwrap();
    let tool = root.path().join("hang");
    script(&tool, "#!/bin/sh\nexec /bin/sleep 60\n");
    let tools = Tools {
        database: tool.clone(),
        mime: tool.clone(),
        receipt: None,
    };
    let cancel = CancellationToken::new();
    let deadline = Instant::now() + Duration::from_millis(30);
    assert!(
        command(&tool, &[], &tools, deadline, &cancel)
            .await
            .is_err()
    );
    cancel.cancel();
    assert!(
        repair(
            root.path().join("never-created"),
            root.path().join("vgames"),
            "app.vgames.launcher".into(),
            &cancel,
            &tools
        )
        .await
        .is_err()
    );
    assert!(!root.path().join("never-created").exists());
}
