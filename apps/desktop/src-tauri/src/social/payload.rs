//! The plaintext inside each Olm message (05-social §4.1, 05-social-notes §2.3). It exists
//! only in the launcher; the server relays it encrypted.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;
use vgames_proto::social::is_valid_join_secret;

pub const PAYLOAD_VERSION: u8 = 1;
/// Longest text message, in characters.
pub const MAX_TEXT_CHARS: usize = 4_000;
/// Largest plaintext before encryption (the envelope limit is 64 KiB of ciphertext).
pub const MAX_PAYLOAD_BYTES: usize = 60 * 1024;

/// Message types the launcher understands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { body: String },
    #[serde(rename = "invite.join")]
    InviteJoin {
        invite_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        join_secret: Option<String>,
    },
    #[serde(rename = "receipt.read")]
    ReceiptRead { up_to: Uuid },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u8,
    pub conversation_id: Uuid,
    pub client_message_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
    #[serde(flatten)]
    pub content: Content,
}

/// What a received plaintext turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    Known(Payload),
    /// Valid header, but a type or field this launcher does not interpret. Shown as
    /// "unsupported message", never acted on.
    Unsupported {
        conversation_id: Uuid,
        client_message_id: Uuid,
        sent_at: OffsetDateTime,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    #[error("the message is too long")]
    TooLong,
    #[error("the message is empty")]
    Empty,
    #[error("the join secret does not match the allowed grammar")]
    BadJoinSecret,
    #[error("the plaintext is not a vgames payload")]
    Malformed,
}

