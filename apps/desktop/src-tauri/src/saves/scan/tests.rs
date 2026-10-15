use super::*;
use vgames_core::manifest::SaveBase;
fn config() -> SaveLocation {
    SaveLocation {
        id: "game".into(),
        base: SaveBase::Documents,
        path: "game".into(),
        include: vec!["**/*.sav".into()],
        exclude: vec!["cache/**".into()],
    }
}
#[test]
fn scan_filters_hashes_only_changes_and_keeps_windows_relative_names() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("game/campaign")).unwrap();
    std::fs::create_dir_all(root.path().join("game/cache")).unwrap();
    std::fs::write(root.path().join("game/slot.sav"), b"old save").unwrap();
    std::fs::write(root.path().join("game/campaign/auto.sav"), b"campaign").unwrap();
    std::fs::write(root.path().join("game/cache/cache.sav"), b"excluded").unwrap();
    std::fs::write(root.path().join("game/log.txt"), b"excluded").unwrap();
    let locations = [Location::open(&config(), root.path()).unwrap()];
    let cancel = CancellationToken::new();
    let first = scan(&locations, &BTreeMap::new(), &cancel).unwrap();
    assert_eq!(first.hashed_files, 2);
    assert_eq!(first.total_bytes, 16);
    assert_eq!(
        first.files.get("game/slot.sav").unwrap().blake3,
        blake3::hash(b"old save").to_hex().to_string()
    );
    assert!(first.files.contains_key("game/campaign/auto.sav"));
    let repeat = scan(&locations, &first.files, &cancel).unwrap();
    assert_eq!(repeat.hashed_files, 0);
    assert_eq!(repeat.files, first.files);
    let changed = root.path().join("game/slot.sav");
    std::fs::write(&changed, b"new save").unwrap();
    std::fs::File::options()
        .write(true)
        .open(changed)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100)),
        )
        .unwrap();
    let updated = scan(&locations, &first.files, &cancel).unwrap();
    assert_eq!(updated.hashed_files, 1);
    assert_ne!(
        updated.files.get("game/slot.sav"),
        first.files.get("game/slot.sav")
    );
}
#[test]
fn missing_locations_limits_bad_filters_and_cancellation_do_not_create_or_change_files() {
    let root = tempfile::tempdir().unwrap();
    let location = Location::open(&config(), root.path()).unwrap();
    assert!(
        scan(&[location], &BTreeMap::new(), &CancellationToken::new())
            .unwrap()
            .files
            .is_empty()
    );
    assert!(!root.path().join("game").exists());
    std::fs::create_dir(root.path().join("game")).unwrap();
    std::fs::write(root.path().join("game/slot.sav"), b"save").unwrap();
    let locations = [Location::open(&config(), root.path()).unwrap()];
    for limits in [
        Limits {
            files: 0,
            bytes: 100,
        },
        Limits {
            files: 10,
            bytes: 3,
        },
    ] {
        assert!(matches!(
            scan_with_limits(
                &locations,
                &BTreeMap::new(),
                &CancellationToken::new(),
                limits
            ),
            Err(Error::Limit)
        ));
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        scan(&locations, &BTreeMap::new(), &cancel),
        Err(Error::Cancelled)
    ));
    let mut bad = config();
    bad.path = "../outside".into();
    assert!(matches!(
        Location::open(&bad, root.path()),
        Err(Error::Invalid)
    ));
    bad = config();
    bad.include = vec!["[".into()];
    assert!(matches!(
        Location::open(&bad, root.path()),
        Err(Error::Invalid)
    ));
    assert_eq!(
        std::fs::read(root.path().join("game/slot.sav")).unwrap(),
        b"save"
    );
}
#[cfg(unix)]
#[test]
fn dangling_known_folder_is_refused_even_when_a_child_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("Documents");
    std::os::unix::fs::symlink(root.path().join("absent"), &base).unwrap();
    for path in [&base, &base.join("nested")] {
        assert!(matches!(Location::open(&config(), path), Err(Error::Link)));
    }
}
#[test]
fn absent_known_folder_is_empty_without_creation_and_preserves_overlap_checks() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("Documents/nested");
    let cancel = CancellationToken::new();
    let location = Location::open(&config(), &base).unwrap();
    let result = scan(&[location], &BTreeMap::new(), &cancel).unwrap();
    assert!(result.files.is_empty());
    assert_eq!(result.hashed_files, 0);
    assert!(!root.path().join("Documents").exists());
    let mut second = config();
    second.id = "second".into();
    second.path = "nested/game/child".into();
    let locations = [
        Location::open(&config(), &base).unwrap(),
        Location::open(&second, &root.path().join("Documents")).unwrap(),
    ];
    assert!(matches!(
        scan(&locations, &BTreeMap::new(), &cancel),
        Err(Error::Invalid)
    ));
    std::fs::write(root.path().join("not-a-folder"), b"fixture").unwrap();
    assert!(Location::open(&config(), &root.path().join("not-a-folder")).is_err());
}
#[cfg(unix)]
#[test]
fn both_location_and_descendant_links_are_refused_before_reading_outside_bytes() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private.sav"), b"private fixture").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("game")).unwrap();
    assert!(matches!(
        Location::open(&config(), root.path()),
        Err(Error::Link)
    ));
    std::fs::remove_file(root.path().join("game")).unwrap();
    std::fs::create_dir(root.path().join("game")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("private.sav"),
        root.path().join("game/link.sav"),
    )
    .unwrap();
    let locations = [Location::open(&config(), root.path()).unwrap()];
    assert!(matches!(
        scan(&locations, &BTreeMap::new(), &CancellationToken::new()),
        Err(Error::Link)
    ));
}
#[cfg(target_os = "linux")]
#[test]
fn portable_case_collisions_are_refused_even_on_a_case_sensitive_filesystem() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("game")).unwrap();
    std::fs::write(root.path().join("game/slot.sav"), b"one").unwrap();
    std::fs::write(root.path().join("game/SLOT.sav"), b"two").unwrap();
    let locations = [Location::open(&config(), root.path()).unwrap()];
    assert!(matches!(
        scan(&locations, &BTreeMap::new(), &CancellationToken::new()),
        Err(Error::Invalid)
    ));
}

#[test]
fn overlapping_location_roots_are_refused_instead_of_uploading_the_same_files_twice() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("game/nested")).unwrap();
    let mut nested = config();
    nested.id = "nested".into();
    nested.path = "game/nested".into();
    let locations = [
        Location::open(&config(), root.path()).unwrap(),
        Location::open(&nested, root.path()).unwrap(),
    ];
    assert!(matches!(
        scan(&locations, &BTreeMap::new(), &CancellationToken::new()),
        Err(Error::Invalid)
    ));
}
#[cfg(windows)]
#[test]
fn windows_junctions_cannot_retarget_a_declared_save_location() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private.sav"), b"private fixture").unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.path().join("game"))
        .arg(outside.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(matches!(
        Location::open(&config(), root.path()),
        Err(Error::Link)
    ));
}
