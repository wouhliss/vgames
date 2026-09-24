//! The error every command returns to the UI: a closed `code` the UI switches
//! on, plus a human message. Internal details (paths aside from the user's own
//! folders, SQL, OS error chains) stay in the logs.

use serde::Serialize;
use specta::Type;

/// Stable, closed set of error codes for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// An argument failed validation (bad id, path outside a library, …).
    InvalidArgument,
    NotFound,
    /// The action conflicts with the current state (already installing, …).
    Conflict,
    /// The command is not available in this window or state.
    NotAllowed,
    /// Sign-in is required or the session ended.
    Unauthenticated,
    /// The server refused the action for this account.
    Forbidden,
    /// The server could not be reached.
    Offline,
    /// A signature, hash or trust check failed. Never retried automatically.
    Integrity,
    /// Not enough disk space.
    DiskFull,
    /// Anything else. Details are in the log.
    Internal,
}

/// Error payload of every command.
#[derive(Debug, Clone, Serialize, Type, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    /// Logs `error` with its full chain and returns a generic message.
    pub fn internal(context: &str, error: &dyn std::error::Error) -> Self {
        tracing::error!(error = %DisplayChain(error), "{context}");
        Self::new(
            ErrorCode::Internal,
            format!("{context}. See the log for details."),
        )
    }
}

/// Formats an error and all its sources on one line.
pub struct DisplayChain<'a>(pub &'a dyn std::error::Error);

impl std::fmt::Display for DisplayChain<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(error) = source {
            write!(f, ": {error}")?;
            source = error.source();
        }
        Ok(())
    }
}

pub type CommandResult<T> = Result<T, CommandError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_snake_case_code() {
        let json =
            serde_json::to_value(CommandError::new(ErrorCode::DiskFull, "Need 3 GB more")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"code": "disk_full", "message": "Need 3 GB more"})
        );
    }

    #[test]
    fn display_chain_includes_sources() {
        let inner = std::io::Error::other("inner");
        let outer = crate::paths::PathsError::Create {
            path: "/x".into(),
            source: inner,
        };
        assert_eq!(DisplayChain(&outer).to_string(), "cannot create /x: inner");
    }
}
