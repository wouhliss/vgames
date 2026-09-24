//! Trust bundles and root key helpers (A1-T08). All cryptography comes from `vgames-core`
//! (Agent 5); this module only adapts it to HTTP.

use crate::error::{ApiError, ApiResult};

/// Root key fingerprint as shown to users (`VG1-…`, docs/architecture/01-security.md §3.1).
///
/// Delegates to `vgames-core` (A5-T03). Until that interface lands, `/.well-known/vgames.json`
/// answers 503 rather than risking a second, divergent implementation of the fingerprint.
pub fn root_fingerprint(_root_public_key: &[u8; 32]) -> ApiResult<String> {
    Err(ApiError::unavailable().with_detail("The server fingerprint is not available yet."))
}
