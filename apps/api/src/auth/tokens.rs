//! Opaque tokens and their digests (docs/architecture/01-security.md §4.2).
//!
//! Tokens are 256-bit random values, base64url without padding (43 chars), with a
//! prefix that makes leaks greppable (`vga_` access, `vgr_` refresh, `vgs_` web session).
//! The database stores only SHA-256 digests.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use crate::error::ApiError;

pub const ACCESS_PREFIX: &str = "vga_";
pub const REFRESH_PREFIX: &str = "vgr_";
pub const WEB_SESSION_PREFIX: &str = "vgs_";

/// 32 random bytes, base64url (43 chars).
pub fn random_b64(bytes: usize) -> Result<String, ApiError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(ApiError::internal_from)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

pub fn new_token(prefix: &str) -> Result<String, ApiError> {
    Ok(format!("{prefix}{}", random_b64(32)?))
}

pub fn digest(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

/// `true` when `token` is `prefix` + 43 base64url chars.
pub fn well_formed(token: &str, prefix: &str) -> bool {
    token.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 43
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// PKCE S256: `BASE64URL(SHA-256(verifier)) == challenge`, compared in constant time.
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    constant_time_eq(computed.as_bytes(), challenge.as_bytes())
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_prefixed_and_well_formed() {
        let t = new_token(ACCESS_PREFIX).unwrap();
        assert!(well_formed(&t, ACCESS_PREFIX));
        assert!(!well_formed(&t, REFRESH_PREFIX));
        assert!(!well_formed("vga_short", ACCESS_PREFIX));
        assert_ne!(t, new_token(ACCESS_PREFIX).unwrap());
    }

    #[test]
    fn pkce_rfc7636_appendix_b() {
        assert!(pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
        assert!(!pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXx",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
    }
}
