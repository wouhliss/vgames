//! Cursor pagination (docs/architecture/03-api.md §3).
//!
//! A cursor is `base64url(json{k, id, f} || mac)`, where `k` is the sort key of the last
//! item, `id` its UUID (tie-breaker), `f` a hash of the filters it was built with, and
//! `mac` the first 16 bytes of HMAC-SHA-256 over the JSON with a key derived from
//! `VGAMES_SERVER_SECRET`. A tampered cursor, or one reused with different filters, is
//! `400 invalid_cursor`.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;
use uuid::Uuid;
use vgames_proto::FieldError;

use crate::{error::ApiError, http::json::Validate};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;
const MAX_CURSOR_LEN: usize = 512;
const MAC_LEN: usize = 16;

/// `limit` and `cursor` query parameters (flatten into endpoint query structs).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PageParams {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl PageParams {
    pub fn limit(&self) -> u32 {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }
}

impl Validate for PageParams {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if let Some(l) = self.limit
            && !(1..=MAX_LIMIT).contains(&l)
        {
            crate::http::json::invalid(
                errors,
                "limit",
                "out_of_range",
                format!("must be 1..={MAX_LIMIT}"),
            );
        }
        if let Some(c) = &self.cursor
            && (c.is_empty() || c.len() > MAX_CURSOR_LEN)
        {
            crate::http::json::invalid(errors, "cursor", "invalid", "is not a valid cursor");
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Payload<K> {
    k: K,
    id: Uuid,
    f: String,
}

/// Encodes and decodes cursors with a secret key.
pub struct CursorCodec<'a> {
    key: &'a [u8; 32],
}

impl<'a> CursorCodec<'a> {
    pub fn new(key: &'a [u8; 32]) -> Self {
        Self { key }
    }

    fn mac(&self, data: &[u8]) -> Result<[u8; MAC_LEN], ApiError> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key).map_err(ApiError::internal_from)?;
        mac.update(data);
        let full = mac.finalize().into_bytes();
        let mut out = [0u8; MAC_LEN];
        out.copy_from_slice(full.get(..MAC_LEN).ok_or_else(ApiError::internal)?);
        Ok(out)
    }

    /// Builds the cursor that resumes *after* the item with `sort_key`/`id`.
    pub fn encode<K: Serialize, F: Serialize>(
        &self,
        sort_key: &K,
        id: Uuid,
        filters: &F,
    ) -> Result<String, ApiError> {
        let payload = Payload {
            k: sort_key,
            id,
            f: filter_hash(filters)?,
        };
        let mut bytes = serde_json::to_vec(&payload).map_err(ApiError::internal_from)?;
        let mac = self.mac(&bytes)?;
        bytes.extend_from_slice(&mac);
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Verifies a cursor and returns `(sort_key, id)`.
    pub fn decode<K: DeserializeOwned, F: Serialize>(
        &self,
        cursor: &str,
        filters: &F,
    ) -> Result<(K, Uuid), ApiError> {
        let invalid = || {
            ApiError::bad_request(
                "invalid_cursor",
                "The cursor is invalid or belongs to a different query",
            )
        };
        if cursor.len() > MAX_CURSOR_LEN {
            return Err(invalid());
        }
        let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
        if bytes.len() <= MAC_LEN {
            return Err(invalid());
        }
        let (data, tag) = bytes.split_at(bytes.len() - MAC_LEN);
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key).map_err(ApiError::internal_from)?;
        mac.update(data);
        mac.verify_truncated_left(tag).map_err(|_| invalid())?;
        let payload: Payload<K> = serde_json::from_slice(data).map_err(|_| invalid())?;
        if payload.f != filter_hash(filters)? {
            return Err(invalid());
        }
        Ok((payload.k, payload.id))
    }
}

fn filter_hash<F: Serialize>(filters: &F) -> Result<String, ApiError> {
    let json = serde_json::to_vec(filters).map_err(ApiError::internal_from)?;
    let hash = blake3::hash(&json);
    Ok(hex::encode(hash.as_bytes().get(..8).unwrap_or_default()))
}

