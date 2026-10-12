//! Confirmed external links. Only ordinary ASCII-host http(s) URLs reach the
//! operating-system browser; untrusted input is never included in errors/logs.
use crate::error::AppError;

#[tauri::command]
#[specta::specta]
pub async fn open_external_url(url: String) -> Result<(), AppError> {
    let url = checked_url(&url)?;
    let opened = tokio::task::spawn_blocking(move || open::that_detached(url.as_str()).is_ok())
        .await
        .map_err(|error| AppError::internal("open the browser", &error))?;
    if opened {
        Ok(())
    } else {
        Err(AppError::internal(
            "open the browser",
            &std::io::Error::other("No browser could be started."),
        ))
    }
}
fn checked_url(input: &str) -> Result<url::Url, AppError> {
    let invalid = || {
        AppError::invalid(
            "url",
            "Use an ordinary website link starting with http:// or https://.",
        )
    };
    if input.len() > 8192
        || input
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(invalid());
    }
    let url = url::Url::parse(input).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    let host = url.host_str().ok_or_else(invalid)?;
    // IDNA normalizes Unicode and already-encoded homographs to these labels.
    // Refuse both forms rather than relying on the WebView's displayed label.
    if host.split('.').any(|label| label.starts_with("xn--")) {
        return Err(invalid());
    }
    Ok(url)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_plain_website_schemes_and_hosts_reach_the_browser() {
        for link in [
            "javascript:alert(1)",
            "file:///tmp/test",
            "vgames://launch/example",
            "data:text/plain,hello",
            "https://раypal.com/",
            "https://xn--ypal-43d9g.com/",
            "https://name:password@example.com/",
            "https://example.com/\n",
            "https://example.com/ a",
        ] {
            assert!(
                matches!(checked_url(link), Err(AppError::InvalidInput { .. })),
                "{link}"
            );
        }
        for link in [
            "https://example.com/help?q=controllers#setup",
            "http://127.0.0.1:8080/",
            "https://[::1]/",
        ] {
            assert_eq!(checked_url(link).unwrap().as_str(), link);
        }
    }
    #[test]
    fn rejected_url_details_are_not_echoed_in_errors() {
        let input = "https://user:private-value@example.com/";
        let error = serde_json::to_string(&checked_url(input).unwrap_err()).unwrap();
        assert!(!error.contains("private-value"));
        assert!(!error.contains("example.com"));
    }
}
