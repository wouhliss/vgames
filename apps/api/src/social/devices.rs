//! E2EE devices and the key directory (05-social §4.1, 05-social-notes §2.1, A4-T04).
//!
//! - `POST /v1/devices` registers the launcher's Olm account keys and binds the calling
//!   desktop session to that device. The self-signature over the canonical JSON (bound to
//!   this user and this server) is checked with strict Ed25519 before anything is stored.
//!   Signing in again with the same account binds the new session to the existing device.
//!   Keys are never reused: keys of a revoked device, or another user's keys, are refused.
//! - One-time keys and the fallback key are signed by the device's signing key; each
//!   signature is checked. At most [`MAX_UNCLAIMED_ONE_TIME_KEYS`] stay unclaimed.
//! - `POST /v1/keys/claim` hands out each one-time key once (`FOR UPDATE SKIP LOCKED`), and
//!   the fallback key when none is left, only for the caller's own other devices and devices
//!   of accepted friends or conversation co-members. Other devices are silently left out.
//! - Revoking a device deletes its unclaimed keys, revokes the sessions bound to it, and
//!   tells contacts with `device.revoked`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
};
use base64::Engine as _;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    auth::ClientKind,
    realtime::{DeviceEvent, kinds},
    social::{
        ClaimKeysRequest, ClaimedKey, ClaimedKeyList, Device, DeviceKeys, DeviceKeysList,
        DeviceList, DeviceRegister, MAX_CLAIM_DEVICES, MAX_UNCLAIMED_ONE_TIME_KEYS,
        OneTimeKeysStored, OneTimeKeysUpload, OsFamily, PUBLIC_KEY_B64_LEN, SIGNATURE_B64_LEN,
        SignedOneTimeKey, canonical,
    },
};

use super::{events, relations};
use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    http::{
        json::{Json, JsonResponse, Validate, invalid},
        ratelimit::Policy,
    },
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, TooManyRequests, Unauthorized},
    state::AppState,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_my_devices, register_device))
        .routes(routes!(revoke_device))
        .routes(routes!(upload_one_time_keys))
        .routes(routes!(claim_one_time_keys))
        .routes(routes!(list_user_devices))
}

// ---------------------------------------------------------------------------------------
// Validation and signatures
// ---------------------------------------------------------------------------------------

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD_NO_PAD;

fn decode_fixed<const N: usize>(s: &str, b64_len: usize) -> Option<[u8; N]> {
    if s.len() != b64_len {
        return None;
    }
    B64.decode(s).ok()?.try_into().ok()
}

/// Strict Ed25519 verification of `message` by `signing_key` (both unpadded base64).
pub fn verify_signature(signing_key: &str, message: &str, signature: &str) -> bool {
    let (Some(key), Some(sig)) = (
        decode_fixed::<32>(signing_key, PUBLIC_KEY_B64_LEN),
        decode_fixed::<64>(signature, SIGNATURE_B64_LEN),
    ) else {
        return false;
    };
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&key) else {
        return false;
    };
    key.verify_strict(
        message.as_bytes(),
        &ed25519_dalek::Signature::from_bytes(&sig),
    )
    .is_ok()
}

fn is_key_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

fn bad_signature(field: &str) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "bad_signature",
        "A signature does not verify",
    )
    .with_errors(vec![FieldError {
        field: field.to_string(),
        code: "bad_signature".into(),
        message: Some("does not verify against the device's signing key".into()),
    }])
}

impl Validate for DeviceRegister {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        let name = self.display_name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            invalid(
                errors,
                "display_name",
                "length",
                "1 to 64 printable characters",
            );
        }
        if decode_fixed::<32>(&self.identity_key, PUBLIC_KEY_B64_LEN).is_none() {
            invalid(
                errors,
                "identity_key",
                "format",
                "32 bytes, unpadded base64",
            );
        }
        if decode_fixed::<32>(&self.signing_key, PUBLIC_KEY_B64_LEN).is_none() {
            invalid(errors, "signing_key", "format", "32 bytes, unpadded base64");
        }
        if decode_fixed::<64>(&self.keys_signature, SIGNATURE_B64_LEN).is_none() {
            invalid(
                errors,
                "keys_signature",
                "format",
                "64 bytes, unpadded base64",
            );
        }
    }
}

