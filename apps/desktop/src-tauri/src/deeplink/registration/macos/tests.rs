use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("Fixture.app/Contents/MacOS/fixture");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, b"fixture").unwrap();
    let mut values = Dictionary::new();
    for (key, value) in [
        ("CFBundleIdentifier", "app.vgames.fixture"),
        ("CFBundleExecutable", "fixture"),
        ("CFBundleName", "vgames protocol fixture"),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleVersion", "1"),
    ] {
        values.insert(key.into(), Value::String(value.into()));
    }
    Value::Dictionary(values)
        .to_file_binary(root.path().join("Fixture.app/Contents/Info.plist"))
        .unwrap();
    (root, executable)
}

#[test]
fn missing_url_type_is_repaired_without_removing_other_bundle_properties() {
    let (_root, executable) = fixture();
    let bundle = ensure_types(&executable, "app.vgames.fixture", "vgames").unwrap();
    let path = bundle.join("Contents/Info.plist");
    let first = std::fs::read(&path).unwrap();
    let value = Value::from_file(&path).unwrap();
    assert_eq!(
        value
            .as_dictionary()
            .unwrap()
            .get("CFBundleName")
            .unwrap()
            .as_string(),
        Some("vgames protocol fixture")
    );
    assert!(ensure_types(&executable, "wrong.identifier", "vgames").is_err());
    ensure_types(&executable, "app.vgames.fixture", "other-scheme").unwrap();
    let other = std::fs::read(&path).unwrap();
    assert_ne!(first, other);
    ensure_types(&executable, "app.vgames.fixture", "vgames").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), other);
    let value = Value::from_file(&path).unwrap();
    let types = value
        .as_dictionary()
        .unwrap()
        .get("CFBundleURLTypes")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(types.len(), 2);
    for entry in types {
        assert_eq!(
            entry.as_dictionary().unwrap()["CFBundleTypeRole"].as_string(),
            Some("Editor")
        );
    }
    let entries = std::fs::read_dir(path.parent().unwrap()).unwrap();
    assert!(
        !entries
            .map(Result::unwrap)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
    );
}

#[test]
fn an_existing_scheme_missing_its_required_role_is_repaired_without_duplication() {
    let (_root, executable) = fixture();
    let bundle = ensure_types(&executable, "app.vgames.fixture", "vgames").unwrap();
    let path = bundle.join("Contents/Info.plist");
    let mut value = Value::from_file(&path).unwrap();
    let entry = value.as_dictionary_mut().unwrap()["CFBundleURLTypes"]
        .as_array_mut()
        .unwrap()[0]
        .as_dictionary_mut()
        .unwrap();
    entry.remove("CFBundleTypeRole");
    entry.insert("CFBundleURLIconFile".into(), Value::String("KeepMe".into()));
    value.to_file_xml(&path).unwrap();
    let incomplete = std::fs::read(&path).unwrap();
    let signature = bundle.join("Contents/_CodeSignature");
    std::fs::create_dir(&signature).unwrap();
    assert!(ensure_types(&executable, "app.vgames.fixture", "vgames").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), incomplete);
    std::fs::remove_dir(signature).unwrap();
    ensure_types(&executable, "app.vgames.fixture", "vgames").unwrap();
    let value = Value::from_file(&path).unwrap();
    let types = value.as_dictionary().unwrap()["CFBundleURLTypes"]
        .as_array()
        .unwrap();
    assert_eq!(types.len(), 1);
    let entry = types[0].as_dictionary().unwrap();
    assert_eq!(entry["CFBundleTypeRole"].as_string(), Some("Editor"));
    assert_eq!(entry["CFBundleURLIconFile"].as_string(), Some("KeepMe"));
}

#[test]
fn damaged_signed_oversized_or_unbundled_metadata_is_not_rewritten() {
    let (root, executable) = fixture();
    let contents = root.path().join("Fixture.app/Contents");
    let path = contents.join("Info.plist");
    let original = std::fs::read(&path).unwrap();
    std::fs::create_dir(contents.join("_CodeSignature")).unwrap();
    assert!(ensure_types(&executable, "app.vgames.fixture", "vgames").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::remove_dir(contents.join("_CodeSignature")).unwrap();
    assert!(bundle(Path::new("relative.app/Contents/MacOS/exe")).is_err());
    assert!(bundle(&root.path().join("Fixture.app/exe")).is_err());
    std::fs::write(&path, b"broken").unwrap();
    assert!(ensure_types(&executable, "app.vgames.fixture", "vgames").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"broken");
    std::fs::write(&path, vec![0; LIMIT as usize + 1]).unwrap();
    assert!(ensure_types(&executable, "app.vgames.fixture", "vgames").is_err());
}

#[tokio::test]
async fn cancelled_startup_does_not_rewrite_bundle_or_start_registration_tool() {
    let (_root, executable) = fixture();
    let path = bundle(&executable).unwrap().join("Contents/Info.plist");
    let original = std::fs::read(&path).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        repair(
            executable,
            "app.vgames.fixture".into(),
            "vgames".into(),
            &cancel
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn linked_bundle_metadata_is_refused() {
    let (root, executable) = fixture();
    let path = bundle(&executable).unwrap().join("Contents/Info.plist");
    let outside = root.path().join("outside.plist");
    std::fs::rename(&path, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(ensure_types(&executable, "app.vgames.fixture", "vgames").is_err());
}

#[cfg(target_os = "macos")]
mod native;
