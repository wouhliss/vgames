//! Rosetta 2 on Apple silicon (09-compatibility §3, GAME-06): x86_64 Wine needs it.
//! Detected with `arch -x86_64 /usr/bin/true`; installed (`rosetta_install`, GAME-07) with
//! `softwareupdate --install-rosetta --agree-to-license` only after the player said yes in a
//! native dialog that Rust shows itself (GAME-14 C4): the UI's own confirmation is not enough,
//! since the WebView can call the command without it.

use std::time::Duration;

use serde::Serialize;
use specta::Type;

/// Text of the native confirmation dialog.
pub const CONFIRM_TITLE: &str = "Install Rosetta 2?";
pub const CONFIRM_TEXT: &str = "vgames will install Rosetta 2 from Apple so this Mac can run games made for Intel Macs. Installing it accepts Apple's license for Rosetta 2. macOS may ask for your password.";
pub const CONFIRM_OK: &str = "Install";
pub const CONFIRM_CANCEL: &str = "Cancel";

/// What `rosetta_install` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RosettaInstallOutcome {
    Installed,
    /// The player said no in the native dialog; nothing was run.
    Cancelled,
}

/// Installing Rosetta downloads a few hundred megabytes.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const DETECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RosettaError {
    #[error("Rosetta 2 is only for Macs with Apple silicon")]
    NotAppleSilicon,
    #[error("installing Rosetta 2 took too long")]
    TimedOut,
    #[error("installing Rosetta 2 failed: {0}")]
    Failed(String),
}

/// Whether x86_64 code runs here: always on Intel Macs, with Rosetta on Apple silicon.
pub async fn available() -> bool {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return cfg!(target_os = "macos");
    }
    runs("arch", &["-x86_64", "/usr/bin/true"], DETECT_TIMEOUT).await == Ok(true)
}

/// Whether Rosetta 2 can be installed here at all (Apple silicon).
pub fn installable() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

/// Asks the player with `confirm` (a native dialog in the app) and runs `installer`
/// only on a yes. Nothing is asked where Rosetta cannot be installed.
pub async fn install_after_confirmation(
    installable: bool,
    confirm: impl AsyncFnOnce() -> bool,
    installer: impl AsyncFnOnce() -> Result<(), RosettaError>,
) -> Result<RosettaInstallOutcome, RosettaError> {
    if !installable {
        return Err(RosettaError::NotAppleSilicon);
    }
    if !confirm().await {
        tracing::info!("the player declined installing Rosetta 2");
        return Ok(RosettaInstallOutcome::Cancelled);
    }
    installer().await?;
    Ok(RosettaInstallOutcome::Installed)
}

/// Installs Rosetta 2. Call only through [`install_after_confirmation`].
pub async fn install() -> Result<(), RosettaError> {
    if !installable() {
        return Err(RosettaError::NotAppleSilicon);
    }
    match runs(
        "/usr/sbin/softwareupdate",
        &["--install-rosetta", "--agree-to-license"],
        INSTALL_TIMEOUT,
    )
    .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(RosettaError::Failed(
            "softwareupdate reported an error".into(),
        )),
        Err(None) => Err(RosettaError::TimedOut),
        Err(Some(e)) => Err(RosettaError::Failed(e)),
    }
}

/// `Ok(success)`, `Err(None)` on timeout, `Err(Some(reason))` when it cannot start.
async fn runs(program: &str, args: &[&str], timeout: Duration) -> Result<bool, Option<String>> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, command.status()).await {
        Ok(Ok(status)) => Ok(status.success()),
        Ok(Err(e)) => Err(Some(e.to_string())),
        Err(_) => Err(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detection_matches_the_host() {
        let x86_runs = available().await;
        if cfg!(not(target_os = "macos")) {
            assert!(!x86_runs, "no Wine path off macOS");
            assert_eq!(install().await, Err(RosettaError::NotAppleSilicon));
        }
        if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            assert!(x86_runs);
        }
        // Apple silicon: recorded either way (hosted macos-15 runners ship Rosetta).
        println!("x86_64 code runs here: {x86_runs}");
    }

    use std::sync::atomic::{AtomicU32, Ordering};

    /// Runs the decision with a fake dialog answer and a fake installer.
    async fn decide(
        installable: bool,
        answer: bool,
        result: Result<(), RosettaError>,
    ) -> (Result<RosettaInstallOutcome, RosettaError>, u32, u32) {
        let asked = AtomicU32::new(0);
        let ran = AtomicU32::new(0);
        let got = install_after_confirmation(
            installable,
            async || {
                asked.fetch_add(1, Ordering::SeqCst);
                answer
            },
            async || {
                ran.fetch_add(1, Ordering::SeqCst);
                result
            },
        )
        .await;
        (got, asked.into_inner(), ran.into_inner())
    }

    #[tokio::test]
    async fn a_declined_confirmation_never_runs_the_installer() {
        let (got, asked, ran) = decide(true, false, Ok(())).await;
        assert_eq!(got, Ok(RosettaInstallOutcome::Cancelled));
        assert_eq!((asked, ran), (1, 0));
    }

    #[tokio::test]
    async fn an_accepted_confirmation_runs_the_installer_once() {
        let (got, asked, ran) = decide(true, true, Ok(())).await;
        assert_eq!(got, Ok(RosettaInstallOutcome::Installed));
        assert_eq!((asked, ran), (1, 1));
        let failed = Err(RosettaError::Failed("boom".into()));
        let (got, _, ran) = decide(true, true, failed.clone()).await;
        assert_eq!(got, Err(RosettaError::Failed("boom".into())));
        assert_eq!(ran, 1);
    }

    #[tokio::test]
    async fn nothing_is_asked_or_run_where_rosetta_does_not_exist() {
        let (got, asked, ran) = decide(false, true, Ok(())).await;
        assert_eq!(got, Err(RosettaError::NotAppleSilicon));
        assert_eq!((asked, ran), (0, 0));
    }

    #[test]
    fn the_dialog_names_apple_and_its_license() {
        assert!(CONFIRM_TEXT.contains("Apple"));
        assert!(CONFIRM_TEXT.contains("license"));
    }

    #[tokio::test]
    async fn a_missing_program_is_a_failure_not_a_panic() {
        assert!(matches!(
            runs("/nonexistent/vgames-arch", &[], DETECT_TIMEOUT).await,
            Err(Some(_))
        ));
    }
}