fn validate_otk(errors: &mut Vec<FieldError>, field: &str, key: &SignedOneTimeKey) {
    if !is_key_id(&key.key_id) {
        invalid(
            errors,
            &format!("{field}.key_id"),
            "format",
            "1 to 64 base64 characters",
        );
    }
    if decode_fixed::<32>(&key.public_key, PUBLIC_KEY_B64_LEN).is_none() {
        invalid(
            errors,
            &format!("{field}.public_key"),
            "format",
            "32 bytes, unpadded base64",
        );
    }
    if decode_fixed::<64>(&key.signature, SIGNATURE_B64_LEN).is_none() {
        invalid(
            errors,
            &format!("{field}.signature"),
            "format",
            "64 bytes, unpadded base64",
        );
    }
}

impl Validate for OneTimeKeysUpload {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.one_time_keys.len() > MAX_UNCLAIMED_ONE_TIME_KEYS {
            invalid(errors, "one_time_keys", "max_items", "at most 100 keys");
        }
        let mut seen = std::collections::HashSet::new();
        for (i, k) in self.one_time_keys.iter().enumerate() {
            validate_otk(errors, &format!("one_time_keys[{i}]"), k);
            if !seen.insert(k.key_id.as_str()) {
                invalid(
                    errors,
                    &format!("one_time_keys[{i}].key_id"),
                    "duplicate",
                    "key ids must be unique",
                );
            }
        }
        if let Some(f) = &self.fallback_key {
            validate_otk(errors, "fallback_key", f);
            if seen.contains(f.key_id.as_str()) {
                invalid(
                    errors,
                    "fallback_key.key_id",
                    "duplicate",
                    "key ids must be unique",
                );
            }
        }
    }
}

impl Validate for ClaimKeysRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.device_ids.is_empty() || self.device_ids.len() > MAX_CLAIM_DEVICES {
            invalid(errors, "device_ids", "length", "1 to 64 devices");
        }
        let unique: std::collections::HashSet<_> = self.device_ids.iter().collect();
        if unique.len() != self.device_ids.len() {
            invalid(errors, "device_ids", "unique", "device ids must be unique");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Own devices
// ---------------------------------------------------------------------------------------

async fn load_devices(
    state: &AppState,
    user: Uuid,
    only: Option<Uuid>,
    current: Option<Uuid>,
) -> ApiResult<Vec<Device>> {
    let rows = sqlx::query!(
        r#"SELECT d.id, d.display_name, d.platform, d.identity_key, d.signing_key, d.created_at, d.last_seen_at,
                  (SELECT count(*) FROM device_one_time_keys k
                    WHERE k.device_id = d.id AND NOT k.is_fallback AND k.claimed_at IS NULL) AS "available!",
                  EXISTS (SELECT 1 FROM device_one_time_keys k WHERE k.device_id = d.id AND k.is_fallback) AS "has_fallback!"
           FROM devices d
           WHERE d.user_id = $1 AND d.revoked_at IS NULL AND ($2::uuid IS NULL OR d.id = $2)
           ORDER BY d.created_at"#,
        user,
        only
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Device {
            id: r.id,
            display_name: r.display_name,
            platform: OsFamily::parse(&r.platform).unwrap_or(OsFamily::Linux),
            identity_key: r.identity_key,
            signing_key: r.signing_key,
            one_time_keys_available: Some(r.available),
            has_fallback_key: Some(r.has_fallback),
            current: Some(Some(r.id) == current),
            created_at: r.created_at,
            last_seen_at: r.last_seen_at,
        })
        .collect())
}

