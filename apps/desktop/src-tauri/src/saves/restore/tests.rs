use std::path::Path;
use std::time::Duration;

use time::OffsetDateTime;
use vgames_core::manifest::{SaveBase, SaveLocation};

use super::*;

fn config() -> SaveLocation {
    SaveLocation {
        id: "game".into(),
        base: SaveBase::Documents,
        path: "game".into(),
        include: vec!["**/*.sav".into()],
        exclude: vec!["cache/**".into()],
    }
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn file(path: &str, bytes: &[u8]) -> SaveFile {
    SaveFile {
        root: "game".into(),
        path: path.into(),
        size: i64::try_from(bytes.len()).unwrap(),
        blake3: hash(bytes),
        mtime: OffsetDateTime::UNIX_EPOCH,
    }
}

fn dir(path: &Path) -> Dir {
    std::fs::create_dir_all(path).unwrap();
    Dir::open_ambient_dir(path, cap_std::ambient_authority()).unwrap()
}

fn stage(staging: &Path, contents: &[&[u8]]) {
    std::fs::create_dir_all(staging).unwrap();
    for bytes in contents {
        std::fs::write(staging.join(hash(bytes)), bytes).unwrap();
    }
}

fn plan(locations: &[Location], writes: Vec<SaveFile>, deletes: Vec<SaveFile>) -> Plan {
    let protected = ProtectedInstalls::open(std::iter::empty()).unwrap();
    Plan::new(
        locations,
        writes,
        deletes,
        &protected,
        &CancellationToken::new(),
    )
    .unwrap()
}

fn temps(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| scan::is_temp_name(name))
        .collect()
}

#[test]
fn restore_backs_up_replaces_deletes_and_rolls_back_exactly() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("docs/game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("slot.sav"), b"old slot").unwrap();
    std::fs::write(game.join("gone.sav"), b"remove me").unwrap();
    let locations = [Location::open(&config(), &root.path().join("docs")).unwrap()];
    stage(&root.path().join("staging"), &[b"new slot", b"fresh"]);
    let plan = plan(
        &locations,
        vec![file("slot.sav", b"new slot"), file("dlc/new.sav", b"fresh")],
        vec![file("gone.sav", b"remove me")],
    );
    let backups = dir(&root.path().join("backups"));
    let cancel = CancellationToken::new();

    let backup = back_up(&locations, &plan, &backups, &cancel).unwrap();
    assert_eq!(read_backup(&backups, backup.id).unwrap(), backup);
    let saved: Vec<_> = backup
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.original.clone()))
        .collect();
    assert_eq!(
        saved,
        [
            (
                "slot.sav",
                Some(Original {
                    size: 8,
                    blake3: hash(b"old slot")
                })
            ),
            ("dlc/new.sav", None),
            (
                "gone.sav",
                Some(Original {
                    size: 9,
                    blake3: hash(b"remove me")
                })
            ),
        ]
    );

    apply(
        &locations,
        &plan,
        &dir(&root.path().join("staging")),
        &cancel,
    )
    .unwrap();
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"new slot");
    assert_eq!(std::fs::read(game.join("dlc/new.sav")).unwrap(), b"fresh");
    assert!(!game.join("gone.sav").exists());
    assert!(temps(&game).is_empty());

    roll_back(&locations, &backup, &backups).unwrap();
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"old slot");
    assert_eq!(std::fs::read(game.join("gone.sav")).unwrap(), b"remove me");
    assert!(!game.join("dlc/new.sav").exists());
    // Rolling back twice (an interrupted roll back is rerun) changes nothing.
    roll_back(&locations, &backup, &backups).unwrap();
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"old slot");
}