/// Splits a `limit + 1` fetch into the page and the cursor for the next one.
pub fn finish_page<T>(
    mut rows: Vec<T>,
    limit: u32,
    cursor_of: impl Fn(&T) -> Result<String, ApiError>,
) -> Result<vgames_proto::common::Page<T>, ApiError> {
    let limit = limit as usize;
    let next_cursor = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(&cursor_of).transpose()?
    } else {
        None
    };
    Ok(vgames_proto::common::Page {
        items: rows,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const KEY: [u8; 32] = [7; 32];

    proptest! {
        /// Cursors round-trip for any sort key, id and filters.
        #[test]
        fn cursors_round_trip(key in any::<String>(), id in any::<u128>(), q in proptest::option::of(any::<String>())) {
            let codec = CursorCodec::new(&KEY);
            let f = serde_json::json!({ "q": q });
            let id = Uuid::from_u128(id);
            let c = codec.encode(&key, id, &f).unwrap();
            if c.len() <= MAX_CURSOR_LEN {
                let (k, got): (String, Uuid) = codec.decode(&c, &f).unwrap();
                prop_assert_eq!((k, got), (key, id));
            }
        }

        /// Arbitrary input never panics and is (practically) never accepted.
        #[test]
        fn arbitrary_cursors_are_rejected(input in any::<String>()) {
            let codec = CursorCodec::new(&KEY);
            let res = codec.decode::<String, _>(&input, &serde_json::json!({}));
            prop_assert_eq!(res.unwrap_err().code, "invalid_cursor");
        }

        /// Flipping any bit of a valid cursor invalidates it.
        #[test]
        fn tampered_cursors_are_rejected(key in "[a-z]{0,40}", byte in any::<prop::sample::Index>(), bit in 0u8..8) {
            let codec = CursorCodec::new(&KEY);
            let f = serde_json::json!({});
            let c = codec.encode(&key, Uuid::from_u128(7), &f).unwrap();
            let mut raw = URL_SAFE_NO_PAD.decode(&c).unwrap();
            let i = byte.index(raw.len());
            raw[i] ^= 1 << bit;
            let tampered = URL_SAFE_NO_PAD.encode(raw);
            prop_assert_eq!(codec.decode::<String, _>(&tampered, &f).unwrap_err().code, "invalid_cursor");
        }
    }

    #[derive(Serialize)]
    struct Filters<'a> {
        q: Option<&'a str>,
        sort: &'a str,
    }

    #[test]
    fn round_trips() {
        let codec = CursorCodec::new(&KEY);
        let f = Filters {
            q: Some("portal"),
            sort: "title",
        };
        let id = Uuid::now_v7();
        let c = codec.encode(&"Portal 2", id, &f).unwrap();
        let (k, got): (String, Uuid) = codec.decode(&c, &f).unwrap();
        assert_eq!((k.as_str(), got), ("Portal 2", id));
    }

    #[test]
    fn rejects_tampering_other_filters_and_other_keys() {
        let codec = CursorCodec::new(&KEY);
        let f = Filters {
            q: None,
            sort: "title",
        };
        let c = codec.encode(&"a", Uuid::now_v7(), &f).unwrap();

        let mut bytes = URL_SAFE_NO_PAD.decode(&c).unwrap();
        bytes[3] ^= 1;
        let tampered = URL_SAFE_NO_PAD.encode(bytes);
        assert_eq!(
            codec.decode::<String, _>(&tampered, &f).unwrap_err().code,
            "invalid_cursor"
        );

        let other = Filters {
            q: Some("x"),
            sort: "title",
        };
        assert_eq!(
            codec.decode::<String, _>(&c, &other).unwrap_err().code,
            "invalid_cursor"
        );

        let other_key = [8u8; 32];
        assert_eq!(
            CursorCodec::new(&other_key)
                .decode::<String, _>(&c, &f)
                .unwrap_err()
                .code,
            "invalid_cursor"
        );

        for junk in ["", "!!!", "AAAA", &"A".repeat(600)] {
            assert_eq!(
                codec.decode::<String, _>(junk, &f).unwrap_err().code,
                "invalid_cursor"
            );
        }
    }

    #[test]
    fn finish_page_emits_cursor_only_when_more_rows_exist() {
        let page = finish_page(vec![1, 2, 3], 2, |n| Ok(n.to_string())).unwrap();
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(page.next_cursor.as_deref(), Some("2"));
        let page = finish_page(vec![1, 2], 2, |n| Ok(n.to_string())).unwrap();
        assert_eq!(page.next_cursor, None);
    }
}