/// List own devices with key counts
#[utoipa::path(
    get,
    path = "/v1/devices",
    tag = "messaging",
    operation_id = "listMyDevices",
    responses((status = 200, description = "Own active devices", body = DeviceList), Unauthorized)
)]
pub async fn list_my_devices(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<axum::Json<DeviceList>> {
    Ok(axum::Json(DeviceList {
        items: load_devices(&state, user.user_id, None, user.device_id).await?,
    }))
}

/// Register this launcher's device and bind the session to it
#[utoipa::path(
    post,
    path = "/v1/devices",
    tag = "messaging",
    operation_id = "registerDevice",
    request_body = DeviceRegister,
    responses(
        (status = 201, description = "Registered (or re-bound to the same keys)", body = Device),
        BadRequest, Unauthorized, Conflict
    )
)]
pub async fn register_device(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<DeviceRegister>,
) -> ApiResult<JsonResponse<Device>> {
    if user.kind != ClientKind::Desktop {
        return Err(ApiError::bad_request(
            "desktop_session_required",
            "Only a launcher session can register a device",
        ));
    }
    let message = canonical::device_keys(
        &body.identity_key,
        state.config.server_id,
        &body.signing_key,
        user.user_id,
    );
    if !verify_signature(&body.signing_key, &message, &body.keys_signature) {
        return Err(bad_signature("keys_signature"));
    }
    let name = body.display_name.trim().to_string();
    let mut tx = state.db.begin().await?;
    let bound = sqlx::query_scalar!(
        "SELECT device_id FROM sessions WHERE id = $1 AND revoked_at IS NULL FOR UPDATE",
        user.session_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::unauthenticated)?;
    // Any device already holding one of these keys.
    let holder = sqlx::query!(
        "SELECT id, user_id, identity_key, signing_key, revoked_at FROM devices
         WHERE identity_key = $1 OR signing_key = $2 ORDER BY created_at LIMIT 1",
        body.identity_key,
        body.signing_key
    )
    .fetch_optional(&mut *tx)
    .await?;
    let same_account = holder.as_ref().is_some_and(|h| {
        h.user_id == user.user_id
            && h.revoked_at.is_none()
            && h.identity_key.as_deref() == Some(body.identity_key.as_str())
            && h.signing_key.as_deref() == Some(body.signing_key.as_str())
    });
    let (device_id, added) = match (bound, holder) {
        // Retrying the registration of this session's device.
        (Some(b), Some(h)) if same_account && h.id == b => (b, false),
        (Some(_), _) => {
            return Err(ApiError::conflict(
                "device_already_registered",
                "This session already belongs to another device",
            ));
        }
        // Signing in again with an existing account: bind the new session to it.
        (None, Some(h)) if same_account => (h.id, false),
        (None, Some(_)) => {
            return Err(ApiError::conflict(
                "device_keys_in_use",
                "These keys belong to another or a revoked device; create a new account",
            ));
        }
        (None, None) => {
            let id = sqlx::query_scalar!(
                "INSERT INTO devices (user_id, display_name, platform, identity_key, signing_key, keys_signature, last_seen_at)
                 VALUES ($1, $2, $3, $4, $5, $6, now()) RETURNING id",
                user.user_id,
                name,
                body.platform.as_str(),
                body.identity_key,
                body.signing_key,
                body.keys_signature
            )
            .fetch_one(&mut *tx)
            .await?;
            (id, true)
        }
    };
    sqlx::query!(
        "UPDATE devices SET display_name = $2, platform = $3, last_seen_at = now() WHERE id = $1",
        device_id,
        name,
        body.platform.as_str()
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE sessions SET device_id = $2, device_name = $3 WHERE id = $1",
        user.session_id,
        device_id,
        name
    )
    .execute(&mut *tx)
    .await?;
    if added {
        let audience = relations::contacts(&mut tx, user.user_id).await?;
        events::publish(
            &mut tx,
            &audience,
            kinds::DEVICE_ADDED,
            DeviceEvent {
                user_id: user.user_id,
                device_id,
            },
        )
        .await?;
    }
    tx.commit().await?;
    let device = load_devices(&state, user.user_id, Some(device_id), Some(device_id))
        .await?
        .pop()
        .ok_or_else(ApiError::not_found)?;
    Ok(JsonResponse(StatusCode::CREATED, device))
}

