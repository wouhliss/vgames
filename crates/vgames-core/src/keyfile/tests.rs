use serde_json::{Value, json};

use super::*;
use crate::codec::Digest;
use crate::sign::Context;

const PASS: &[u8] = b"correct horse battery staple";

fn key() -> SecretKey {
    SecretKey::from_seed(&[42; 32])
}

fn file() -> KeyFile {
    KeyFile::encrypt_with(
        &key(),
        KeyKind::Publisher,
        "alice@workstation",
        "2026-09-24T10:00:00Z".parse().unwrap(),
        PASS,
        [1; SALT_LEN],
        [2; NONCE_LEN],
    )
    .unwrap()
}

fn json_of(f: &KeyFile) -> Value {
    serde_json::from_slice(&f.to_bytes()).unwrap()
}

fn reparse(v: &Value) -> Result<KeyFile, KeyFileError> {
    KeyFile::parse(&serde_json::to_vec(v).unwrap())
}

#[test]
fn roundtrip_and_sign() {
    let f = KeyFile::encrypt(
        &key(),
        KeyKind::Root,
        "owner offline laptop",
        "2026-09-24T10:00:00Z".parse().unwrap(),
        PASS,
    )
    .unwrap();
    let parsed = KeyFile::parse(&f.to_bytes()).unwrap();
    assert_eq!(parsed, f);
    assert_eq!(parsed.kind, KeyKind::Root);
    assert_eq!(parsed.key_id, key().public_key().key_id());
    let k = parsed.decrypt(PASS).unwrap();
    assert_eq!(k.public_key(), key().public_key());
    let sig = k.sign(Context::Trust, b"bundle");
    key()
        .public_key()
        .verify(Context::Trust, b"bundle", &sig)
        .unwrap();
}

#[test]
fn fresh_salt_and_nonce_every_time() {
    let at = "2026-09-24T10:00:00Z".parse().unwrap();
    let a = KeyFile::encrypt(&key(), KeyKind::Publisher, "a", at, PASS).unwrap();
    let b = KeyFile::encrypt(&key(), KeyKind::Publisher, "a", at, PASS).unwrap();
    assert_ne!(a.salt, b.salt);
    assert_ne!(a.nonce, b.nonce);
    assert_ne!(a.ciphertext, b.ciphertext);
}

#[test]
fn format_is_pinned() {
    // Deterministic inputs → deterministic file. A change here is a format change.
    let bytes = file().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["format"], "vgames.key/1");
    assert_eq!(
        v["kdf"],
        json!({"alg": "argon2id", "m_kib": 65536, "t": 3, "p": 1, "salt": "AQEBAQEBAQEBAQEBAQEBAQ=="})
    );
    assert_eq!(v["cipher"]["alg"], "xchacha20poly1305");
    assert_eq!(
        Digest::of(&bytes).to_hex(),
        "9b8e2c8853e363f502bbfdf582babb6640ab360fb18dd649bfbb0781c02593e2",
        "update only with a contract change"
    );
}

#[test]
fn wrong_passphrase_is_a_distinct_error() {
    assert_eq!(
        file().decrypt(b"wrong").unwrap_err(),
        KeyFileError::WrongPassphrase
    );
    assert_eq!(
        file().decrypt(b"").unwrap_err(),
        KeyFileError::WrongPassphrase
    );
}

#[test]
fn tampered_header_fails_authentication() {
    let other = SecretKey::from_seed(&[7; 32]).public_key();
    type Edit = Box<dyn Fn(&mut Value)>;
    let edits: [(&str, Edit); 5] = [
        ("kind", Box::new(|v| v["kind"] = json!("root"))),
        ("label", Box::new(|v| v["label"] = json!("mallory"))),
        (
            "created_at",
            Box::new(|v| v["created_at"] = json!("2027-01-01T00:00:00Z")),
        ),
        (
            "nonce",
            Box::new(|v| v["cipher"]["nonce"] = json!(codec::encode_base64(&[3; NONCE_LEN]))),
        ),
        (
            "public_key",
            Box::new(move |v| {
                v["public_key"] = json!(other.to_base64());
                v["key_id"] = json!(other.key_id().to_hex());
            }),
        ),
    ];
    for (name, edit) in edits {
        let mut v = json_of(&file());
        edit(&mut v);
        let f = reparse(&v).unwrap();
        assert_eq!(
            f.decrypt(PASS).unwrap_err(),
            KeyFileError::AuthenticationFailed,
            "{name}"
        );
    }
}

#[test]
fn tampered_ciphertext_fails_authentication() {
    let mut f = file();
    f.ciphertext[0] ^= 1;
    assert_eq!(
        f.decrypt(PASS).unwrap_err(),
        KeyFileError::AuthenticationFailed
    );
    let mut f = file();
    f.ciphertext[47] ^= 0x80; // tag
    assert_eq!(
        f.decrypt(PASS).unwrap_err(),
        KeyFileError::AuthenticationFailed
    );
}

#[test]
fn tampered_salt_or_check_reads_as_wrong_passphrase() {
    let mut f = file();
    f.salt[0] ^= 1;
    assert_eq!(f.decrypt(PASS).unwrap_err(), KeyFileError::WrongPassphrase);
    let mut f = file();
    f.check[0] ^= 1;
    assert_eq!(f.decrypt(PASS).unwrap_err(), KeyFileError::WrongPassphrase);
}

#[test]
fn kdf_parameters_are_fixed() {
    for (field, value) in [
        ("m_kib", json!(4_194_304)),
        ("m_kib", json!(8)),
        ("t", json!(1)),
        ("p", json!(64)),
        ("alg", json!("argon2i")),
    ] {
        let mut v = json_of(&file());
        v["kdf"][field] = value;
        assert_eq!(
            reparse(&v).unwrap_err(),
            KeyFileError::UnsupportedKdf,
            "{field}"
        );
    }
    let mut v = json_of(&file());
    v["cipher"]["alg"] = json!("aes-256-gcm");
    assert!(matches!(
        reparse(&v),
        Err(KeyFileError::UnsupportedCipher(_))
    ));
}

#[test]
fn strict_parsing() {
    let mut v = json_of(&file());
    v["format"] = json!("vgames.key/2");
    assert!(matches!(reparse(&v), Err(KeyFileError::Format(_))));

    let mut v = json_of(&file());
    v["extra"] = json!(1);
    assert!(matches!(reparse(&v), Err(KeyFileError::Json(_))));

    let mut v = json_of(&file());
    v["key_id"] = json!(
        SecretKey::from_seed(&[7; 32])
            .public_key()
            .key_id()
            .to_hex()
    );
    assert_eq!(reparse(&v).unwrap_err(), KeyFileError::KeyIdMismatch);

    let mut v = json_of(&file());
    v["ciphertext"] = json!("AAAA");
    assert!(matches!(
        reparse(&v),
        Err(KeyFileError::Field {
            field: "ciphertext",
            ..
        })
    ));

    let mut v = json_of(&file());
    v["label"] = json!("");
    assert_eq!(reparse(&v).unwrap_err(), KeyFileError::Label);

    assert_eq!(
        KeyFile::parse(&vec![b' '; MAX_KEYFILE_BYTES + 1]).unwrap_err(),
        KeyFileError::TooLarge
    );
}

#[test]
fn debug_hides_ciphertext_and_salt() {
    let dbg = format!("{:?}", file());
    assert!(dbg.contains("alice@workstation"));
    assert!(!dbg.contains("salt") && !dbg.contains("ciphertext"));
}
