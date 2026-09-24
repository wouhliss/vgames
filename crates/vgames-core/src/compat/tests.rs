use serde_json::{Value, json};

use super::*;

pub(crate) const PACKAGE: &str = "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b";

/// The 09-compatibility §4 example (Linux).
pub(crate) fn linux() -> Value {
    json!({
        "format": "vgames.compat/1",
        "server_id": "01920000-0000-7000-8000-000000000000",
        "package_id": PACKAGE,
        "target": "linux",
        "revision": 3,
        "created_at": "2026-09-24T10:00:00Z",
        "applies_to": { "platform": "windows-x86_64", "min_sequence": 1, "max_sequence": null },
        "status": "verified",
        "notes": "Controller works out of the box.",
        "runner": {
            "kind": "proton",
            "prefer": ["umu-proton", "ge-proton"],
            "min_version": "GE-Proton10-1",
            "umu_game_id": "umu-12345",
            "env": { "PROTON_ENABLE_NVAPI": "1" },
            "dll_overrides": { "d3dcompiler_47": "native,builtin" },
            "winetricks": ["vcrun2022"]
        }
    })
}

fn macos() -> Value {
    let mut v = linux();
    v["target"] = json!("macos");
    v["runner"]["kind"] = json!("wine");
    v["runner"]["prefer"] = json!(["wine-gcenx"]);
    v["runner"]["min_version"] = json!("10.0");
    v["runner"]["graphics"] = json!(["d3dmetal", "dxmt", "dxvk", "wined3d"]);
    v["runner"].as_object_mut().unwrap().remove("umu_game_id");
    v
}

fn check(v: &Value) -> Result<CompatProfile, CompatError> {
    parse_and_validate(&serde_json::to_vec(v).unwrap())
}

fn with(f: impl FnOnce(&mut Value)) -> Result<CompatProfile, CompatError> {
    let mut v = linux();
    f(&mut v);
    check(&v)
}

#[test]
fn examples_are_valid() {
    let p = check(&linux()).unwrap();
    assert_eq!(p.runner.kind, RunnerKind::Proton);
    assert_eq!(p.wine_dll_overrides(), "d3dcompiler_47=native,builtin");
    assert!(p.applies_to(Platform::WindowsX86_64, 12));
    assert!(!p.applies_to(Platform::WindowsAarch64, 12));
    let m = check(&macos()).unwrap();
    assert_eq!(m.runner.graphics[0], Graphics::D3dmetal);
    // Minimal profile: every optional field absent.
    let mut v = linux();
    v.as_object_mut().unwrap().remove("notes");
    v["runner"] = json!({ "kind": "proton" });
    check(&v).unwrap();
}

#[test]
fn wire_format_snapshot() {
    let p = check(&linux()).unwrap();
    insta::assert_snapshot!(serde_json::to_string(&p).unwrap());
}

#[test]
fn rule_format_and_unknown_fields() {
    assert!(matches!(
        with(|v| v["format"] = json!("vgames.compat/2")),
        Err(CompatError::Format(_))
    ));
    assert!(matches!(
        with(|v| v["runner"]["shell"] = json!("bash")),
        Err(CompatError::Json(_))
    ));
    assert!(matches!(
        with(|v| v["target"] = json!("windows")),
        Err(CompatError::Json(_))
    ));
    assert!(matches!(
        with(|v| v["status"] = json!("great")),
        Err(CompatError::Json(_))
    ));
    assert_eq!(
        parse_and_validate(&vec![b' '; MAX_PROFILE_BYTES + 1]),
        Err(CompatError::TooLarge)
    );
}

#[test]
fn rule_revision_and_applies_to() {
    assert_eq!(
        with(|v| v["revision"] = json!(0)),
        Err(CompatError::ZeroRevision)
    );
    assert_eq!(
        with(|v| v["applies_to"]["platform"] = json!("linux-x86_64")),
        Err(CompatError::NotWindows)
    );
    assert_eq!(
        with(|v| v["applies_to"]["min_sequence"] = json!(0)),
        Err(CompatError::SequenceRange)
    );
    assert_eq!(
        with(|v| {
            v["applies_to"]["min_sequence"] = json!(5);
            v["applies_to"]["max_sequence"] = json!(4);
        }),
        Err(CompatError::SequenceRange)
    );
    let p = with(|v| v["applies_to"]["max_sequence"] = json!(10)).unwrap();
    assert!(p.applies_to(Platform::WindowsX86_64, 10));
    assert!(!p.applies_to(Platform::WindowsX86_64, 11));
}

#[test]
fn rule_notes_plain_text() {
    with(|v| v["notes"] = json!("line one\nline two\ttab")).unwrap();
    assert_eq!(
        with(|v| v["notes"] = json!("\u{1b}[31mred")),
        Err(CompatError::Notes)
    );
    assert_eq!(
        with(|v| v["notes"] = json!("x".repeat(MAX_NOTES_CHARS + 1))),
        Err(CompatError::Notes)
    );
}