#[test]
fn a_wrong_or_missing_staged_blob_leaves_the_target_untouched() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("docs/game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("slot.sav"), b"old slot").unwrap();
    let locations = [Location::open(&config(), &root.path().join("docs")).unwrap()];
    let staging = root.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    // Named for "new slot" but holding other bytes, as a tampered download would.
    std::fs::write(staging.join(hash(b"new slot")), b"evil bits").unwrap();
    let plan = plan(&locations, vec![file("slot.sav", b"new slot")], vec![]);
    let cancel = CancellationToken::new();
    assert!(matches!(
        apply(&locations, &plan, &dir(&staging), &cancel),
        Err(Error::Mismatch)
    ));
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"old slot");
    assert!(temps(&game).is_empty());

    std::fs::remove_file(staging.join(hash(b"new slot"))).unwrap();
    assert!(matches!(
        apply(&locations, &plan, &dir(&staging), &cancel),
        Err(Error::Mismatch)
    ));
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"old slot");

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        apply(&locations, &plan, &dir(&staging), &cancelled),
        Err(Error::Cancelled)
    ));
}

#[test]
fn plans_refuse_filtered_and_temporary_names() {
    let root = tempfile::tempdir().unwrap();
    let locations = [Location::open(&config(), root.path()).unwrap()];
    let protected = ProtectedInstalls::open(std::iter::empty()).unwrap();
    let cancel = CancellationToken::new();
    for path in ["cache/x.sav", ".vgames-0.tmp", "notes.txt"] {
        assert!(
            Plan::new(
                &locations,
                vec![file(path, b"x")],
                vec![],
                &protected,
                &cancel
            )
            .is_err(),
            "{path}"
        );
    }
}

#[cfg(unix)]
#[test]
fn links_in_the_save_tree_are_never_followed() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("docs/game");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("x.sav"), b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, game.join("dir")).unwrap();
    std::os::unix::fs::symlink(outside.join("x.sav"), game.join("slot.sav")).unwrap();
    let locations = [Location::open(&config(), &root.path().join("docs")).unwrap()];
    let staging = root.path().join("staging");
    stage(&staging, &[b"new"]);
    let backups = dir(&root.path().join("backups"));
    let cancel = CancellationToken::new();

    let through_dir = plan(&locations, vec![file("dir/x.sav", b"new")], vec![]);
    assert!(matches!(
        back_up(&locations, &through_dir, &backups, &cancel),
        Err(Error::Link)
    ));
    assert!(matches!(
        apply(&locations, &through_dir, &dir(&staging), &cancel),
        Err(Error::Link)
    ));
    let onto_link = plan(&locations, vec![file("slot.sav", b"new")], vec![]);
    assert!(matches!(
        back_up(&locations, &onto_link, &backups, &cancel),
        Err(Error::Link)
    ));
    assert_eq!(std::fs::read(outside.join("x.sav")).unwrap(), b"outside");
}

#[test]
fn prune_keeps_the_newest_backups_and_protected_ones() {
    let root = tempfile::tempdir().unwrap();
    let backups = dir(&root.path().join("backups"));
    let ids: Vec<Uuid> = (0..8)
        .map(|_| {
            let id = Uuid::now_v7();
            std::fs::create_dir_all(root.path().join("backups").join(id.to_string())).unwrap();
            std::thread::sleep(Duration::from_millis(2));
            id
        })
        .collect();
    std::fs::write(root.path().join("backups/notes"), b"not a backup").unwrap();
    prune(&backups, KEEP_BACKUPS, &[ids[0]]).unwrap();
    let mut left: Vec<Uuid> = std::fs::read_dir(root.path().join("backups"))
        .unwrap()
        .filter_map(|entry| Uuid::parse_str(&entry.unwrap().file_name().to_string_lossy()).ok())
        .collect();
    left.sort();
    let mut expected = vec![ids[0]];
    expected.extend(&ids[3..]);
    assert_eq!(left, expected);
    assert!(root.path().join("backups/notes").exists());
}

