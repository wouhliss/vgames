//! `vgames://` routing (00-overview §3.1, 01-security §7). Every URL is
//! untrusted: the parser accepts only the documented forms, each parameter
//! exactly once, nothing else. Unknown forms are logged (route only) and
//! ignored.
//!
//! Routes: `server/add` (the UI asks the user to confirm; the pin is taken
//! only by `server_confirm`), `auth/callback` (needs a matching pending
//! sign-in) and `launch/{package_id}` (the active server's package, through
//! the same checks and rate limit as the Play button; the URL never adds
//! arguments). `package/…` and `invite/…` arrive with the UI routes for them.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::events::{AppEvent, EventBus, PackageRef, ServerAddRequested};
use crate::launch::TargetChoice;
use crate::launch::orchestrate::Launcher;
use crate::servers::Servers;
use crate::servers::auth::{parse_callback_error_link, parse_callback_link};
use crate::servers::discovery::normalize_url;

const MAX_LINK_CHARS: usize = 4096;

/// A recognized deep link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    /// `vgames://server/add?url={https-url}&fp={root-fingerprint}`
    ServerAdd { url: String, fingerprint: String },
    /// `vgames://auth/callback?code=…&client_state=…`
    AuthCallback { code: String, client_state: String },
    /// `vgames://auth/callback?error=…&client_state=…` (a refused sign-in)
    AuthRefused { error: String, client_state: String },
    /// `vgames://launch/{package_id}` (desktop shortcuts)
    Launch { package_id: uuid::Uuid },
}

/// Parses a deep link; `None` for anything not in the grammar.
pub fn parse(text: &str, allow_loopback_http: bool) -> Option<DeepLink> {
    if text.len() > MAX_LINK_CHARS {
        return None;
    }
    let url = Url::parse(text).ok()?;
    if url.scheme() != "vgames" || url.fragment().is_some() {
        return None;
    }
    match (url.host_str()?, url.path()) {
        ("server", "/add") => {
            let mut server = None;
            let mut fp = None;
            for (key, value) in url.query_pairs() {
                let slot = match key.as_ref() {
                    "url" => &mut server,
                    "fp" => &mut fp,
                    _ => return None,
                };
                if slot.replace(value.into_owned()).is_some() {
                    return None;
                }
            }
            let server = normalize_url(&server?, allow_loopback_http).ok()?;
            let fingerprint: vgames_core::sign::Fingerprint = fp?.parse().ok()?;
            Some(DeepLink::ServerAdd {
                url: server.to_string(),
                fingerprint: fingerprint.to_string(),
            })
        }
        ("auth", "/callback") => {
            if let Some((code, client_state)) = parse_callback_link(text) {
                return Some(DeepLink::AuthCallback { code, client_state });
            }
            let (error, client_state) = parse_callback_error_link(text)?;
            Some(DeepLink::AuthRefused {
                error,
                client_state,
            })
        }
        ("launch", path) => {
            if url.query().is_some() {
                return None;
            }
            let id = path.strip_prefix('/')?;
            // Canonical lowercase hyphenated form only.
            let package_id = uuid::Uuid::try_parse(id).ok()?;
            (package_id.hyphenated().to_string() == id && !package_id.is_nil())
                .then_some(DeepLink::Launch { package_id })
        }
        _ => None,
    }
}

/// Routes deep links from the bus until `shutdown`. `ui_ready` delays
/// UI-facing events until the UI listens (a link can start the launcher).
pub fn spawn_router(
    servers: Arc<Servers>,
    launcher: Arc<Launcher<Servers>>,
    bus: EventBus,
    ui_ready: CancellationToken,
    allow_loopback_http: bool,
    shutdown: CancellationToken,
) {
    let mut receiver = bus.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => break,
                event = receiver.recv() => event,
            };
            let url = match event {
                Ok(AppEvent::DeepLinkReceived { url }) => url,
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "deep-link router lagged");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match parse(&url, allow_loopback_http) {
                Some(DeepLink::ServerAdd { url, fingerprint }) => {
                    let bus = bus.clone();
                    let ui_ready = ui_ready.clone();
                    tauri::async_runtime::spawn(async move {
                        ui_ready.cancelled().await;
                        bus.publish(AppEvent::ServerAddRequested(ServerAddRequested {
                            url,
                            fingerprint,
                        }));
                    });
                }
                Some(DeepLink::AuthCallback { code, client_state }) => {
                    let servers = Arc::clone(&servers);
                    tauri::async_runtime::spawn(async move {
                        servers.auth_callback(&code, &client_state).await;
                    });
                }
                Some(DeepLink::Launch { package_id }) => {
                    let servers = Arc::clone(&servers);
                    let launcher = Arc::clone(&launcher);
                    tauri::async_runtime::spawn(async move {
                        launch_from_link(&servers, &launcher, package_id).await;
                    });
                }
                Some(DeepLink::AuthRefused {
                    error,
                    client_state,
                }) => servers.auth_callback_error(&error, &client_state),
                None => {
                    let route = Url::parse(&url)
                        .ok()
                        .and_then(|u| u.host_str().map(str::to_owned))
                        .unwrap_or_default();
                    tracing::info!(route = %route.chars().take(32).collect::<String>(), "deep link not handled");
                }
            }
        }
    });
}

