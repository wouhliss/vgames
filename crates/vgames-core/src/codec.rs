//! Strict wire encodings shared by every format: BLAKE3 digests (lowercase hex),
//! base64 (RFC 4648 standard alphabet, padded, canonical) and RFC 3339 UTC
//! timestamps.
//!
//! Parsers here accept exactly one spelling of each value. A value that could
//! be written two ways (upper/lowercase hex, unpadded base64, a `+02:00`
//! offset) is rejected rather than normalized, so every signed document has a
//! single canonical reading.

use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Length of a BLAKE3-256 digest in bytes.
pub const DIGEST_LEN: usize = 32;

/// Why a hex, base64 or timestamp string was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("expected {expected} lowercase hex characters")]
    Hex { expected: usize },
    #[error("expected canonical padded base64 of {expected} bytes")]
    Base64 { expected: usize },
    #[error("invalid base64")]
    Base64Any,
    #[error("expected an RFC 3339 UTC timestamp like 2026-09-24T10:00:00Z")]
    Timestamp,
}

/// Decodes exactly `2 * N` lowercase hex characters.
pub fn decode_hex<const N: usize>(s: &str) -> Result<[u8; N], CodecError> {
    let err = CodecError::Hex { expected: 2 * N };
    if s.len() != 2 * N || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(err);
    }
    let mut out = [0u8; N];
    hex::decode_to_slice(s, &mut out).map_err(|_| err)?;
    Ok(out)
}

/// Encodes bytes as standard padded base64.
pub fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes standard padded base64 of any length (canonical encodings only).
pub fn decode_base64(s: &str) -> Result<Vec<u8>, CodecError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| CodecError::Base64Any)
}

/// Decodes standard padded base64 that must hold exactly `N` bytes.
pub fn decode_base64_exact<const N: usize>(s: &str) -> Result<[u8; N], CodecError> {
    let err = CodecError::Base64 { expected: N };
    // A canonical padded encoding of N bytes has exactly this length; checking
    // first bounds the allocation for hostile input.
    if s.len() != N.div_ceil(3) * 4 {
        return Err(err);
    }
    let bytes = decode_base64(s).map_err(|_| err.clone())?;
    bytes.try_into().map_err(|_| err)
}

/// Compares two 32-byte values in constant time.
pub fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    // `blake3::Hash`'s equality is constant-time (`constant_time_eq`).
    blake3::Hash::from_bytes(*a) == blake3::Hash::from_bytes(*b)
}

/// A BLAKE3-256 digest, lowercase hex on the wire. Equality is constant-time.
#[derive(Clone, Copy, Eq)]
pub struct Digest([u8; DIGEST_LEN]);

impl Digest {
    /// BLAKE3 of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub const fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    /// 64 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl From<blake3::Hash> for Digest {
    fn from(h: blake3::Hash) -> Self {
        Self(*h.as_bytes())
    }
}

impl PartialEq for Digest {
    fn eq(&self, other: &Self) -> bool {
        ct_eq_32(&self.0, &other.0)
    }
}

impl std::hash::Hash for Digest {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl FromStr for Digest {
    type Err = CodecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        decode_hex::<DIGEST_LEN>(s).map(Self)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// An RFC 3339 instant in UTC (`Z` or `+00:00` on input, `Z` on output).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// Converts to UTC. Callers pass `now` explicitly: this crate never reads a clock.
    pub fn new(t: OffsetDateTime) -> Self {
        Self(t.to_offset(time::UtcOffset::UTC))
    }

    pub fn from_unix(seconds: i64) -> Result<Self, CodecError> {
        OffsetDateTime::from_unix_timestamp(seconds)
            .map(Self)
            .map_err(|_| CodecError::Timestamp)
    }

    pub const fn get(&self) -> OffsetDateTime {
        self.0
    }

    pub fn unix(&self) -> i64 {
        self.0.unix_timestamp()
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(t: OffsetDateTime) -> Self {
        Self::new(t)
    }
}

impl FromStr for Timestamp {
    type Err = CodecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Bound the input before handing it to the parser.
        if s.len() > 40
            || s.as_bytes().get(10) != Some(&b'T')
            || !(s.ends_with('Z') || s.ends_with("+00:00"))
        {
            return Err(CodecError::Timestamp);
        }
        let t = OffsetDateTime::parse(s, &Rfc3339).map_err(|_| CodecError::Timestamp)?;
        if !t.offset().is_utc() {
            return Err(CodecError::Timestamp);
        }
        Ok(Self(t))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.format(&Rfc3339) {
            Ok(s) => f.write_str(&s),
            Err(_) => Err(fmt::Error),
        }
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({self})")
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_and_exact() {
        assert_eq!(decode_hex::<2>("0aff").unwrap(), [0x0a, 0xff]);
        for bad in ["0AFF", "0af", "0aff0", "0agf", " 0af"] {
            assert!(decode_hex::<2>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn base64_is_canonical_and_exact() {
        let b = encode_base64(&[1, 2, 3, 4]);
        assert_eq!(b, "AQIDBA==");
        assert_eq!(decode_base64_exact::<4>(&b).unwrap(), [1, 2, 3, 4]);
        // Unpadded, URL-safe, non-canonical trailing bits, wrong length.
        for bad in ["AQIDBA", "AQIDBB==", "AQID", "AQIDBA==AA", "AQ-DBA=="] {
            assert!(decode_base64_exact::<4>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn digest_hex_roundtrip() {
        let d = Digest::of(b"");
        assert_eq!(
            d.to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(d.to_hex().parse::<Digest>().unwrap(), d);
        assert!(d.to_hex().to_uppercase().parse::<Digest>().is_err());
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), d);
    }

    #[test]
    fn timestamps_are_utc_rfc3339() {
        let t: Timestamp = "2026-09-24T10:00:00Z".parse().unwrap();
        assert_eq!(t.to_string(), "2026-09-24T10:00:00Z");
        assert_eq!("2026-09-24T10:00:00+00:00".parse::<Timestamp>().unwrap(), t);
        for bad in [
            "2026-09-24T10:00:00+02:00",
            "2026-09-24 10:00:00Z",
            "2026-09-24",
            "",
            "2026-13-24T10:00:00Z",
        ] {
            assert!(bad.parse::<Timestamp>().is_err(), "{bad}");
        }
    }
}