#[test]
fn rule_runner_kind_matches_target() {
    assert_eq!(
        with(|v| v["runner"]["kind"] = json!("wine")),
        Err(CompatError::RunnerKind)
    );
    let mut v = macos();
    v["runner"]["kind"] = json!("proton");
    assert_eq!(check(&v), Err(CompatError::RunnerKind));
}

#[test]
fn rule_prefer_ids() {
    for bad in [
        json!(["UMU-Proton"]),
        json!(["a/b"]),
        json!(["x", "x"]),
        json!([""]),
    ] {
        assert!(
            matches!(
                with(|v| v["runner"]["prefer"] = bad.clone()),
                Err(CompatError::Prefer(_))
            ),
            "{bad}"
        );
    }
}

#[test]
fn rule_versions_and_umu_ids() {
    for bad in ["", "10 1", "GE-Proton$(id)", &"9".repeat(65)] {
        assert!(matches!(
            with(|v| v["runner"]["min_version"] = json!(bad)),
            Err(CompatError::MinVersion(_))
        ));
    }
    for bad in ["12345", "umu-", "umu-12 3", "umu-$(x)"] {
        assert!(matches!(
            with(|v| v["runner"]["umu_game_id"] = json!(bad)),
            Err(CompatError::UmuGameId(_))
        ));
    }
}

#[test]
fn rule_graphics_macos_only() {
    assert_eq!(
        with(|v| v["runner"]["graphics"] = json!(["dxvk"])),
        Err(CompatError::Graphics)
    );
    let mut v = macos();
    v["runner"]["graphics"] = json!(["dxmt", "dxmt"]);
    assert_eq!(check(&v), Err(CompatError::Graphics));
    let mut v = macos();
    v["runner"]["graphics"] = json!(["vulkan"]);
    assert!(matches!(check(&v), Err(CompatError::Json(_))));
}

#[test]
fn rule_env_manifest_rules_plus_launcher_owned() {
    for key in [
        "PATH",
        "LD_PRELOAD",
        "DYLD_INSERT_LIBRARIES",
        "WINEPREFIX",
        "WINEDLLOVERRIDES",
        "PROTONPATH",
        "GAMEID",
        "STORE",
        "STEAM_COMPAT_DATA_PATH",
        "UMU_RUNTIME_UPDATE",
        "PRESSURE_VESSEL_FILESYSTEMS_RW",
    ] {
        assert!(
            matches!(
                with(|v| v["runner"]["env"] = json!({ key: "x" })),
                Err(CompatError::Env(EnvFault::KeyDenied(_)))
            ),
            "{key}"
        );
    }
    assert!(matches!(
        with(|v| v["runner"]["env"] = json!({ "lower": "x" })),
        Err(CompatError::Env(EnvFault::KeySyntax(_)))
    ));
    for ok in [
        "PROTON_ENABLE_NVAPI",
        "DXVK_HUD",
        "WINEDEBUG",
        "MTL_HUD_ENABLED",
    ] {
        with(|v| v["runner"]["env"] = json!({ ok: "1" })).unwrap();
    }
    // Duplicate keys are refused, not silently collapsed.
    let dup = String::from_utf8(serde_json::to_vec(&linux()).unwrap())
        .unwrap()
        .replacen(
            "\"PROTON_ENABLE_NVAPI\":\"1\"",
            "\"PROTON_ENABLE_NVAPI\":\"1\",\"PROTON_ENABLE_NVAPI\":\"0\"",
            1,
        );
    assert!(matches!(
        parse_and_validate(dup.as_bytes()),
        Err(CompatError::Json(_))
    ));
}

#[test]
fn rule_dll_overrides() {
    for (name, mode, ok) in [
        ("d3d11", "native", true),
        ("d3d11", "n,b", true),
        ("d3d11", "builtin,native", true),
        ("d3d11", "", true),
        ("xinput1_3.dll", "native", true),
        ("D3D11", "native", false),
        ("d3d11", "native,builtin,native", false),
        ("d3d11", "native;evil=native", false),
        ("../x", "native", false),
        ("", "native", false),
        ("d3d11", "disabled", false),
    ] {
        let r = with(|v| v["runner"]["dll_overrides"] = json!({ name: mode }));
        assert_eq!(r.is_ok(), ok, "{name}={mode}: {r:?}");
    }
}

#[test]
fn rule_winetricks_allowlist() {
    for verb in WINETRICKS_ALLOWLIST {
        with(|v| v["runner"]["winetricks"] = json!([verb])).unwrap();
    }
    for bad in [
        json!(["dxvk"]),
        json!(["win10"]),
        json!(["vcrun2022", "vcrun2022"]),
        json!(["; rm -rf ~"]),
    ] {
        assert!(
            matches!(
                with(|v| v["runner"]["winetricks"] = bad.clone()),
                Err(CompatError::Winetricks(_))
            ),
            "{bad}"
        );
    }
}
