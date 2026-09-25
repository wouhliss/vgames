//! Conversations and the ciphertext relay (05-social §4.1, §4.4, A4-T05).
//!
//! The server only ever handles Olm ciphertext and metadata. Rules:
//! - A direct conversation exists once per pair (`direct_key`); only accepted friends
//!   without a block can open one. A party has the creator plus 1–15 accepted friends.
//! - Sending needs a registered device and current membership. Each envelope goes to a
//!   non-revoked device of a member, or to another device of the sender; members on either
//!   side of a block with the sender are not addressable. ≤ 64 envelopes of ≤ 64 KiB each;
//!   120 sends per minute per device; idempotent on `(sender device, client_message_id,
//!   recipient device)`. `unknown_devices` lists addressable devices the send left out.
//! - A device reads and acknowledges only its own inbox. Undelivered envelopes expire
//!   after 30 days (`social.sweep`).

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    realtime::{InboxNew, kinds},
    social::{
        Conversation, ConversationCreate, ConversationKind, ConversationPage, InboxAck,
        InboxEnvelope, InboxPage, MAX_ENVELOPE_CIPHERTEXT, MAX_ENVELOPES_PER_SEND,
        MAX_PARTY_MEMBERS, SendMessageRequest, SendMessageResponse,
    },
};

use super::{events, relations};
use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    http::{
        json::{Json, JsonResponse, Validate, invalid},
        pagination::{CursorCodec, DEFAULT_LIMIT, PageParams, finish_page},
        query::Query,
        ratelimit::Policy,
    },
    openapi_problems::{
        BadRequest, Forbidden, NotFound, PayloadTooLarge, TooManyRequests, Unauthorized,
    },
    state::AppState,
};

/// 64 envelopes of 64 KiB in base64, plus JSON overhead.
const SEND_BODY_LIMIT: usize = 6 * 1024 * 1024;
/// Most ids per `POST /v1/inbox/ack`.
pub const MAX_ACK_IDS: usize = 500;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn routes() -> OpenApiRouter<AppState> {
    let send = OpenApiRouter::new()
        .routes(routes!(send_message))
        .layer(DefaultBodyLimit::max(SEND_BODY_LIMIT));
    OpenApiRouter::new()
        .routes(routes!(list_conversations, create_conversation))
        .routes(routes!(get_inbox))
        .routes(routes!(ack_inbox))
        .merge(send)
}

fn device_required() -> ApiError {
    ApiError::forbidden_code("device_required", "Register this launcher's device first")
}

fn not_allowed() -> ApiError {
    ApiError::forbidden_code(
        "not_allowed",
        "You can only start conversations with friends",
    )
}

// ---------------------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------------------

impl Validate for ConversationCreate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if let ConversationCreate::Party { user_ids } = self {
            if user_ids.is_empty() || user_ids.len() >= MAX_PARTY_MEMBERS {
                invalid(errors, "user_ids", "length", "1 to 15 other members");
            }
            if user_ids.iter().collect::<HashSet<_>>().len() != user_ids.len() {
                invalid(errors, "user_ids", "unique", "members must be unique");
            }
        }
    }
}

async fn load_conversations(
    conn: &mut sqlx::PgConnection,
    state: &AppState,
    ids: &[Uuid],
) -> ApiResult<Vec<Conversation>> {
    let rows = sqlx::query!(
        "SELECT id, kind, created_at, last_message_at FROM conversations WHERE id = ANY($1)",
        ids
    )
    .fetch_all(&mut *conn)
    .await?;
    let members = sqlx::query!(
        "SELECT conversation_id, user_id FROM conversation_members
         WHERE conversation_id = ANY($1) AND left_at IS NULL ORDER BY joined_at, user_id",
        ids
    )
    .fetch_all(&mut *conn)
    .await?;
    let user_ids: Vec<Uuid> = members.iter().map(|m| m.user_id).collect();
    let users = crate::packages::users_public(state, &user_ids).await?;
    let mut by_conv: HashMap<Uuid, Vec<_>> = HashMap::new();
    for m in members {
        if let Some(u) = users.get(&m.user_id) {
            by_conv
                .entry(m.conversation_id)
                .or_default()
                .push(u.clone());
        }
    }
    let mut out: HashMap<Uuid, Conversation> = rows
        .into_iter()
        .map(|r| {
            (
                r.id,
                Conversation {
                    id: r.id,
                    kind: if r.kind == "direct" {
                        ConversationKind::Direct
                    } else {
                        ConversationKind::Party
                    },
                    members: by_conv.remove(&r.id).unwrap_or_default(),
                    created_at: r.created_at,
                    last_activity_at: r.last_message_at,
                },
            )
        })
        .collect();
    Ok(ids.iter().filter_map(|id| out.remove(id)).collect())
}