#[test]
fn tampered_manifests_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let backups = dir(&root.path().join("backups"));
    let id = Uuid::now_v7();
    let own = root.path().join("backups").join(id.to_string());
    std::fs::create_dir_all(&own).unwrap();
    let bad = Backup {
        id,
        entries: vec![Entry {
            root: "game".into(),
            path: "../escape.sav".into(),
            original: None,
        }],
    };
    std::fs::write(own.join(MANIFEST), serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(matches!(read_backup(&backups, id), Err(Error::Invalid)));
    assert!(read_backup(&backups, Uuid::now_v7()).is_err());
}

const CRASH_ENV: &str = "VGAMES_SAVE_RESTORE_CRASH_CHILD";
const CRASH_FILES: [&str; 3] = ["a.sav", "deep/b.sav", "c.sav"];

fn crash_content(round: u8, index: usize) -> Vec<u8> {
    vec![round.wrapping_mul(31).wrapping_add(index as u8); 1 << 20]
}

/// Child side of `killed_restores_leave_whole_files_and_recover`: restores
/// alternating snapshots until killed. Does nothing in a normal test run.
#[test]
fn restore_crash_child() {
    let Some(root) = std::env::var_os(CRASH_ENV) else {
        return;
    };
    let root = Path::new(&root);
    let locations = [Location::open(&config(), &root.join("docs")).unwrap()];
    let backups = dir(&root.join("backups"));
    let staging = root.join("staging");
    let cancel = CancellationToken::new();
    for round in 0..u64::MAX {
        let generation = u8::try_from(round % 2).unwrap();
        let contents: Vec<Vec<u8>> = (0..CRASH_FILES.len())
            .map(|index| crash_content(generation, index))
            .collect();
        stage(
            &staging,
            &contents.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        );
        let writes = CRASH_FILES
            .iter()
            .zip(&contents)
            .map(|(path, bytes)| file(path, bytes))
            .collect();
        let plan = plan(&locations, writes, vec![]);
        back_up(&locations, &plan, &backups, &cancel).unwrap();
        apply(&locations, &plan, &dir(&staging), &cancel).unwrap();
        prune(&backups, KEEP_BACKUPS, &[]).unwrap();
        if round == 1 {
            std::fs::write(root.join("started"), b"").unwrap();
        }
    }
}

#[test]
fn killed_restores_leave_whole_files_and_recover() {
    if std::env::var_os(CRASH_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    for round in 0..5u64 {
        let root = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(&exe)
            .args([
                "saves::restore::tests::restore_crash_child",
                "--exact",
                "--test-threads=1",
            ])
            .env(CRASH_ENV, root.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !root.path().join("started").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child never started restoring"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(15 + round * 23));
        child.kill().unwrap();
        child.wait().unwrap();

        let game = root.path().join("docs/game");
        let whole = |path: &str| {
            let bytes = std::fs::read(game.join(path)).unwrap();
            let index = CRASH_FILES.iter().position(|p| *p == path).unwrap();
            assert!(
                (0..2).any(|generation| bytes == crash_content(generation, index)),
                "{path} is partial after a kill"
            );
        };
        CRASH_FILES.iter().for_each(|path| whole(path));

        // Recovery: the newest complete backup restores whole files and
        // removes temporary files left by the kill.
        let locations = [Location::open(&config(), &root.path().join("docs")).unwrap()];
        let backups = dir(&root.path().join("backups"));
        let newest = std::fs::read_dir(root.path().join("backups"))
            .unwrap()
            .filter_map(|entry| Uuid::parse_str(&entry.unwrap().file_name().to_string_lossy()).ok())
            .filter_map(|id| read_backup(&backups, id).ok())
            .max_by_key(|backup| backup.id)
            .expect("a complete backup");
        roll_back(&locations, &newest, &backups).unwrap();
        CRASH_FILES.iter().for_each(|path| whole(path));
        assert!(temps(&game).is_empty());
        assert!(temps(&game.join("deep")).is_empty());
    }
}
