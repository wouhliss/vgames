use proptest::prelude::*;
use serde_json::{Value, json};

use super::*;

const SERVER: &str = "01920000-0000-7000-8000-000000000000";
const PACKAGE: &str = "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b";
const VERSION: &str = "0192a6f1-aaaa-7bbb-8ccc-dddddddddddd";

/// Builds a valid manifest the way the packer does: sorted files, the shared
/// layout, raw chunks, packs of at most `pack_size`.
pub(crate) fn build(files: &[(&str, u64)], pack_size: u64) -> Manifest {
    let mut files: Vec<(String, u64)> = files.iter().map(|(p, s)| (p.to_string(), *s)).collect();
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let sizes: Vec<u64> = files.iter().map(|f| f.1).collect();
    let l = layout::assign_chunks(&sizes, CHUNK_SIZE).unwrap();
    let stored: Vec<u64> = l.chunks.iter().map(|c| c.size).collect();
    let (packs, placements) = layout::packs(&stored, pack_size).unwrap();
    Manifest {
        format: FORMAT.into(),
        server_id: SERVER.parse().unwrap(),
        package_id: PACKAGE.parse().unwrap(),
        version_id: VERSION.parse().unwrap(),
        sequence: 12,
        version_label: "1.4.2".into(),
        platform: Platform::WindowsX86_64,
        created_at: "2026-09-24T10:00:00Z".parse().unwrap(),
        chunk_size: CHUNK_SIZE,
        totals: Totals {
            files: files.len() as u64,
            bytes: sizes.iter().sum(),
            chunks: l.chunks.len() as u64,
            packs: packs.len() as u64,
        },
        packs: packs
            .iter()
            .enumerate()
            .map(|(i, p)| Pack {
                size: p.size,
                blake3: Digest::of(format!("pack{i}").as_bytes()),
            })
            .collect(),
        chunks: l
            .chunks
            .iter()
            .zip(&placements)
            .enumerate()
            .map(|(i, (c, pl))| Chunk {
                pack: pl.pack,
                offset: pl.offset,
                stored_size: c.size,
                size: c.size,
                encoding: Encoding::Raw,
                blake3: Digest::of(format!("chunk{i}").as_bytes()),
            })
            .collect(),
        files: files
            .iter()
            .zip(&l.files)
            .map(|((path, size), slot)| File {
                path: path.clone(),
                size: *size,
                blake3: if *size == 0 {
                    EMPTY_BLAKE3
                } else {
                    Digest::of(path.as_bytes())
                },
                executable: path.ends_with(".exe"),
                chunk: slot.chunk,
                offset: slot.offset,
            })
            .collect(),
        directories: vec![],
        launch: None,
        controllers: None,
        saves: None,
        multiplayer: None,
    }
}

fn example() -> Manifest {
    let mut m = build(
        &[
            ("Game/Binaries/Win64/Game.exe", 104_857_600),
            ("Game/config.ini", 4096),
            ("Game/readme.txt", 4096),
            ("Game/empty.flag", 0),
        ],
        PACK_SIZE,
    );
    m.directories = vec!["Game/Saved".into()];
    m.launch = Some(Launch {
        default: "play".into(),
        targets: vec![LaunchTarget {
            id: "play".into(),
            label: "Play".into(),
            executable: "Game/Binaries/Win64/Game.exe".into(),
            args: vec!["-dx12".into()],
            working_dir: Some("Game/Binaries/Win64".into()),
            env: [("GAME_LANG".to_string(), "en".to_string())].into(),
        }],
    });
    m.controllers = Some(Controllers {
        supported: vec![ControllerKind::Xinput],
        emulate_as: Some(EmulatedController::Xbox360),
    });
    m.saves = Some(Saves {
        locations: vec![SaveLocation {
            id: "main".into(),
            base: SaveBase::Documents,
            path: "My Games/Example/Saves".into(),
            include: vec!["**/*".into()],
            exclude: vec!["**/*.log".into()],
        }],
    });
    m.multiplayer = Some(Multiplayer {
        join: Join {
            target: "play".into(),
            args: vec!["+connect".into(), JOIN_SECRET_PLACEHOLDER.into()],
        },
    });
    m
}

fn bytes(m: &Manifest) -> Vec<u8> {
    serde_json::to_vec(m).unwrap()
}

fn check(m: &Manifest) -> Result<Manifest, ManifestError> {
    parse_and_validate(&bytes(m))
}

