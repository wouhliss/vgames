use super::*;

fn catalog(entries: serde_json::Value) -> Catalog {
    serde_json::from_value(serde_json::json!({
        "format": "vgames.runtimes/1", "version": 1, "generated_at": "2026-10-01T00:00:00Z",
        "commercial": false, "runtimes": entries
    }))
    .unwrap()
}

fn entry(id: &str, version: &str, os: &str, arch: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "version": version, "os": os, "arch": arch,
        "url": format!("https://github.com/x/{id}/releases/download/{version}/a.tar.gz"),
        "sha256": "a".repeat(64), "size": 1, "archive": "tar.gz", "license": "MIT",
        "min_launcher_version": "0.1.0"
    })
}

fn sample() -> Catalog {
    let mut wine = entry("wine-macos", "10.0", "macos", "x86_64");
    wine["rosetta_required"] = true.into();
    wine["macos_max_supported"] = 27.into();
    let mut d3dmetal = entry("d3dmetal", "3.0", "macos", "aarch64");
    d3dmetal["redistribution"] = "non-commercial".into();
    catalog(serde_json::json!([
        entry("umu-proton", "UMU-Proton-9.0-3", "linux", "x86_64"),
        entry("umu-proton", "UMU-Proton-9.0-4e", "linux", "x86_64"),
        entry("ge-proton", "GE-Proton10-17", "linux", "x86_64"),
        entry("umu-launcher", "1.2.9", "linux", "x86_64"),
        wine,
        entry("wine-macos", "9.0", "macos", "x86_64"),
        entry("dxmt", "0.60", "macos", "x86_64"),
        d3dmetal,
    ]))
}

#[test]
fn runners_are_the_hosts_builds_by_preference_then_newest() {
    let c = sample();
    let proton: Vec<_> = runners(Some(&c), Some(HostLayer::Proton))
        .into_iter()
        .map(|r| r.version)
        .collect();
    assert_eq!(
        proton,
        ["UMU-Proton-9.0-4e", "UMU-Proton-9.0-3", "GE-Proton10-17"]
    );
    let wine: Vec<_> = runners(Some(&c), Some(HostLayer::Wine))
        .into_iter()
        .map(|r| (r.runtime, r.version))
        .collect();
    assert_eq!(
        wine,
        [
            (RuntimeId::WineMacos, "10.0".into()),
            (RuntimeId::WineMacos, "9.0".into())
        ]
    );
    assert!(runners(None, Some(HostLayer::Proton)).is_empty());
    assert!(runners(Some(&c), None).is_empty());
}

#[test]
fn macs_never_get_d3dmetal_and_other_hosts_get_no_backends() {
    let c = sample();
    assert_eq!(
        graphics(Some(&c), Some(HostLayer::Wine)),
        [GraphicsBackend::Dxmt, GraphicsBackend::Wined3d],
        "listed backends plus wined3d; D3DMetal is in the catalog but deferred"
    );
    assert!(graphics(Some(&c), Some(HostLayer::Proton)).is_empty());
    assert_eq!(rosetta_last_macos(Some(&c)), Some("27".into()));
}

#[test]
fn choices_are_checked_against_the_catalog() {
    let c = sample();
    let wine = Some(HostLayer::Wine);
    let proton = Some(HostLayer::Proton);
    let ge = RunnerVersion {
        runtime: RuntimeId::GeProton,
        version: "GE-Proton10-17".into(),
    };
    assert_eq!(check_choice(Some(&c), proton, Some(&ge), None), Ok(()));
    assert_eq!(
        check_choice(Some(&c), wine, Some(&ge), None),
        Err(CompatSettingsError::UnknownRunner)
    );
    let gone = RunnerVersion {
        runtime: RuntimeId::GeProton,
        version: "GE-Proton1-1".into(),
    };
    assert_eq!(
        check_choice(Some(&c), proton, Some(&gone), None),
        Err(CompatSettingsError::UnknownRunner)
    );
    for backend in [GraphicsBackend::D3dmetal, GraphicsBackend::Dxvk] {
        assert_eq!(
            check_choice(Some(&c), wine, None, Some(backend)),
            Err(CompatSettingsError::GraphicsUnavailable { backend }),
            "{backend:?}"
        );
    }
    assert_eq!(
        check_choice(Some(&c), wine, None, Some(GraphicsBackend::Dxmt)),
        Ok(())
    );
    assert_eq!(
        check_choice(Some(&c), proton, None, Some(GraphicsBackend::Wined3d)),
        Err(CompatSettingsError::GraphicsUnavailable {
            backend: GraphicsBackend::Wined3d
        })
    );
}

#[test]
fn license_files_are_found_and_capped() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(license_text(dir.path()), None);
    std::fs::create_dir_all(dir.path().join("dist/share")).unwrap();
    std::fs::write(dir.path().join("dist/share/COPYING"), "LGPL text").unwrap();
    assert_eq!(license_text(dir.path()).as_deref(), Some("LGPL text"));
    std::fs::write(dir.path().join("LICENSE"), "x".repeat(400 * 1024)).unwrap();
    assert_eq!(license_text(dir.path()).map(|t| t.len()), Some(256 * 1024));
}