/// Open a direct conversation (get or create) or create a party
#[utoipa::path(
    post,
    path = "/v1/conversations",
    tag = "messaging",
    operation_id = "createConversation",
    request_body = ConversationCreate,
    responses(
        (status = 200, description = "The existing direct conversation", body = Conversation),
        (status = 201, description = "Created", body = Conversation),
        BadRequest, Unauthorized, Forbidden
    )
)]
pub async fn create_conversation(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<ConversationCreate>,
) -> ApiResult<JsonResponse<Conversation>> {
    let me = user.user_id;
    let mut tx = state.db.begin().await?;
    let (id, created) = match body {
        ConversationCreate::Direct { user_id } => {
            if user_id == me {
                return Err(ApiError::field(
                    "user_id",
                    "self",
                    "you cannot message yourself",
                ));
            }
            if !relations::are_friends(&mut tx, me, user_id).await? {
                return Err(not_allowed());
            }
            let (low, high) = relations::pair(me, user_id);
            let key = format!("{low}:{high}");
            let inserted = sqlx::query_scalar!(
                "INSERT INTO conversations (kind, direct_key, created_by) VALUES ('direct', $1, $2)
                 ON CONFLICT (direct_key) DO NOTHING RETURNING id",
                key,
                me
            )
            .fetch_optional(&mut *tx)
            .await?;
            let (id, created) = match inserted {
                Some(id) => (id, true),
                None => (
                    sqlx::query_scalar!("SELECT id FROM conversations WHERE direct_key = $1", key)
                        .fetch_one(&mut *tx)
                        .await?,
                    false,
                ),
            };
            // Both are members again, whatever happened before.
            sqlx::query!(
                "INSERT INTO conversation_members (conversation_id, user_id, role) VALUES ($1, $2, 'member'), ($1, $3, 'member')
                 ON CONFLICT (conversation_id, user_id) DO UPDATE SET left_at = NULL",
                id,
                me,
                user_id
            )
            .execute(&mut *tx)
            .await?;
            (id, created)
        }
        ConversationCreate::Party { user_ids } => {
            if user_ids.contains(&me) {
                return Err(ApiError::field(
                    "user_ids",
                    "self",
                    "you are added automatically",
                ));
            }
            for u in &user_ids {
                if !relations::are_friends(&mut tx, me, *u).await? {
                    return Err(not_allowed());
                }
            }
            let id = sqlx::query_scalar!(
                "INSERT INTO conversations (kind, created_by) VALUES ('party', $1) RETURNING id",
                me
            )
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query!(
                "INSERT INTO conversation_members (conversation_id, user_id, role) VALUES ($1, $2, 'owner')",
                id,
                me
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                "INSERT INTO conversation_members (conversation_id, user_id, role) SELECT $1, unnest($2::uuid[]), 'member'",
                id,
                &user_ids
            )
            .execute(&mut *tx)
            .await?;
            (id, true)
        }
    };
    tx.commit().await?;
    let mut conn = state.db.acquire().await?;
    let conv = load_conversations(&mut conn, &state, &[id])
        .await?
        .pop()
        .ok_or_else(ApiError::not_found)?;
    Ok(JsonResponse(
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        conv,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl Validate for PageQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
        .validate(errors);
    }
}

/// Your conversations, most recently active first
#[utoipa::path(
    get,
    path = "/v1/conversations",
    tag = "messaging",
    operation_id = "listConversations",
    params(PageQuery),
    responses((status = 200, description = "Page of conversations", body = ConversationPage), Unauthorized)
)]
pub async fn list_conversations(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<PageQuery>,
) -> ApiResult<axum::Json<ConversationPage>> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let filters = json!({ "conversations": user.user_id });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<(OffsetDateTime, Uuid)> = q
        .cursor
        .as_deref()
        .map(|c| {
            codec.decode::<String, _>(c, &filters).and_then(|(k, id)| {
                OffsetDateTime::parse(&k, &time::format_description::well_known::Rfc3339)
                    .map(|t| (t, id))
                    .map_err(|_| ApiError::bad_request("invalid_cursor", "The cursor is invalid"))
            })
        })
        .transpose()?;
    let rows = sqlx::query!(
        r#"SELECT c.id, coalesce(c.last_message_at, c.created_at) AS "active!"
           FROM conversations c
           JOIN conversation_members m ON m.conversation_id = c.id AND m.user_id = $1 AND m.left_at IS NULL
           WHERE $2::timestamptz IS NULL OR (coalesce(c.last_message_at, c.created_at), c.id) < ($2, $3)
           ORDER BY coalesce(c.last_message_at, c.created_at) DESC, c.id DESC
           LIMIT $4"#,
        user.user_id,
        after.map(|a| a.0),
        after.map(|a| a.1),
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    let page = finish_page(rows, limit, |r| {
        let k = r
            .active
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(ApiError::internal_from)?;
        codec.encode(&k, r.id, &filters)
    })?;
    let ids: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
    let mut conn = state.db.acquire().await?;
    Ok(axum::Json(ConversationPage {
        items: load_conversations(&mut conn, &state, &ids).await?,
        next_cursor: page.next_cursor,
    }))
}

