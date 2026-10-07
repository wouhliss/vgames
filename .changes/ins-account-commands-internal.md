---
audience: internal
component: launcher
type: added
---
INS-05: `account_sessions` and `account_session_revoke` (revoking this launcher's own session forgets it locally, without calling logout); `ApiError::Problem` carries the parsed problem body (`title`, `detail`, field `errors`, `Retry-After`) and `ApiClient::with_auth` is the reusable "401 → refresh once → retry once" hook for any request (for GAME-01's move of `social/api.rs`); Settings → Account uses the generated `AccountSession` and `auth_token_storage`, and the pending `account_*` and `credential_storage` entries are deleted.
