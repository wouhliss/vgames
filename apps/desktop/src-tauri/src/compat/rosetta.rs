//! Rosetta 2 on Apple silicon (09-compatibility §3, GAME-06): x86_64 Wine needs it.
//! Detected with `arch -x86_64 /usr/bin/true`; installed only after the UI's explicit
//! confirmation (`rosetta_install`, GAME-07) with
//! `softwareupdate --install-rosetta --agree-to-license`.

use std::time::Duration;

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

/// Installs Rosetta 2. Call only after the user confirmed in the UI.
pub async fn install() -> Result<(), RosettaError> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
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

    #[tokio::test]
    async fn a_missing_program_is_a_failure_not_a_panic() {
        assert!(matches!(
            runs("/nonexistent/vgames-arch", &[], DETECT_TIMEOUT).await,
            Err(Some(_))
        ));
    }
}
