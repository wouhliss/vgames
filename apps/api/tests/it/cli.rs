//! A1-T01: the binary validates configuration before doing anything else.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vgames-api"));
    // Run from an empty directory so no `.env` is picked up, with an empty environment.
    c.current_dir(std::env::temp_dir()).env_clear();
    c
}

#[test]
fn invalid_configuration_lists_every_problem_and_exits_1() {
    let out = bin()
        .arg("--check-config")
        .env("VGAMES_PUBLIC_URL", "http://example.com")
        .env("VGAMES_LOG_FORMAT", "xml")
        .output()
        .expect("run binary");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in [
        "VGAMES_PUBLIC_URL is invalid: must use https",
        "VGAMES_LOG_FORMAT is invalid",
        "VGAMES_SERVER_ID is required",
        "DATABASE_URL is required",
        "VGAMES_ROOT_PUBLIC_KEY is required",
        "VGAMES_STORAGE_BACKEND is required",
    ] {
        assert!(stderr.contains(needle), "missing `{needle}` in:\n{stderr}");
    }
}

#[test]
fn valid_configuration_passes_the_check() {
    let out = bin()
        .arg("--check-config")
        .env("VGAMES_PUBLIC_URL", "http://localhost:8080")
        .env("VGAMES_SERVER_ID", "01920000-0000-7000-8000-000000000000")
        .env("DATABASE_URL", "postgres://u:p@localhost/db")
        .env(
            "VGAMES_ROOT_PUBLIC_KEY",
            "sn/b0D2mU+3RfBglb8/Jy/2BfTWHvE2SyIgXkx3m8U8=",
        )
        .env(
            "VGAMES_SERVER_SECRET",
            "c9evMYsFxkS9GqCgKOfR45wqwHMxmbhPPzHjLtlpruc=",
        )
        .env("VGAMES_DEV_FAKE_DISCORD", "true")
        .env("VGAMES_STORAGE_BACKEND", "fs")
        .env("VGAMES_FS_STORAGE_ROOT", "/tmp/vgames-cli-test")
        .env(
            "VGAMES_FS_URL_SIGNING_KEY",
            "c9evMYsFxkS9GqCgKOfR45wqwHMxmbhPPzHjLtlpruc=",
        )
        .output()
        .expect("run binary");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