fn check_json(v: &Value) -> Result<Manifest, ManifestError> {
    parse_and_validate(&serde_json::to_vec(v).unwrap())
}

fn with(f: impl FnOnce(&mut Manifest)) -> Result<Manifest, ManifestError> {
    let mut m = example();
    f(&mut m);
    check(&m)
}

fn with_json(f: impl FnOnce(&mut Value)) -> Result<Manifest, ManifestError> {
    let mut v = serde_json::to_value(example()).unwrap();
    f(&mut v);
    check_json(&v)
}

#[test]
fn example_is_valid_and_roundtrips() {
    let m = example();
    assert_eq!(check(&m).unwrap(), m);
    assert_eq!(m.totals.chunks, 26);
}

#[test]
fn accepts_packer_output() {
    // Exact bytes produced by vgames-pack (crates/vgames-pack/tests/snapshots).
    let m = parse_and_validate(include_bytes!("../../tests/vectors/pack_manifest.json")).unwrap();
    assert_eq!(m.files.len(), 4);
    assert_eq!(m.chunks[2].encoding, Encoding::Zstd);
}

#[test]
fn wire_format_snapshot() {
    insta::assert_snapshot!(String::from_utf8(bytes(&example())).unwrap());
}

#[test]
fn empty_package_is_valid() {
    let m = build(&[("a", 0)], PACK_SIZE);
    check(&m).unwrap();
    assert!(m.packs.is_empty());
}

// ---- one negative test per rule --------------------------------------------

#[test]
fn rejects_oversized_and_malformed_json() {
    assert!(matches!(
        parse_and_validate(b"{"),
        Err(ManifestError::Json(_))
    ));
    assert!(matches!(
        with_json(|v| v["unexpected"] = json!(1)),
        Err(ManifestError::Json(_))
    ));
    assert!(matches!(
        with_json(|v| v["launch"]["targets"][0]["shell"] = json!(true)),
        Err(ManifestError::Json(_))
    ));
    let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
    assert!(matches!(
        parse_and_validate(&big),
        Err(ManifestError::TooLarge { .. })
    ));
}

