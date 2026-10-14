//! Build-generated notices; no runtime filesystem or network access.
use crate::error::AppError;

const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/third-party-notices.txt"));

#[tauri::command]
#[specta::specta]
pub async fn app_licenses() -> Result<String, AppError> {
    Ok(NOTICES.to_owned())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn notices_are_embedded_for_both_dependency_families() {
        let notices = super::app_licenses().await.unwrap();
        assert!(notices.contains("Rust dependencies"));
        assert!(notices.contains("npm production dependencies"));
        for name in ["sdl3", "react", "tauri", "Declared license:"] {
            assert!(notices.contains(name), "missing notice for {name}");
        }
    }
}