/// Launches a package of the active server; failures are logged (the
/// launcher applies every check, including the rate limit).
async fn launch_from_link(servers: &Servers, launcher: &Launcher<Servers>, package_id: uuid::Uuid) {
    let server_id = match servers.active_id().await {
        Ok(Some(id)) => id,
        Ok(None) => {
            tracing::info!("launch link without an active server; ignored");
            return;
        }
        Err(error) => {
            tracing::warn!(error = %crate::error::DisplayChain(&error), "cannot read the active server");
            return;
        }
    };
    let package = PackageRef {
        server_id,
        package_id,
    };
    match launcher.launch(package, TargetChoice::Default).await {
        Ok(pid) => tracing::info!(pid, "launched from a link"),
        Err(error) => tracing::info!(%error, "launch link refused"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "VG1-0000-0000-0000-0000-0000-0000-0000-0000";

    #[test]
    fn server_add_links_are_parsed_and_normalized() {
        let link = format!("vgames://server/add?url=https%3A%2F%2Fgames.example.com%2Fx&fp={FP}");
        assert_eq!(
            parse(&link, false),
            Some(DeepLink::ServerAdd {
                url: "https://games.example.com/".into(),
                fingerprint: FP.into()
            })
        );
    }

    #[test]
    fn anything_outside_the_grammar_is_ignored() {
        for link in [
            format!("vgames://server/add?url=http%3A%2F%2Fgames.example.com&fp={FP}"),
            format!("vgames://server/add?url=https%3A%2F%2Fa.example&fp={FP}&x=1"),
            format!(
                "vgames://server/add?url=https%3A%2F%2Fa.example&url=https%3A%2F%2Fb.example&fp={FP}"
            ),
            "vgames://server/add?url=https%3A%2F%2Fa.example&fp=VG1-nope".into(),
            "vgames://server/add?url=https%3A%2F%2Fa.example".into(),
            format!("vgames://server/remove?url=https%3A%2F%2Fa.example&fp={FP}"),
            format!("https://server/add?url=https%3A%2F%2Fa.example&fp={FP}"),
            format!("vgames://server/add?url=https%3A%2F%2Fa.example&fp={FP}#frag"),
            "vgames://auth/callback?code=a".into(),
            "not a url".into(),
            format!(
                "vgames://server/add?url=https%3A%2F%2Fa.example&fp={FP}&pad={}",
                "a".repeat(5000)
            ),
        ] {
            assert_eq!(parse(&link, true), None, "{link}");
        }
    }

    #[test]
    fn auth_callbacks_are_recognized() {
        assert_eq!(
            parse("vgames://auth/callback?code=abc&client_state=xyz", false),
            Some(DeepLink::AuthCallback {
                code: "abc".into(),
                client_state: "xyz".into()
            })
        );
    }

    #[test]
    fn refused_sign_ins_are_recognized() {
        assert_eq!(
            parse(
                "vgames://auth/callback?error=user_disabled&client_state=xyz",
                false
            ),
            Some(DeepLink::AuthRefused {
                error: "user_disabled".into(),
                client_state: "xyz".into()
            })
        );
        assert_eq!(
            parse("vgames://auth/callback?error=user_disabled", false),
            None
        );
    }

    #[test]
    fn launch_links_take_only_a_canonical_package_id() {
        let id = "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b";
        assert_eq!(
            parse(&format!("vgames://launch/{id}"), false),
            Some(DeepLink::Launch {
                package_id: uuid::Uuid::parse_str(id).unwrap()
            })
        );
        for bad in [
            format!("vgames://launch/{id}?args=-console"),
            format!("vgames://launch/{id}/extra"),
            format!("vgames://launch/{id}#x"),
            format!("vgames://launch/{}", id.to_uppercase()),
            format!("vgames://launch/{}", id.replace('-', "")),
            "vgames://launch/00000000-0000-0000-0000-000000000000".into(),
            "vgames://launch/".into(),
            "vgames://launch".into(),
        ] {
            assert_eq!(parse(&bad, false), None, "{bad}");
        }
    }

    proptest::proptest! {
        #[test]
        fn parsing_never_panics(text in "\\PC{0,300}", tail in "[a-z0-9=&%:/?._~-]{0,200}") {
            let _ = parse(&text, true);
            let _ = parse(&format!("vgames://server/add?{tail}"), false);
            let _ = parse(&format!("vgames://auth/callback?{tail}"), false);
            let _ = parse(&format!("vgames://launch/{tail}"), false);
        }
    }
}