#[test]
fn rejects_duplicate_json_keys() {
    // serde refuses duplicate struct fields; env maps use `unique_map`.
    let good = String::from_utf8(bytes(&example())).unwrap();
    let dup_field = good.replacen("\"sequence\":12", "\"sequence\":12,\"sequence\":13", 1);
    assert!(matches!(
        parse_and_validate(dup_field.as_bytes()),
        Err(ManifestError::Json(_))
    ));
    let dup_env = good.replacen(
        "\"GAME_LANG\":\"en\"",
        "\"GAME_LANG\":\"en\",\"GAME_LANG\":\"fr\"",
        1,
    );
    assert_ne!(dup_env, good);
    assert!(matches!(
        parse_and_validate(dup_env.as_bytes()),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn rule_format() {
    assert!(matches!(
        with(|m| m.format = "vgames.manifest/2".into()),
        Err(ManifestError::Format(_))
    ));
}

#[test]
fn rule_uuids_canonical() {
    for bad in [
        "01920000000070008000000000000000",
        "01920000-0000-7000-8000-00000000000G",
        "01920000-0000-7000-8000-00000000000A",
        "{01920000-0000-7000-8000-000000000000}",
        "00000000-0000-0000-0000-000000000000",
    ] {
        for field in ["server_id", "package_id", "version_id"] {
            assert!(
                matches!(
                    with_json(|v| v[field] = json!(bad)),
                    Err(ManifestError::Json(_))
                ),
                "{field} {bad}"
            );
        }
    }
}

#[test]
fn rule_sequence_positive() {
    assert_eq!(with(|m| m.sequence = 0), Err(ManifestError::ZeroSequence));
    assert!(matches!(
        with_json(|v| v["sequence"] = json!(-1)),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn rule_platform_known() {
    assert!(matches!(
        with_json(|v| v["platform"] = json!("windows-arm")),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn rule_version_label() {
    for bad in [String::new(), "x".repeat(65), "1.0\n".into()] {
        assert_eq!(
            with(|m| m.version_label = bad),
            Err(ManifestError::VersionLabel)
        );
    }
    with(|m| m.version_label = "é".repeat(64)).unwrap();
}

#[test]
fn rule_created_at_utc() {
    assert!(matches!(
        with_json(|v| v["created_at"] = json!("2026-09-24T12:00:00+02:00")),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn rule_chunk_size() {
    assert_eq!(
        with(|m| m.chunk_size = 1 << 20),
        Err(ManifestError::ChunkSize(1 << 20))
    );
}

#[test]
fn rule_totals() {
    for field in ["files", "bytes", "chunks", "packs"] {
        let r = with_json(|v| {
            let n = v["totals"][field].as_u64().unwrap();
            v["totals"][field] = json!(n + 1);
        });
        assert!(
            matches!(r, Err(ManifestError::Totals { field: f, .. }) if f == field),
            "{field}: {r:?}"
        );
    }
}

#[test]
fn rule_chunk_contiguous_offsets() {
    let r = with(|m| m.chunks[1].offset += 1);
    assert!(matches!(
        r,
        Err(ManifestError::Chunk {
            index: 1,
            fault: ChunkFault::Offset { .. }
        })
    ));
    // The first chunk of a pack starts at 0.
    let mut m = build(&[("a", CHUNK_SIZE), ("b", CHUNK_SIZE)], CHUNK_SIZE);
    m.chunks[1].offset = 5;
    assert!(matches!(
        check(&m),
        Err(ManifestError::Chunk {
            index: 1,
            fault: ChunkFault::Offset { expected: 0, .. }
        })
    ));
}

#[test]
fn rule_chunk_pack_order() {
    let mut m = build(
        &[("a", CHUNK_SIZE), ("b", CHUNK_SIZE), ("c", CHUNK_SIZE)],
        CHUNK_SIZE,
    );
    check(&m).unwrap();
    m.chunks.swap(0, 1);
    m.chunks[0].offset = 0;
    assert!(matches!(
        check(&m),
        Err(ManifestError::Chunk {
            index: 0,
            fault: ChunkFault::PackOrder
        })
    ));
    let mut m = build(&[("a", CHUNK_SIZE)], PACK_SIZE);
    m.chunks[0].pack = 1;
    assert!(matches!(
        check(&m),
        Err(ManifestError::Chunk {
            index: 0,
            fault: ChunkFault::NoSuchPack(1)
        })
    ));
}

#[test]
fn rule_chunk_sizes() {
    let r = with(|m| m.chunks[0].size = CHUNK_SIZE + 1);
    assert!(matches!(
        r,
        Err(ManifestError::Chunk {
            index: 0,
            fault: ChunkFault::Size(_)
        })
    ));
    // zstd stored size may exceed size by at most 64 KiB.
    let mut m = build(&[("a", 1000)], PACK_SIZE);
    m.chunks[0].encoding = Encoding::Zstd;
    m.chunks[0].stored_size = 1000 + MAX_STORED_OVERHEAD;
    m.packs[0].size = m.chunks[0].stored_size;
    check(&m).unwrap();
    m.chunks[0].stored_size += 1;
    m.packs[0].size += 1;
    assert!(matches!(
        check(&m),
        Err(ManifestError::Chunk {
            fault: ChunkFault::StoredSize { .. },
            ..
        })
    ));
    // raw ⇒ stored_size == size.
    let mut m = build(&[("a", 1000)], PACK_SIZE);
    m.chunks[0].stored_size = 999;
    m.packs[0].size = 999;
    assert!(matches!(
        check(&m),
        Err(ManifestError::Chunk {
            fault: ChunkFault::RawStoredSize,
            ..
        })
    ));
}

#[test]
fn rule_chunk_size_matches_layout() {
    let mut m = build(&[("a", 10), ("b", 20)], PACK_SIZE);
    m.chunks[0].size = 31;
    m.chunks[0].stored_size = 31;
    m.packs[0].size = 31;
    m.totals.bytes = 30;
    assert!(matches!(
        check(&m),
        Err(ManifestError::Chunk {
            fault: ChunkFault::LayoutSize {
                expected: 30,
                found: 31
            },
            ..
        })
    ));
}

#[test]
fn rule_pack_size_and_limit() {
    let r = with(|m| m.packs[0].size += 1);
    assert!(matches!(
        r,
        Err(ManifestError::Pack {
            index: 0,
            fault: PackFault::Size { .. }
        })
    ));
    // 65 full chunks in one pack: 260 MiB > 256 MiB.
    let m = build(&[("a", 65 * CHUNK_SIZE)], 65 * CHUNK_SIZE);
    assert!(matches!(
        check(&m),
        Err(ManifestError::Pack {
            fault: PackFault::TooLarge(_),
            ..
        })
    ));
    // A pack without chunks.
    let mut m = build(&[("a", 10)], PACK_SIZE);
    m.packs.push(m.packs[0].clone());
    m.totals.packs = 2;
    assert!(matches!(
        check(&m),
        Err(ManifestError::Pack {
            index: 1,
            fault: PackFault::Empty
        })
    ));
}

#[test]
fn rule_files_sorted() {
    let mut m = build(&[("a", 1), ("b", 1)], PACK_SIZE);
    m.files.swap(0, 1);
    assert!(matches!(
        check(&m),
        Err(ManifestError::File {
            index: 1,
            fault: FileFault::Order,
            ..
        })
    ));
    let mut m = build(&[("a", 1), ("b", 1)], PACK_SIZE);
    m.files[1].path = "a".into();
    assert!(matches!(
        check(&m),
        Err(ManifestError::File {
            fault: FileFault::Order,
            ..
        })
    ));
}

#[test]
fn rule_files_match_layout() {
    let r = with(|m| m.files[3].offset += 1);
    assert!(matches!(
        r,
        Err(ManifestError::File {
            fault: FileFault::Layout { .. },
            ..
        })
    ));
    let r = with(|m| m.files[0].chunk = Some(1));
    assert!(matches!(
        r,
        Err(ManifestError::File {
            fault: FileFault::Layout { .. },
            ..
        })
    ));
    // A size change that the declared chunks cannot hold.
    let r = with(|m| {
        m.files[0].size += CHUNK_SIZE;
        m.totals.bytes += CHUNK_SIZE;
    });
    assert!(matches!(
        r,
        Err(ManifestError::File {
            fault: FileFault::TooManyChunks,
            ..
        })
    ));
    // Hostile sizes fail before any allocation proportional to them.
    let r = with(|m| {
        m.files[0].size = u64::MAX - 10_000;
        m.totals.bytes = u64::MAX - 1;
    });
    assert!(r.is_err());
}

#[test]
fn rule_empty_files() {
    let mut m = build(&[("a", 0), ("b", 5)], PACK_SIZE);
    m.files[0].blake3 = Digest::of(b"x");
    assert!(matches!(
        check(&m),
        Err(ManifestError::File {
            index: 0,
            fault: FileFault::Empty,
            ..
        })
    ));
    let mut m = build(&[("a", 0), ("b", 5)], PACK_SIZE);
    m.files[0].chunk = Some(0);
    assert!(matches!(
        check(&m),
        Err(ManifestError::File {
            index: 0,
            fault: FileFault::Empty,
            ..
        })
    ));
}

#[test]
fn rule_paths_valid() {
    let mut m = build(&[("a/../b", 1)], PACK_SIZE);
    assert!(matches!(
        check(&m),
        Err(ManifestError::Path(PathError::BadStructure(_)))
    ));
    m = build(&[(".vgames/install.json", 1)], PACK_SIZE);
    assert!(matches!(
        check(&m),
        Err(ManifestError::Path(PathError::ReservedRoot(_)))
    ));
    m = build(&[("A", 1), ("a", 1)], PACK_SIZE);
    assert!(matches!(
        check(&m),
        Err(ManifestError::Path(PathError::CaseCollision(..)))
    ));
}

#[test]
fn rule_directories_valid_and_not_files() {
    let r = with(|m| m.directories.push("Game/config.ini".into()));
    assert!(matches!(
        r,
        Err(ManifestError::Path(PathError::Duplicate(_)))
    ));
    let r = with(|m| m.directories.push("Game/config.ini/x".into()));
    assert!(matches!(
        r,
        Err(ManifestError::Path(PathError::FileIsDirectory(_)))
    ));
    let r = with(|m| m.directories.push("CON".into()));
    assert!(matches!(
        r,
        Err(ManifestError::Path(PathError::ReservedName(_)))
    ));
}

fn target(m: &mut Manifest) -> &mut LaunchTarget {
    &mut m.launch.as_mut().unwrap().targets[0]
}

#[test]
fn rule_launch_targets() {
    let r = with(|m| target(m).executable = "Game/missing.exe".into());
    assert!(matches!(
        r,
        Err(ManifestError::LaunchTarget {
            fault: LaunchFault::Executable(_),
            ..
        })
    ));
    // A directory is not an executable.
    let r = with(|m| target(m).executable = "Game/Binaries".into());
    assert!(matches!(
        r,
        Err(ManifestError::LaunchTarget {
            fault: LaunchFault::Executable(_),
            ..
        })
    ));
    let r = with(|m| target(m).working_dir = Some("Game/Nope".into()));
    assert!(matches!(
        r,
        Err(ManifestError::LaunchTarget {
            fault: LaunchFault::WorkingDir(_),
            ..
        })
    ));
    // Listed (empty) directories are valid working directories.
    with(|m| target(m).working_dir = Some("Game/Saved".into())).unwrap();
    let r = with(|m| target(m).working_dir = Some("Game/config.ini".into()));
    assert!(matches!(
        r,
        Err(ManifestError::LaunchTarget {
            fault: LaunchFault::WorkingDir(_),
            ..
        })
    ));
    let r = with(|m| m.launch.as_mut().unwrap().default = "other".into());
    assert!(matches!(
        r,
        Err(ManifestError::Launch(LaunchFault::UnknownDefault(_)))
    ));
    let r = with(|m| m.launch.as_mut().unwrap().targets.clear());
    assert_eq!(r, Err(ManifestError::Launch(LaunchFault::NoTargets)));
    let r = with(|m| {
        let t = target(m).clone();
        m.launch.as_mut().unwrap().targets.push(t);
    });
    assert!(matches!(
        r,
        Err(ManifestError::LaunchTarget {
            index: 1,
            fault: LaunchFault::DuplicateId(_)
        })
    ));
    let r = with(|m| target(m).id = String::new());
    assert!(matches!(
        r,
        Err(ManifestError::LaunchTarget {
            fault: LaunchFault::Id,
            ..
        })
    ));
    let r = with(|m| target(m).label = "x".repeat(65));
    assert!(matches!(
        r,
        Err(ManifestError::LaunchTarget {
            fault: LaunchFault::Label,
            ..
        })
    ));
}

#[test]
fn rule_launch_args_limit() {
    with(|m| target(m).args = vec!["a".repeat(MAX_ARGS_BYTES)]).unwrap();
    for args in [
        vec!["a".repeat(MAX_ARGS_BYTES), "b".into()],
        vec!["a\0b".into()],
    ] {
        let r = with(|m| target(m).args = args);
        assert!(matches!(
            r,
            Err(ManifestError::LaunchTarget {
                fault: LaunchFault::Args,
                ..
            })
        ));
    }
}

#[test]
fn rule_env_keys_and_values() {
    for (key, ok) in [
        ("GAME_LANG", true),
        ("_X", true),
        ("A1", true),
        (&*"A".repeat(64), true),
        (&*"A".repeat(65), false),
        ("lower", false),
        ("1ABC", false),
        ("A-B", false),
        ("", false),
    ] {
        let r = with(|m| target(m).env = [(key.to_string(), "v".to_string())].into());
        assert_eq!(r.is_ok(), ok, "{key}: {r:?}");
        if !ok {
            assert!(matches!(
                r,
                Err(ManifestError::Env {
                    fault: EnvFault::KeySyntax(_),
                    ..
                })
            ));
        }
    }
    for key in [
        "PATH",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_",
        "PYTHONPATH",
        "COMSPEC",
    ] {
        let r = with(|m| target(m).env = [(key.to_string(), "v".to_string())].into());
        assert!(
            matches!(
                r,
                Err(ManifestError::Env {
                    fault: EnvFault::KeyDenied(_),
                    ..
                })
            ),
            "{key}"
        );
    }
    let r = with(|m| target(m).env = [("A".to_string(), "x\0y".to_string())].into());
    assert!(matches!(
        r,
        Err(ManifestError::Env {
            fault: EnvFault::Value(_),
            ..
        })
    ));
}

fn save(m: &mut Manifest) -> &mut SaveLocation {
    &mut m.saves.as_mut().unwrap().locations[0]
}

#[test]
fn rule_saves() {
    assert!(matches!(
        with_json(|v| v["saves"]["locations"][0]["base"] = json!("system32")),
        Err(ManifestError::Json(_))
    ));
    for base in [
        "install",
        "home",
        "documents",
        "saved_games",
        "appdata",
        "localappdata",
        "xdg_data",
        "xdg_config",
    ] {
        with_json(|v| v["saves"]["locations"][0]["base"] = json!(base)).unwrap();
    }
    let r = with(|m| save(m).path = "../../etc".into());
    assert!(matches!(
        r,
        Err(ManifestError::Save {
            fault: SaveFault::Path(_),
            ..
        })
    ));
    for bad in ["", "/abs/*", "../*", "a/../../*", "a\\b"] {
        let r = with(|m| save(m).include = vec![bad.into()]);
        assert!(
            matches!(
                r,
                Err(ManifestError::Save {
                    fault: SaveFault::Pattern(_),
                    ..
                })
            ),
            "{bad:?}"
        );
    }
    let r = with(|m| {
        let l = save(m).clone();
        m.saves.as_mut().unwrap().locations.push(l);
    });
    assert!(matches!(
        r,
        Err(ManifestError::Save {
            index: 1,
            fault: SaveFault::DuplicateId(_)
        })
    ));
}

#[test]
fn rule_controllers() {
    assert!(matches!(
        with_json(|v| v["controllers"]["supported"] = json!(["xinput", "wiimote"])),
        Err(ManifestError::Json(_))
    ));
    assert!(matches!(
        with_json(|v| v["controllers"]["emulate_as"] = json!("dualsense")),
        Err(ManifestError::Json(_))
    ));
    with_json(|v| {
        v["controllers"] =
            json!({ "supported": ["xinput", "dualshock4", "dualsense", "switch_pro", "generic"] })
    })
    .unwrap();
    assert_eq!(
        with(|m| m.controllers.as_mut().unwrap().supported = vec![ControllerKind::Xinput; 2]),
        Err(ManifestError::DuplicateController(ControllerKind::Xinput))
    );
}

#[test]
fn rule_join_placeholder_whole_argument() {
    fn join(m: &mut Manifest) -> &mut Join {
        &mut m.multiplayer.as_mut().unwrap().join
    }
    for arg in ["+connect={join_secret}", "{join_secret}x", "x{join_secret}"] {
        let r = with(|m| join(m).args = vec![arg.into()]);
        assert_eq!(
            r,
            Err(ManifestError::Join(JoinFault::PartialPlaceholder)),
            "{arg}"
        );
    }
    let r = with(|m| join(m).target = "nope".into());
    assert!(matches!(
        r,
        Err(ManifestError::Join(JoinFault::UnknownTarget(_)))
    ));
    let r = with(|m| {
        m.launch = None;
    });
    assert!(matches!(
        r,
        Err(ManifestError::Join(JoinFault::UnknownTarget(_)))
    ));
}

// ---- properties -------------------------------------------------------------

fn tree() -> impl Strategy<Value = Vec<(String, u64)>> {
    let size = prop_oneof![
        3 => Just(0u64),
        10 => 1u64..5000,
        2 => (CHUNK_SIZE - 2)..(CHUNK_SIZE + 2),
        1 => CHUNK_SIZE..(3 * CHUNK_SIZE),
    ];
    prop::collection::btree_map("[a-d]{1,3}(/[a-d]{1,3}){0,2}\\.bin", size, 0..40)
        .prop_map(|m| m.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// The validator accepts every manifest the layout produces.
    #[test]
    fn accepts_every_layout(files in tree(), pack_chunks in 1u64..6) {
        let files: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        // Directories and files may clash in generated trees; skip those.
        let paths_ok = paths::validate_tree(files.iter().map(|f| f.0), []).is_ok();
        let m = build(&files, pack_chunks * CHUNK_SIZE);
        prop_assert_eq!(check(&m).is_ok(), paths_ok, "{:?}", check(&m));
    }

    /// Any single-field change that breaks a layout invariant is refused.
    #[test]
    fn rejects_single_mutations(files in tree(), which in 0usize..9, pick in any::<prop::sample::Index>()) {
        let files: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        prop_assume!(paths::validate_tree(files.iter().map(|f| f.0), []).is_ok());
        let mut m = build(&files, 3 * CHUNK_SIZE);
        prop_assume!(!m.chunks.is_empty());
        let c = pick.index(m.chunks.len());
        let f = pick.index(m.files.len());
        let p = pick.index(m.packs.len());
        match which {
            0 => m.chunks[c].offset += 1,
            1 => m.chunks[c].size += 1,
            2 => m.chunks[c].stored_size += 1,
            3 => m.chunks[c].pack += 1,
            4 => m.packs[p].size -= 1,
            5 => m.files[f].offset += 1,
            6 => m.files[f].chunk = Some(m.files[f].chunk.map_or(0, |x| x + 1)),
            7 => m.totals.bytes += 1,
            _ => m.files[f].size += 1,
        }
        prop_assert!(check(&m).is_err(), "mutation {} accepted", which);
    }

    /// Never panics on arbitrary bytes.
    #[test]
    fn never_panics(b in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = parse_and_validate(&b);
    }
}
