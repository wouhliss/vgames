//! An on-demand support summary uses counts, never server/account identifiers,
//! user labels, URLs, environment dumps or filesystem paths.
use tauri::{AppHandle, Manager};

use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn app_diagnostics(app: AppHandle) -> String {
    let (db, ui_ready) = {
        let state = app.state::<AppState>();
        (state.db.clone(), state.ui_ready.is_cancelled())
    };
    let counts = db
        .call(|conn| {
            let servers: i64 =
                conn.query_row("SELECT COUNT(*) FROM servers", [], |row| row.get(0))?;
            let installed: i64 = conn.query_row(
                "SELECT COUNT(*) FROM installs WHERE state = 'installed'",
                [],
                |row| row.get(0),
            )?;
            Ok((servers, installed))
        })
        .await;
    let database = match counts {
        Ok((servers, installed)) => {
            format!("database: ready\nservers: {servers}\ninstalled packages: {installed}")
        }
        Err(_) => "database: unavailable".into(),
    };
    let text = format!(
        "vgames: {}\nos: {}\narchitecture: {}\ndebug build: {}\nUI ready: {}\n{database}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(debug_assertions),
        ui_ready
    );
    let home = app.path().home_dir().ok();
    redact_report(&text, home.as_deref())
}

/// Apply the common credential/query scrubber, then remove local home prefixes
/// and UUIDs if future summary fields contain them. Work on UTF-8 boundaries.
fn redact_report(text: &str, home: Option<&std::path::Path>) -> String {
    let mut text = crate::logging::redact(text).into_owned();
    if let Some(home) = home {
        let home = home.to_string_lossy();
        if !home.is_empty() {
            for variant in [
                home.to_string(),
                home.replace('\\', "/"),
                home.replace('\\', "\\\\"),
            ] {
                text = text.replace(&variant, "[home]");
            }
        }
    }
    let mut result = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text
            .get(index..index + 36)
            .is_some_and(|candidate| uuid::Uuid::parse_str(candidate).is_ok())
        {
            result.push_str("[id]");
            index += 36;
        } else if let Some(character) = text.get(index..).and_then(|rest| rest.chars().next()) {
            result.push(character);
            index += character.len_utf8();
        } else {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn support_text_removes_credentials_signed_queries_home_paths_and_ids() {
        let text = "token=vga_abcdefghijklmnop signed=https://example.test/blob?X-Goog-Signature=private&key=secret path=/home/player/saves/game.txt user=01920000-0000-7000-8000-000000000001 ✓";
        let result = redact_report(text, Some(std::path::Path::new("/home/player")));
        for secret in [
            "abcdefghijklmnop",
            "X-Goog-Signature",
            "private",
            "key=secret",
            "/home/player",
            "01920000-0000-7000-8000-000000000001",
        ] {
            assert!(!result.contains(secret), "report leaked {secret}");
        }
        assert!(result.contains("[home]/saves/game.txt"));
        assert!(result.contains("user=[id] ✓"));
    }
    #[test]
    fn windows_home_separators_are_scrubbed_without_corrupting_unicode() {
        let text = r"C:\Users\Player\save C:/Users/Player/save C:\\Users\\Player\\save é";
        let result = redact_report(text, Some(std::path::Path::new(r"C:\Users\Player")));
        assert!(!result.contains("Player"));
        assert!(result.ends_with('é'));
    }
}
