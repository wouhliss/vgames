//! The offline key ceremony end to end, through the real `vgames` binary:
//! root → publisher → bundle v1 → sign → verify → revoke in v2 → refuse to
//! re-add the revoked key. Non-interactive (`--passphrase-env`).

use std::path::Path;
use std::process::{Command, Output};

use vgames_core::trust::{RootPin, SignedBundle, verify_bundle};

const SERVER: &str = "01920000-0000-7000-8000-000000000000";
const ALICE: &str = "0192aaaa-0000-7000-8000-000000000001";
const PASS: &str = "correct horse battery staple";

fn vgames(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vgames"))
        .current_dir(dir)
        .env("TEST_PASS", PASS)
        .env("WRONG_PASS", "not the right passphrase at all")
        .args(args)
        .output()
        .expect("run vgames")
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = vgames(dir, args);
    assert!(
        out.status.success(),
        "vgames {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn fails(dir: &Path, args: &[&str]) -> String {
    let out = vgames(dir, args);
    assert!(!out.status.success(), "vgames {args:?} should fail");
    String::from_utf8(out.stderr).unwrap()
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s.trim()).unwrap()
}

#[test]
fn offline_ceremony() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();

    let root = json(&ok(
        d,
        &[
            "keys",
            "init-root",
            "--out",
            "root.vgkey",
            "--passphrase-env",
            "TEST_PASS",
            "--json",
        ],
    ));
    let alice = json(&ok(
        d,
        &[
            "keys",
            "issue-publisher",
            "--label",
            "alice@workstation",
            "--out",
            "alice.vgkey",
            "--passphrase-env",
            "TEST_PASS",
            "--json",
        ],
    ));
    let bob = json(&ok(
        d,
        &[
            "keys",
            "issue-publisher",
            "--label",
            "bob@laptop",
            "--out",
            "bob.vgkey",
            "--passphrase-env",
            "TEST_PASS",
            "--json",
        ],
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(d.join("root.vgkey"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Refuses to overwrite a key file.
    assert!(
        fails(
            d,
            &[
                "keys",
                "init-root",
                "--out",
                "root.vgkey",
                "--passphrase-env",
                "TEST_PASS"
            ]
        )
        .contains("already exists")
    );
    // Weak passphrases are refused.
    let weak = vgames(
        d,
        &[
            "keys",
            "init-root",
            "--out",
            "weak.vgkey",
            "--passphrase-env",
            "WEAK",
        ],
    );
    assert!(!weak.status.success());

    let shown = json(&ok(d, &["keys", "show", "root.vgkey", "--json"]));
    assert_eq!(shown["fingerprint"], root["fingerprint"]);
    assert_eq!(shown["kind"], "root");

    let publisher_toml = |p: &serde_json::Value| {
        format!(
            "[[publishers]]\npublic_key = \"{}\"\nlabel = \"{}\"\nholder_user_id = \"{ALICE}\"\nnot_before = \"2026-09-24T00:00:00Z\"\nnot_after = \"2028-09-24T00:00:00Z\"\n",
            p["public_key"].as_str().unwrap(),
            p["label"].as_str().unwrap()
        )
    };
    let head = format!("server_id = \"{SERVER}\"\n");

    // v1: alice.
    std::fs::write(
        d.join("v1.toml"),
        format!("{head}{}", publisher_toml(&alice)),
    )
    .unwrap();
    ok(
        d,
        &[
            "trust",
            "build",
            "--spec",
            "v1.toml",
            "--root",
            "root.vgkey",
            "--out",
            "v1.json",
        ],
    );
    // Wrong passphrase: distinct message, nothing written.
    let err = fails(
        d,
        &[
            "trust",
            "sign",
            "--bundle",
            "v1.json",
            "--root",
            "root.vgkey",
            "--out",
            "v1.signed.json",
            "--yes",
            "--passphrase-env",
            "WRONG_PASS",
        ],
    );
    assert!(err.contains("wrong passphrase"), "{err}");
    assert!(!d.join("v1.signed.json").exists());
    // A publisher key cannot sign a bundle.
    assert!(
        fails(
            d,
            &[
                "trust",
                "sign",
                "--bundle",
                "v1.json",
                "--root",
                "alice.vgkey",
                "--out",
                "x.json",
                "--yes",
                "--passphrase-env",
                "TEST_PASS",
            ],
        )
        .contains("root")
    );
    ok(
        d,
        &[
            "trust",
            "sign",
            "--bundle",
            "v1.json",
            "--root",
            "root.vgkey",
            "--out",
            "v1.signed.json",
            "--yes",
            "--passphrase-env",
            "TEST_PASS",
        ],
    );
    let fp = root["fingerprint"].as_str().unwrap();
    ok(
        d,
        &[
            "trust",
            "verify",
            "--signed",
            "v1.signed.json",
            "--root",
            "root.vgkey",
            "--fingerprint",
            fp,
            "--server-id",
            SERVER,
        ],
    );
    // Against another root, or the wrong fingerprint: refused.
    fails(
        d,
        &[
            "trust",
            "verify",
            "--signed",
            "v1.signed.json",
            "--root",
            bob["public_key"].as_str().unwrap(),
        ],
    );
    fails(
        d,
        &[
            "trust",
            "verify",
            "--signed",
            "v1.signed.json",
            "--root",
            "root.vgkey",
            "--fingerprint",
            bob["fingerprint"].as_str().unwrap(),
        ],
    );

    // v2: bob added, alice revoked. Version = previous + 1.
    std::fs::write(
        d.join("v2.toml"),
        format!(
            "{head}{}[[revoked]]\nkey_id = \"{}\"\nreason = \"laptop stolen\"\n",
            publisher_toml(&bob),
            alice["key_id"].as_str().unwrap()
        ),
    )
    .unwrap();
    let summary = ok(
        d,
        &[
            "trust",
            "build",
            "--spec",
            "v2.toml",
            "--root",
            "root.vgkey",
            "--previous",
            "v1.signed.json",
            "--out",
            "v2.json",
        ],
    );
    assert!(summary.contains("v2"), "{summary}");
    assert!(summary.contains("revokes alice@workstation"), "{summary}");
    ok(
        d,
        &[
            "trust",
            "sign",
            "--bundle",
            "v2.json",
            "--root",
            "root.vgkey",
            "--out",
            "v2.signed.json",
            "--yes",
            "--passphrase-env",
            "TEST_PASS",
        ],
    );
    // An older version is refused when the last seen one is known.
    fails(
        d,
        &[
            "trust",
            "verify",
            "--signed",
            "v1.signed.json",
            "--root",
            "root.vgkey",
            "--last-version",
            "2",
        ],
    );

    // The signed file is exactly what launchers and the server verify.
    let signed: SignedBundle =
        serde_json::from_slice(&std::fs::read(d.join("v2.signed.json")).unwrap()).unwrap();
    let root_pk =
        vgames_core::PublicKey::from_base64(root["public_key"].as_str().unwrap()).unwrap();
    let v = verify_bundle(
        &signed.bundle_bytes().unwrap(),
        &signed.signature,
        &RootPin::new(root_pk),
        Some(1),
        SERVER.parse().unwrap(),
    )
    .unwrap();
    assert_eq!(v.state.version(), 2);
    let alice_id = alice["key_id"].as_str().unwrap().parse().unwrap();
    assert!(v.state.is_revoked(&alice_id));

    // v3 must keep alice revoked even if the spec forgets the revocation…
    std::fs::write(d.join("v3.toml"), format!("{head}{}", publisher_toml(&bob))).unwrap();
    ok(
        d,
        &[
            "trust",
            "build",
            "--spec",
            "v3.toml",
            "--root",
            "root.vgkey",
            "--previous",
            "v2.signed.json",
            "--out",
            "v3.json",
        ],
    );
    let v3: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.join("v3.json")).unwrap()).unwrap();
    assert_eq!(v3["version"], 3);
    assert_eq!(v3["revoked"][0]["key_id"], alice["key_id"]);
    // …and refuses to trust alice again.
    std::fs::write(
        d.join("v3b.toml"),
        format!("{head}{}", publisher_toml(&alice)),
    )
    .unwrap();
    let err = fails(
        d,
        &[
            "trust",
            "build",
            "--spec",
            "v3b.toml",
            "--root",
            "root.vgkey",
            "--previous",
            "v2.signed.json",
            "--out",
            "v3b.json",
        ],
    );
    assert!(err.contains("revoked"), "{err}");
}

#[test]
fn every_command_has_help_examples() {
    let d = tempfile::tempdir().unwrap();
    for cmd in [
        &["keys", "init-root", "--help"][..],
        &["keys", "issue-publisher", "--help"],
        &["keys", "show", "--help"],
        &["trust", "build", "--help"],
        &["trust", "sign", "--help"],
        &["trust", "verify", "--help"],
    ] {
        let out = ok(d.path(), cmd);
        assert!(out.contains("Examples:"), "{cmd:?}");
    }
}