impl Payload {
    pub fn text(
        conversation_id: Uuid,
        client_message_id: Uuid,
        sent_at: OffsetDateTime,
        body: &str,
    ) -> Result<Self, PayloadError> {
        let body = body.trim_end();
        if body.trim().is_empty() {
            return Err(PayloadError::Empty);
        }
        if body.chars().count() > MAX_TEXT_CHARS {
            return Err(PayloadError::TooLong);
        }
        Ok(Self {
            v: PAYLOAD_VERSION,
            conversation_id,
            client_message_id,
            sent_at,
            content: Content::Text {
                body: body.to_owned(),
            },
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, PayloadError> {
        let bytes = serde_json::to_vec(self).map_err(|_| PayloadError::Malformed)?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(PayloadError::TooLong);
        }
        Ok(bytes)
    }
}

#[derive(Deserialize)]
struct Header {
    v: u8,
    conversation_id: Uuid,
    client_message_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    sent_at: OffsetDateTime,
}

/// Parses a decrypted plaintext. Only a malformed header is an error; anything else that
/// is not understood becomes [`Decoded::Unsupported`].
pub fn decode(bytes: &[u8]) -> Result<Decoded, PayloadError> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(PayloadError::Malformed);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| PayloadError::Malformed)?;
    let header: Header =
        serde_json::from_value(value.clone()).map_err(|_| PayloadError::Malformed)?;
    let unsupported = || Decoded::Unsupported {
        conversation_id: header.conversation_id,
        client_message_id: header.client_message_id,
        sent_at: header.sent_at,
    };
    if header.v != PAYLOAD_VERSION {
        return Ok(unsupported());
    }
    let Ok(payload) = serde_json::from_value::<Payload>(value) else {
        return Ok(unsupported());
    };
    let valid = match &payload.content {
        Content::Text { body } => !body.trim().is_empty() && body.chars().count() <= MAX_TEXT_CHARS,
        Content::InviteJoin { join_secret, .. } => {
            join_secret.as_deref().is_none_or(is_valid_join_secret)
        }
        Content::ReceiptRead { .. } => true,
    };
    Ok(if valid {
        Decoded::Known(payload)
    } else {
        unsupported()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (Uuid, Uuid) {
        (Uuid::now_v7(), Uuid::now_v7())
    }

    #[test]
    fn text_round_trip_and_wire_shape() {
        let (c, m) = ids();
        let p = Payload::text(c, m, OffsetDateTime::UNIX_EPOCH, "gg  \n").unwrap();
        let bytes = p.encode().unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "text");
        assert_eq!(v["v"], 1);
        assert_eq!(v["body"], "gg");
        assert_eq!(v["sent_at"], "1970-01-01T00:00:00Z");
        assert_eq!(decode(&bytes).unwrap(), Decoded::Known(p));
    }

    #[test]
    fn limits() {
        let (c, m) = ids();
        assert!(matches!(
            Payload::text(c, m, OffsetDateTime::UNIX_EPOCH, "   "),
            Err(PayloadError::Empty)
        ));
        assert!(Payload::text(c, m, OffsetDateTime::UNIX_EPOCH, &"é".repeat(4000)).is_ok());
        assert!(matches!(
            Payload::text(c, m, OffsetDateTime::UNIX_EPOCH, &"é".repeat(4001)),
            Err(PayloadError::TooLong)
        ));
    }

    #[test]
    fn unknown_or_invalid_content_is_unsupported_never_interpreted() {
        let (c, m) = ids();
        let base = |extra: &str| {
            format!(
                r#"{{"v":1,"conversation_id":"{c}","client_message_id":"{m}","sent_at":"2026-09-24T10:00:00Z"{extra}}}"#
            )
        };
        for extra in [
            r#","type":"file","url":"https://x""#,
            r#","type":"invite.join","invite_id":"not-a-uuid""#,
            r#","type":"invite.join","invite_id":"01920000-0000-7000-8000-000000000001","join_secret":"a b; rm -rf""#,
            r#","type":"text","body":"""#,
            r#""#,
        ] {
            assert!(
                matches!(
                    decode(base(extra).as_bytes()).unwrap(),
                    Decoded::Unsupported { .. }
                ),
                "{extra}"
            );
        }
        let future = format!(
            r#"{{"v":2,"conversation_id":"{c}","client_message_id":"{m}","sent_at":"2026-09-24T10:00:00Z","type":"text","body":"x"}}"#
        );
        assert!(matches!(
            decode(future.as_bytes()).unwrap(),
            Decoded::Unsupported { .. }
        ));
        assert!(decode(b"not json").is_err());
        assert!(decode(br#"{"type":"text","body":"no header"}"#).is_err());
    }

    #[test]
    fn invite_join_with_valid_secret() {
        let (c, m) = ids();
        let p = Payload {
            v: 1,
            conversation_id: c,
            client_message_id: m,
            sent_at: OffsetDateTime::UNIX_EPOCH,
            content: Content::InviteJoin {
                invite_id: Uuid::now_v7(),
                join_secret: Some("10.0.0.2:7777".into()),
            },
        };
        assert_eq!(decode(&p.encode().unwrap()).unwrap(), Decoded::Known(p));
    }
}

/// No-panic properties (01-security §9): decrypted plaintext comes from other people.
#[cfg(test)]
mod no_panic {
    use proptest::prelude::*;

    use super::*;

    fn valid() -> Vec<u8> {
        Payload {
            v: 1,
            conversation_id: Uuid::from_u128(1),
            client_message_id: Uuid::from_u128(2),
            sent_at: OffsetDateTime::UNIX_EPOCH,
            content: Content::InviteJoin {
                invite_id: Uuid::from_u128(3),
                join_secret: Some("10.0.0.1:7777".into()),
            },
        }
        .encode()
        .unwrap()
    }

    proptest! {
        #[test]
        fn arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let _ = decode(&bytes);
        }

        #[test]
        fn mutated_payloads(edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..24)) {
            let mut bytes = valid();
            for (i, v) in edits {
                let at = i.index(bytes.len());
                bytes[at] = v;
            }
            if let Ok(Decoded::Known(p)) = decode(&bytes) {
                // Whatever still parses as known content obeys the rules.
                if let Content::InviteJoin { join_secret: Some(s), .. } = &p.content {
                    prop_assert!(is_valid_join_secret(s));
                }
            }
        }

        #[test]
        fn oversized_input_is_refused(extra in 0usize..64) {
            let bytes = vec![b' '; MAX_PAYLOAD_BYTES + 1 + extra];
            prop_assert!(decode(&bytes).is_err());
        }
    }
}
