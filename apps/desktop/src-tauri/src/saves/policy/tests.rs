use super::*;
use vgames_core::manifest::{SaveBase, SaveLocation};

fn config() -> SaveLocation {
    SaveLocation {
        id: "game".into(),
        base: SaveBase::Documents,
        path: "saves".into(),
        include: vec!["**/*.sav".into()],
        exclude: vec!["cache".into()],
    }
}
fn file(path: &str) -> SaveFile {
    SaveFile {
        root: "game".into(),
        path: path.into(),
        size: 4,
        blake3: blake3::hash(b"save").to_hex().to_string(),
        mtime: time::OffsetDateTime::UNIX_EPOCH,
    }
}
#[test]
fn signed_filters_and_portable_tree_are_enforced_without_writes() {
    let base = tempfile::tempdir().unwrap();
    let locations = [Location::open(&config(), base.path()).unwrap()];
    let protected = ProtectedInstalls::open(std::iter::empty()).unwrap();
    let cancel = CancellationToken::new();
    assert!(
        preflight(
            &locations,
            &[file("campaign/slot.sav")],
            &protected,
            &cancel
        )
        .is_ok()
    );
    for path in [
        "../escape.sav",
        "cache/slot.sav",
        "log.txt",
        "CON.sav",
        ".vgames/slot.sav",
    ] {
        assert!(preflight(&locations, &[file(path)], &protected, &cancel).is_err());
    }
    for files in [
        vec![file("slot.sav"), file("SLOT.sav")],
        vec![file("slot.sav"), file("slot.sav/child.sav")],
        vec![file("folder/a.sav"), file("Folder/b.sav")],
    ] {
        assert!(preflight(&locations, &files, &protected, &cancel).is_err());
    }
    let mut unknown = file("slot.sav");
    unknown.root = "remote_root".into();
    assert!(preflight(&locations, &[unknown], &protected, &cancel).is_err());
    assert!(!base.path().join("saves").exists());
}
#[test]
fn install_overlap_is_refused_even_for_other_base_or_case() {
    let base = tempfile::tempdir().unwrap();
    let locations = [Location::open(&config(), base.path()).unwrap()];
    let install = base.path().join("saves/Game");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::write(install.join("authenticated.sav"), b"signed").unwrap();
    let protected = ProtectedInstalls::open([install.as_path()]).unwrap();
    let cancel = CancellationToken::new();
    assert!(
        preflight(
            &locations,
            &[file("game/authenticated.sav")],
            &protected,
            &cancel
        )
        .is_err()
    );
    assert!(preflight(&locations, &[file("GAME/new.sav")], &protected, &cancel).is_err());
    let mut broad = config();
    broad.include.clear();
    let broad = [Location::open(&broad, base.path()).unwrap()];
    assert!(preflight(&broad, &[file("game")], &protected, &cancel).is_err());
    assert!(
        preflight(
            &locations,
            &[file("game-other/slot.sav")],
            &protected,
            &cancel
        )
        .is_ok()
    );
    let ancestor = ProtectedInstalls::open([base.path()]).unwrap();
    assert!(preflight(&locations, &[file("slot.sav")], &ancestor, &cancel).is_err());
    assert_eq!(
        std::fs::read(install.join("authenticated.sav")).unwrap(),
        b"signed"
    );
}
#[test]
fn bounds_cancellation_and_root_aliases_fail_closed() {
    let base = tempfile::tempdir().unwrap();
    let locations = [Location::open(&config(), base.path()).unwrap()];
    let protected = ProtectedInstalls::open(std::iter::empty()).unwrap();
    let cancel = CancellationToken::new();
    for size in [-1, scan::MAX_BYTES as i64 + 1] {
        let mut oversized = file("slot.sav");
        oversized.size = size;
        assert!(preflight(&locations, &[oversized], &protected, &cancel).is_err());
    }
    let mut invalid = file("slot.sav");
    invalid.blake3 = "0".repeat(63);
    assert!(preflight(&locations, &[invalid], &protected, &cancel).is_err());
    let too_many = vec![file("slot.sav"); scan::MAX_FILES + 1];
    assert!(matches!(
        preflight(&locations, &too_many, &protected, &cancel),
        Err(Error::Limit)
    ));
    let mut alias = config();
    alias.id = "alias".into();
    alias.path = "saves/nested".into();
    let overlap = [
        Location::open(&config(), base.path()).unwrap(),
        Location::open(&alias, base.path()).unwrap(),
    ];
    assert!(preflight(&overlap, &[], &protected, &cancel).is_err());
    cancel.cancel();
    assert!(matches!(
        preflight(&locations, &[], &protected, &cancel),
        Err(Error::Cancelled)
    ));
}
#[cfg(unix)]
#[test]
fn native_inventory_canonicalizes_install_aliases() {
    let base = tempfile::tempdir().unwrap();
    std::fs::create_dir(base.path().join("saves")).unwrap();
    std::os::unix::fs::symlink(base.path().join("saves"), base.path().join("alias")).unwrap();
    let locations = [Location::open(&config(), base.path()).unwrap()];
    let protected = ProtectedInstalls::open([base.path().join("alias").as_path()]).unwrap();
    assert!(
        preflight(
            &locations,
            &[file("slot.sav")],
            &protected,
            &CancellationToken::new()
        )
        .is_err()
    );
    let all = ProtectedInstalls::open([Path::new("/")]).unwrap();
    assert!(preflight(&locations, &[file("slot.sav")], &all, &CancellationToken::new()).is_err());
}
