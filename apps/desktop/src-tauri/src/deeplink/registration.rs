//! Startup-only protocol repair. No idle timers and no WebView filesystem access.
use tokio_util::sync::CancellationToken;

pub fn spawn(app: tauri::AppHandle, shutdown: CancellationToken) {
    #[cfg(target_os = "linux")]
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let Ok(data) = app.path().data_dir() else {
            return;
        };
        let executable = app
            .env()
            .appimage
            .map(std::path::PathBuf::from)
            .or_else(|| tauri::utils::platform::current_exe().ok());
        let Some(executable) = executable else { return };
        if linux::repair(
            data,
            executable,
            app.config().identifier.clone(),
            &shutdown,
            &linux::Tools::default(),
        )
        .await
        .is_err()
            && !shutdown.is_cancelled()
        {
            tracing::warn!("cannot repair launch-link registration");
        }
    });
    #[cfg(windows)]
    tauri::async_runtime::spawn(async move {
        if shutdown.is_cancelled() {
            return;
        }
        let result =
            tauri::async_runtime::spawn_blocking(move || windows_repair(&app, "vgames")).await;
        if !matches!(result, Ok(Ok(()))) && !shutdown.is_cancelled() {
            tracing::warn!("cannot repair launch-link registration");
        }
    });
    #[cfg(target_os = "macos")]
    tauri::async_runtime::spawn(async move {
        let Ok(executable) = tauri::utils::platform::current_exe() else {
            return;
        };
        if macos::repair(
            executable,
            app.config().identifier.clone(),
            "vgames".into(),
            &shutdown,
        )
        .await
        .is_err()
            && !shutdown.is_cancelled()
        {
            tracing::warn!("cannot repair launch-link registration");
        }
    });
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    let _ = (app, shutdown);
}

#[cfg(windows)]
fn windows_repair<R: tauri::Runtime>(app: &tauri::AppHandle<R>, scheme: &str) -> Result<(), ()> {
    use tauri_plugin_deep_link::DeepLinkExt;
    // Always rewrite HKCU, even if a handler exists: an upgrade can move the exe.
    app.deep_link().register(scheme).map_err(|_| ())?;
    match app.deep_link().is_registered(scheme) {
        Ok(true) => Ok(()),
        _ => Err(()),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io::Write,
        path::{Path, PathBuf},
        process::Stdio,
        time::Duration,
    };
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    pub(super) struct Tools {
        pub database: PathBuf,
        pub mime: PathBuf,
        #[cfg(test)]
        pub receipt: Option<PathBuf>,
    }
    impl Default for Tools {
        fn default() -> Self {
            Self {
                database: "update-desktop-database".into(),
                mime: "xdg-mime".into(),
                #[cfg(test)]
                receipt: None,
            }
        }
    }
    fn exec_argument(text: &str) -> Result<String, ()> {
        let mut argument = String::new();
        for character in text.chars() {
            if character.is_control() {
                return Err(());
            }
            match character {
                '\\' => argument.push_str("\\\\"),
                '"' | '$' | '`' => {
                    argument.push('\\');
                    argument.push(character);
                }
                '%' => argument.push_str("%%"),
                other => argument.push(other),
            }
        }
        // Desktop string unescaping happens before Exec argument unescaping.
        Ok(format!("\"{}\"", argument.replace('\\', "\\\\")))
    }
    fn quoted_exec(path: &Path) -> Result<String, ()> {
        if !path.is_absolute() {
            return Err(());
        }
        let executable = exec_argument(path.to_str().ok_or(())?)?;
        // GLib checks argv[0] before expanding %% to a literal percent. A
        // fixed shell trampoline avoids rejecting a valid percent-bearing
        // filename. Its code is constant; executable and URL remain arguments.
        Ok(format!(
            "/bin/sh -c {} vgames-link {executable} %u",
            exec_argument("exec \"$@\"")?
        ))
    }
    fn write(data: &Path, executable: &Path, identifier: &str) -> Result<(PathBuf, String), ()> {
        if identifier.is_empty()
            || identifier.len() > 200
            || !identifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(());
        }
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=vgames\nExec={}\nTerminal=false\nMimeType=x-scheme-handler/vgames;\nNoDisplay=true\nCategories=Game;\n",
            quoted_exec(executable)?
        );
        let applications = data.join("applications");
        std::fs::create_dir_all(&applications).map_err(|_| ())?;
        let name = format!("{identifier}-handler.desktop");
        let temporary = applications.join(format!(".vgames-{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| ())?;
            file.write_all(text.as_bytes()).map_err(|_| ())?;
            file.sync_all().map_err(|_| ())?;
            std::fs::rename(&temporary, applications.join(&name)).map_err(|_| ())?;
            std::fs::File::open(&applications)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| ())?;
            Ok((applications, name))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
    async fn command(
        program: &Path,
        args: &[&std::ffi::OsStr],
        tools: &Tools,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<(), ()> {
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(test)]
        if let Some(receipt) = &tools.receipt {
            command.env("VGAMES_REGISTRATION_RECEIPT", receipt);
        }
        #[cfg(not(test))]
        let _ = tools;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(()),
            result = tokio::time::timeout_at(deadline, command.status()) => match result {
                Ok(Ok(status)) if status.success() => Ok(()),
                _ => Err(()),
            },
        }
    }
    pub(super) async fn repair(
        data: PathBuf,
        executable: PathBuf,
        identifier: String,
        cancel: &CancellationToken,
        tools: &Tools,
    ) -> Result<(), ()> {
        if cancel.is_cancelled() {
            return Err(());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let (directory, name) =
            tokio::task::spawn_blocking(move || write(&data, &executable, &identifier))
                .await
                .map_err(|_| ())??;
        command(
            &tools.database,
            &[directory.as_os_str()],
            tools,
            deadline,
            cancel,
        )
        .await?;
        command(
            &tools.mime,
            &[
                "default".as_ref(),
                name.as_ref(),
                "x-scheme-handler/vgames".as_ref(),
            ],
            tools,
            deadline,
            cancel,
        )
        .await
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tauri_plugin_deep_link::DeepLinkExt;
    #[test]
    fn deleted_per_user_registration_is_recreated_and_matches_the_current_executable() {
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_deep_link::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let scheme = format!("vgames-fixture-{}", uuid::Uuid::now_v7().simple());
        struct Cleanup<'a>(&'a tauri::AppHandle<tauri::test::MockRuntime>, String);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.deep_link().unregister(&self.1);
            }
        }
        let cleanup = Cleanup(app.handle(), scheme.clone());
        windows_repair(app.handle(), &scheme).unwrap();
        app.deep_link().unregister(&scheme).unwrap();
        assert!(!app.deep_link().is_registered(&scheme).unwrap());
        windows_repair(app.handle(), &scheme).unwrap();
        assert!(app.deep_link().is_registered(&scheme).unwrap());
        drop(cleanup);
    }
}

#[cfg(any(test, target_os = "macos"))]
mod macos;