// ---------------------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------------------

impl Validate for SendMessageRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.envelopes.is_empty() || self.envelopes.len() > MAX_ENVELOPES_PER_SEND {
            invalid(errors, "envelopes", "length", "1 to 64 envelopes");
        }
        let mut seen = HashSet::new();
        for (i, e) in self.envelopes.iter().enumerate() {
            if e.olm_message_type > 1 {
                invalid(
                    errors,
                    &format!("envelopes[{i}].olm_message_type"),
                    "enum",
                    "0 or 1",
                );
            }
            if !seen.insert(e.recipient_device_id) {
                invalid(
                    errors,
                    &format!("envelopes[{i}].recipient_device_id"),
                    "duplicate",
                    "one envelope per device",
                );
            }
            if e.ciphertext.is_empty() {
                invalid(
                    errors,
                    &format!("envelopes[{i}].ciphertext"),
                    "empty",
                    "must not be empty",
                );
            }
        }
    }
}

/// Send ciphertext envelopes, one per recipient device
#[utoipa::path(
    post,
    path = "/v1/conversations/{conversation_id}/messages",
    tag = "messaging",
    operation_id = "sendMessage",
    params(("conversation_id" = Uuid, Path)),
    request_body = SendMessageRequest,
    responses(
        (status = 202, description = "Stored for delivery", body = SendMessageResponse),
        BadRequest, Unauthorized, Forbidden, NotFound, PayloadTooLarge, TooManyRequests
    )
)]
pub async fn send_message(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(conversation_id): Path<Uuid>,
    Json(body): Json<SendMessageRequest>,
) -> ApiResult<JsonResponse<SendMessageResponse>> {
    let sender_device = user.device_id.ok_or_else(device_required)?;
    state
        .limits
        .check(Policy::Messages, &format!("send:{sender_device}"))?;
    // Decode and size-check before touching the database.
    let mut ciphertexts = Vec::with_capacity(body.envelopes.len());
    for (i, e) in body.envelopes.iter().enumerate() {
        if e.ciphertext.len() > MAX_ENVELOPE_CIPHERTEXT.div_ceil(3) * 4 {
            return Err(ApiError::payload_too_large()
                .with_detail(format!("envelopes[{i}] is larger than 64 KiB")));
        }
        let bytes = B64.decode(&e.ciphertext).map_err(|_| {
            ApiError::field(
                format!("envelopes[{i}].ciphertext"),
                "format",
                "standard base64",
            )
        })?;
        if bytes.len() > MAX_ENVELOPE_CIPHERTEXT {
            return Err(ApiError::payload_too_large()
                .with_detail(format!("envelopes[{i}] is larger than 64 KiB")));
        }
        if bytes.is_empty() {
            return Err(ApiError::field(
                format!("envelopes[{i}].ciphertext"),
                "empty",
                "must not be empty",
            ));
        }
        ciphertexts.push(bytes);
    }

    let mut tx = state.db.begin().await?;
    let member = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM conversation_members
             WHERE conversation_id = $1 AND user_id = $2 AND left_at IS NULL) AS "e!""#,
        conversation_id,
        user.user_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if !member {
        return Err(ApiError::not_found());
    }
    // Addressable devices: every non-revoked device of a current member without a block
    // either way (the sender's own other devices included), minus the sending device.
    let addressable = sqlx::query!(
        r#"SELECT d.id, d.user_id FROM devices d
           JOIN conversation_members m ON m.user_id = d.user_id AND m.conversation_id = $1 AND m.left_at IS NULL
           WHERE d.revoked_at IS NULL AND d.identity_key IS NOT NULL AND d.id <> $3
             AND (d.user_id = $2 OR NOT EXISTS (
               SELECT 1 FROM user_blocks b
               WHERE (b.blocker_id = $2 AND b.blocked_id = d.user_id) OR (b.blocker_id = d.user_id AND b.blocked_id = $2)))"#,
        conversation_id,
        user.user_id,
        sender_device
    )
    .fetch_all(&mut *tx)
    .await?;
    let owner_of: HashMap<Uuid, Uuid> = addressable.iter().map(|d| (d.id, d.user_id)).collect();
    for (i, e) in body.envelopes.iter().enumerate() {
        if !owner_of.contains_key(&e.recipient_device_id) {
            return Err(ApiError::field(
                format!("envelopes[{i}].recipient_device_id"),
                "unknown_recipient",
                "not an active device of this conversation's members",
            ));
        }
    }
    // A direct conversation with a block is closed, like the other user never existed.
    if owner_of.values().all(|u| *u == user.user_id) {
        let others_blocked = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                 SELECT 1 FROM conversation_members m JOIN user_blocks b
                   ON (b.blocker_id = $2 AND b.blocked_id = m.user_id) OR (b.blocker_id = m.user_id AND b.blocked_id = $2)
                 WHERE m.conversation_id = $1 AND m.left_at IS NULL) AS "e!""#,
            conversation_id,
            user.user_id
        )
        .fetch_one(&mut *tx)
        .await?;
        if others_blocked {
            return Err(ApiError::not_found());
        }
    }

    let mut new_per_user: HashMap<Uuid, i64> = HashMap::new();
    for (e, bytes) in body.envelopes.iter().zip(&ciphertexts) {
        let inserted = sqlx::query_scalar!(
            r#"INSERT INTO message_envelopes
                 (conversation_id, sender_user_id, sender_device_id, recipient_device_id, client_message_id,
                  algorithm, olm_message_type, ciphertext)
               VALUES ($1, $2, $3, $4, $5, 'olm.v1', $6, $7)
               ON CONFLICT (sender_device_id, client_message_id, recipient_device_id) DO NOTHING
               RETURNING id"#,
            conversation_id,
            user.user_id,
            sender_device,
            e.recipient_device_id,
            body.client_message_id,
            i16::from(e.olm_message_type),
            bytes
        )
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_some()
            && let Some(owner) = owner_of.get(&e.recipient_device_id)
        {
            *new_per_user.entry(*owner).or_default() += 1;
        }
    }
    // Devices already served by an earlier attempt of this message count as addressed.
    let served: HashSet<Uuid> = sqlx::query_scalar!(
        "SELECT recipient_device_id FROM message_envelopes WHERE sender_device_id = $1 AND client_message_id = $2",
        sender_device,
        body.client_message_id
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let mut unknown_devices: Vec<Uuid> = owner_of
        .keys()
        .filter(|d| !served.contains(d))
        .copied()
        .collect();
    unknown_devices.sort();
    if !new_per_user.is_empty() {
        sqlx::query!(
            "UPDATE conversations SET last_message_at = now() WHERE id = $1",
            conversation_id
        )
        .execute(&mut *tx)
        .await?;
    }
    for (recipient, count) in &new_per_user {
        events::publish(
            &mut tx,
            &[*recipient],
            kinds::INBOX_NEW,
            InboxNew {
                conversation_id,
                count: *count,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(JsonResponse(
        StatusCode::ACCEPTED,
        SendMessageResponse {
            accepted: i64::try_from(body.envelopes.len()).unwrap_or(i64::MAX),
            unknown_devices,
        },
    ))
}

// ---------------------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------------------

/// Envelopes for this device, oldest first
#[utoipa::path(
    get,
    path = "/v1/inbox",
    tag = "messaging",
    operation_id = "getInbox",
    params(PageQuery),
    responses((status = 200, description = "Page of envelopes", body = InboxPage), Unauthorized, Forbidden)
)]
pub async fn get_inbox(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<PageQuery>,
) -> ApiResult<axum::Json<InboxPage>> {
    let device = user.device_id.ok_or_else(device_required)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    let filters = json!({ "inbox": device });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode::<String, _>(c, &filters))
        .transpose()?
        .map(|(_, id)| id);
    let rows = sqlx::query!(
        r#"SELECT e.id, e.conversation_id, e.sender_user_id, e.sender_device_id, e.olm_message_type,
                  e.ciphertext, e.created_at, d.identity_key AS "sender_identity_key!"
           FROM message_envelopes e JOIN devices d ON d.id = e.sender_device_id
           WHERE e.recipient_device_id = $1 AND e.expires_at > now() AND d.identity_key IS NOT NULL
             AND ($2::uuid IS NULL OR e.id > $2)
           ORDER BY e.id
           LIMIT $3"#,
        device,
        after,
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    sqlx::query!(
        "UPDATE devices SET last_seen_at = now() WHERE id = $1",
        device
    )
    .execute(&state.db)
    .await?;
    let page = finish_page(rows, limit, |r| {
        codec.encode(&String::new(), r.id, &filters)
    })?;
    Ok(axum::Json(InboxPage {
        items: page
            .items
            .into_iter()
            .map(|r| InboxEnvelope {
                id: r.id,
                conversation_id: r.conversation_id,
                sender_user_id: r.sender_user_id,
                sender_device_id: r.sender_device_id,
                sender_identity_key: r.sender_identity_key,
                algorithm: "olm.v1".into(),
                olm_message_type: u8::try_from(r.olm_message_type).unwrap_or(1),
                ciphertext: B64.encode(&r.ciphertext),
                created_at: r.created_at,
            })
            .collect(),
        next_cursor: page.next_cursor,
    }))
}

impl Validate for InboxAck {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.ids.is_empty() || self.ids.len() > MAX_ACK_IDS {
            invalid(errors, "ids", "length", "1 to 500 ids");
        }
    }
}

/// Acknowledge (delete) delivered envelopes
#[utoipa::path(
    post,
    path = "/v1/inbox/ack",
    tag = "messaging",
    operation_id = "ackInbox",
    request_body = InboxAck,
    responses((status = 204, description = "Deleted"), BadRequest, Unauthorized)
)]
pub async fn ack_inbox(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<InboxAck>,
) -> ApiResult<StatusCode> {
    let Some(device) = user.device_id else {
        return Err(ApiError::bad_request(
            "device_required",
            "Register this launcher's device first",
        ));
    };
    sqlx::query!(
        "DELETE FROM message_envelopes WHERE id = ANY($1) AND recipient_device_id = $2",
        &body.ids,
        device
    )
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