/// Revoke one of your devices
#[utoipa::path(
    delete,
    path = "/v1/devices/{device_id}",
    tag = "messaging",
    operation_id = "revokeDevice",
    params(("device_id" = Uuid, Path)),
    responses((status = 204, description = "Revoked"), Unauthorized, NotFound)
)]
pub async fn revoke_device(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(device_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut tx = state.db.begin().await?;
    let revoked = sqlx::query!(
        "UPDATE devices SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        device_id,
        user.user_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::not_found());
    }
    sqlx::query!(
        "DELETE FROM device_one_time_keys WHERE device_id = $1",
        device_id
    )
    .execute(&mut *tx)
    .await?;
    let sessions = sqlx::query_scalar!(
        "UPDATE sessions SET revoked_at = now(), revoked_reason = 'device_revoked'
         WHERE device_id = $1 AND revoked_at IS NULL RETURNING id",
        device_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let audience = relations::contacts(&mut tx, user.user_id).await?;
    events::publish(
        &mut tx,
        &audience,
        kinds::DEVICE_REVOKED,
        DeviceEvent {
            user_id: user.user_id,
            device_id,
        },
    )
    .await?;
    tx.commit().await?;
    if !sessions.is_empty() {
        crate::realtime::bus::revoke_sessions(&state, user.user_id, &sessions, "device_revoked")
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// One-time keys
// ---------------------------------------------------------------------------------------

async fn unclaimed_count(conn: &mut sqlx::PgConnection, device: Uuid) -> ApiResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM device_one_time_keys WHERE device_id = $1 AND NOT is_fallback AND claimed_at IS NULL"#,
        device
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// Upload signed one-time keys and a fallback key
#[utoipa::path(
    post,
    path = "/v1/devices/{device_id}/one-time-keys",
    tag = "messaging",
    operation_id = "uploadOneTimeKeys",
    params(("device_id" = Uuid, Path)),
    request_body = OneTimeKeysUpload,
    responses(
        (status = 200, description = "Unclaimed one-time keys now stored", body = OneTimeKeysStored),
        BadRequest, Unauthorized, Forbidden
    )
)]
pub async fn upload_one_time_keys(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(device_id): Path<Uuid>,
    Json(body): Json<OneTimeKeysUpload>,
) -> ApiResult<axum::Json<OneTimeKeysStored>> {
    // Only the device itself uploads its keys.
    if user.device_id != Some(device_id) {
        return Err(ApiError::forbidden());
    }
    let mut tx = state.db.begin().await?;
    let signing_key = sqlx::query_scalar!(
        "SELECT signing_key FROM devices WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL FOR UPDATE",
        device_id,
        user.user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten()
    .ok_or_else(ApiError::forbidden)?;
    for (i, k) in body.one_time_keys.iter().enumerate() {
        let message = canonical::one_time_key(false, &k.public_key, &k.key_id);
        if !verify_signature(&signing_key, &message, &k.signature) {
            return Err(bad_signature(&format!("one_time_keys[{i}].signature")));
        }
    }
    if let Some(f) = &body.fallback_key {
        let message = canonical::one_time_key(true, &f.public_key, &f.key_id);
        if !verify_signature(&signing_key, &message, &f.signature) {
            return Err(bad_signature("fallback_key.signature"));
        }
    }
    let mut stored = 0i64;
    for k in &body.one_time_keys {
        let row = sqlx::query!(
            r#"INSERT INTO device_one_time_keys (device_id, key_id, public_key, signature)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (device_id, key_id) DO UPDATE SET key_id = excluded.key_id
               RETURNING public_key, is_fallback, (xmax = 0) AS "inserted!""#,
            device_id,
            k.key_id,
            k.public_key,
            k.signature
        )
        .fetch_one(&mut *tx)
        .await?;
        if row.public_key != k.public_key || row.is_fallback {
            return Err(ApiError::field(
                "one_time_keys",
                "key_id_reused",
                "a key id was reused for another key",
            ));
        }
        stored += i64::from(row.inserted);
    }
    let available = unclaimed_count(&mut tx, device_id).await?;
    if stored > 0 && usize::try_from(available).unwrap_or(usize::MAX) > MAX_UNCLAIMED_ONE_TIME_KEYS
    {
        return Err(ApiError::field(
            "one_time_keys",
            "too_many_keys",
            "at most 100 one-time keys may be unclaimed",
        ));
    }
    if let Some(f) = &body.fallback_key {
        // The newest fallback key replaces the previous one.
        sqlx::query!(
            "DELETE FROM device_one_time_keys WHERE device_id = $1 AND is_fallback AND key_id <> $2",
            device_id,
            f.key_id
        )
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query!(
            r#"INSERT INTO device_one_time_keys (device_id, key_id, public_key, signature, is_fallback)
               VALUES ($1, $2, $3, $4, true)
               ON CONFLICT (device_id, key_id) DO UPDATE SET key_id = excluded.key_id
               RETURNING public_key, is_fallback"#,
            device_id,
            f.key_id,
            f.public_key,
            f.signature
        )
        .fetch_one(&mut *tx)
        .await?;
        if row.public_key != f.public_key || !row.is_fallback {
            return Err(ApiError::field(
                "fallback_key",
                "key_id_reused",
                "a key id was reused for another key",
            ));
        }
    }
    sqlx::query!(
        "UPDATE devices SET last_seen_at = now() WHERE id = $1",
        device_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(axum::Json(OneTimeKeysStored { available }))
}

/// Claims one key of `device` for `claimer`: an unclaimed one-time key, else the fallback key.
async fn claim_one(
    conn: &mut sqlx::PgConnection,
    device: Uuid,
    claimer: Uuid,
) -> ApiResult<Option<ClaimedKey>> {
    // A lock recheck can make the locked subquery come back empty while unclaimed keys
    // remain; retry a few times before settling for the fallback key.
    for _ in 0..8 {
        let claimed = sqlx::query!(
            "UPDATE device_one_time_keys SET claimed_at = now(), claimed_by_device = $2
             WHERE (device_id, key_id) = (
               SELECT device_id, key_id FROM device_one_time_keys
               WHERE device_id = $1 AND NOT is_fallback AND claimed_at IS NULL
               ORDER BY created_at, key_id LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING key_id, public_key, signature",
            device,
            claimer
        )
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(k) = claimed {
            return Ok(Some(ClaimedKey {
                device_id: device,
                key_id: k.key_id,
                public_key: k.public_key,
                signature: k.signature,
                is_fallback: false,
            }));
        }
        if unclaimed_count(conn, device).await? == 0 {
            break;
        }
    }
    let fallback = sqlx::query!(
        "UPDATE device_one_time_keys SET claimed_at = now(), claimed_by_device = $2
         WHERE device_id = $1 AND is_fallback
         RETURNING key_id, public_key, signature",
        device,
        claimer
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(fallback.map(|k| ClaimedKey {
        device_id: device,
        key_id: k.key_id,
        public_key: k.public_key,
        signature: k.signature,
        is_fallback: true,
    }))
}

/// Claim one one-time key per target device
#[utoipa::path(
    post,
    path = "/v1/keys/claim",
    tag = "messaging",
    operation_id = "claimOneTimeKeys",
    request_body = ClaimKeysRequest,
    responses(
        (status = 200, description = "One key per claimable device", body = ClaimedKeyList),
        BadRequest, Unauthorized, TooManyRequests
    )
)]
pub async fn claim_one_time_keys(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<ClaimKeysRequest>,
) -> ApiResult<axum::Json<ClaimedKeyList>> {
    let Some(claimer) = user.device_id else {
        return Err(ApiError::bad_request(
            "device_required",
            "Register this launcher's device before claiming keys",
        ));
    };
    state
        .limits
        .check(Policy::Messages, &format!("keys-claim:{claimer}"))?;
    let mut tx = state.db.begin().await?;
    let owners = sqlx::query!(
        "SELECT id, user_id FROM devices WHERE id = ANY($1) AND revoked_at IS NULL AND identity_key IS NOT NULL",
        &body.device_ids
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut allowed_users = std::collections::HashMap::new();
    let mut items = Vec::new();
    for id in &body.device_ids {
        let Some(owner) = owners.iter().find(|o| o.id == *id).map(|o| o.user_id) else {
            continue;
        };
        if *id == claimer {
            continue;
        }
        let allowed = match allowed_users.get(&owner) {
            Some(a) => *a,
            None => {
                let a = relations::may_message(&mut tx, user.user_id, owner).await?;
                allowed_users.insert(owner, a);
                a
            }
        };
        if allowed && let Some(k) = claim_one(&mut tx, *id, claimer).await? {
            items.push(k);
        }
    }
    tx.commit().await?;
    Ok(axum::Json(ClaimedKeyList { items }))
}

/// A related user's device public keys
#[utoipa::path(
    get,
    path = "/v1/users/{user_id}/devices",
    tag = "messaging",
    operation_id = "listUserDevices",
    params(("user_id" = Uuid, Path)),
    responses((status = 200, description = "Active devices", body = DeviceKeysList), Unauthorized, NotFound)
)]
pub async fn list_user_devices(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(other): Path<Uuid>,
) -> ApiResult<axum::Json<DeviceKeysList>> {
    let mut conn = state.db.acquire().await?;
    if !relations::can_view(&mut conn, user.user_id, other).await? {
        return Err(ApiError::not_found());
    }
    let rows = sqlx::query!(
        r#"SELECT id, display_name, identity_key AS "identity_key!", signing_key AS "signing_key!",
                  keys_signature AS "keys_signature!", created_at
           FROM devices
           WHERE user_id = $1 AND revoked_at IS NULL AND identity_key IS NOT NULL
           ORDER BY created_at"#,
        other
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(axum::Json(DeviceKeysList {
        items: rows
            .into_iter()
            .map(|r| DeviceKeys {
                device_id: r.id,
                display_name: Some(r.display_name),
                identity_key: r.identity_key,
                signing_key: r.signing_key,
                keys_signature: r.keys_signature,
                created_at: r.created_at,
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ids_are_base64_characters() {
        assert!(is_key_id("AAAAAAAAAAE"));
        assert!(is_key_id("a+b/9"));
        assert!(!is_key_id(""));
        assert!(!is_key_id(&"A".repeat(65)));
        assert!(!is_key_id("a b"));
        assert!(!is_key_id("é"));
    }

    #[test]
    fn signatures_are_checked_strictly_and_never_panic() {
        use ed25519_dalek::Signer as _;
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let pk = B64.encode(sk.verifying_key().as_bytes());
        let sig = B64.encode(sk.sign(b"hello").to_bytes());
        assert!(verify_signature(&pk, "hello", &sig));
        assert!(!verify_signature(&pk, "hellO", &sig));
        let mut flipped = sk.sign(b"hello").to_bytes();
        flipped[10] ^= 1;
        assert!(!verify_signature(&pk, "hello", &B64.encode(flipped)));
        // Non-canonical base64 (trailing bits set) is refused before verification.
        let last = sig.chars().last().unwrap();
        let noncanonical = format!("{}{}", &sig[..85], if last == 'B' { 'C' } else { 'B' });
        assert!(!verify_signature(&pk, "hello", &noncanonical));
        assert!(!verify_signature("short", "hello", &sig));
        assert!(!verify_signature(&pk, "hello", "é".repeat(43).as_str()));
    }
}
