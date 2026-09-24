#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! A2-T03 acceptance: plan → generate packs → verify every chunk → reassemble
//! → byte-identical tree; determinism; resume offsets; source changes.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use proptest::prelude::*;
use uuid::Uuid;
use vgames_pack::manifest::{self, Execution, Launch, LaunchTarget, VersionIdentity};
use vgames_pack::scan::{self, FsReader};
use vgames_pack::source::{self, SourceError};
use vgames_pack::verify::{self, PackStreamVerifier};
use vgames_pack::{CHUNK_SIZE, Compression, PACK_SIZE, PackSource, Plan};

const MIB: u64 = 1024 * 1024;

fn identity() -> VersionIdentity {
    VersionIdentity {
        server_id: Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0000),
        package_id: Uuid::from_u128(0x0192_a6f0_1c2d_7e3f_8a9b_0c1d_2e3f_4a5b),
        version_id: Uuid::from_u128(0x0192_a6f1_aaaa_7bbb_8ccc_dddd_dddd_dddd),
        sequence: 12,
        version_label: "1.4.2".into(),
        platform: "windows-x86_64".into(),
        created_at: 1_790_244_000,
    }
}

/// Deterministic pseudo-random bytes; `compressible` repeats a short pattern.
fn content(seed: u64, len: u64, compressible: bool) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|i| {
            if compressible {
                b"vgames-save-data-"[(i % 17) as usize]
            } else {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            }
        })
        .collect()
}

fn write_tree(root: &Path, files: &[(String, u64, bool)]) {
    for (i, (path, size, compressible)) in files.iter().enumerate() {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content(i as u64 + 1, *size, *compressible)).unwrap();
    }
}

struct Packed {
    plan: Arc<Plan>,
    source: PackSource,
    manifest: Vec<u8>,
    packs: Vec<Vec<u8>>,
}

/// Plans and packs a folder, generating packs on parallel threads (so file
/// hashes are assembled out of order) and reading each in odd-sized pieces.
fn pack_folder(root: &Path, compression: Compression) -> Packed {
    let scanned = scan::scan(root).unwrap();
    assert!(scanned.rejected.is_empty());
    let plan = Arc::new(Plan::new(scanned.files, scanned.directories).unwrap());
    let reader = Arc::new(FsReader::new(root));
    let source = match compression {
        Compression::None => PackSource::new(
            plan.clone(),
            Arc::new(plan.raw_packing(PACK_SIZE).unwrap()),
            reader,
        ),
        Compression::Auto => {
            source::analyze(plan.clone(), reader, compression, PACK_SIZE, 3).unwrap()
        }
    };
    let count = source.packing.pack_count();
    let packs: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..count)
            .rev()
            .map(|i| {
                let source = source.clone();
                scope.spawn(move || {
                    let mut reader = source.open_pack(i, 0).unwrap();
                    let mut out = Vec::new();
                    let mut piece = vec![0u8; 1_000_003];
                    loop {
                        let n = reader.read(&mut piece).unwrap();
                        if n == 0 {
                            break;
                        }
                        out.extend_from_slice(&piece[..n]);
                    }
                    out
                })
            })
            .collect();
        let mut packs: Vec<Vec<u8>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        packs.reverse();
        packs
    });
    let hashes = source.hashes.finish().unwrap();
    let manifest = manifest::build(
        &identity(),
        &Execution::default(),
        &plan,
        &source.packing,
        &hashes,
    )
    .unwrap();
    Packed {
        plan,
        source,
        manifest,
        packs,
    }
}

/// Verifies every pack against the manifest and rebuilds the tree from the
/// decoded chunks, using only the manifest.
fn reassemble(manifest_bytes: &[u8], packs: &[Vec<u8>], out: &Path) {
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let files = manifest["files"].as_array().unwrap();
    let sizes: Vec<u64> = files.iter().map(|f| f["size"].as_u64().unwrap()).collect();
    let layout = vgames_pack::layout::assign_chunks(&sizes, CHUNK_SIZE).unwrap();
    let mut buffers: Vec<Vec<u8>> = sizes.iter().map(|&s| vec![0u8; s as usize]).collect();
    let mut verified = 0u32;

    for (expectation, bytes) in verify::expectations_from_manifest(manifest_bytes)
        .unwrap()
        .into_iter()
        .zip(packs)
    {
        let mut verifier = PackStreamVerifier::new(expectation);
        for piece in bytes.chunks(777_777) {
            verifier
                .push(piece, |verdict| {
                    let decoded = verdict.result.expect("every chunk verifies");
                    verified += 1;
                    let mut at = 0usize;
                    for (file, offset, len) in layout.extents(&sizes, verdict.index) {
                        let (offset, len) = (offset as usize, len as usize);
                        buffers[file as usize][offset..offset + len]
                            .copy_from_slice(&decoded[at..at + len]);
                        at += len;
                    }
                })
                .unwrap();
        }
        let report = verifier.finish().unwrap();
        assert!(report.first_failure.is_none());
    }
    assert_eq!(
        verified as usize,
        manifest["chunks"].as_array().unwrap().len()
    );

    for (file, bytes) in files.iter().zip(&buffers) {
        assert_eq!(
            file["blake3"].as_str().unwrap(),
            blake3::hash(bytes).to_hex().as_str()
        );
        let path = out.join(file["path"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    for dir in manifest["directories"].as_array().unwrap() {
        std::fs::create_dir_all(out.join(dir.as_str().unwrap())).unwrap();
    }
}

fn tree_contents(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|e| {
            let e = e.unwrap();
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let data = e
                .file_type()
                .is_file()
                .then(|| std::fs::read(e.path()).unwrap());
            (rel, data)
        })
        .collect()
}

fn assert_round_trip(files: &[(String, u64, bool)], empty_dirs: &[&str], compression: Compression) {
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), files);
    for d in empty_dirs {
        std::fs::create_dir_all(src.path().join(d)).unwrap();
    }
    let packed = pack_folder(src.path(), compression);
    let dst = tempfile::tempdir().unwrap();
    reassemble(&packed.manifest, &packed.packs, dst.path());
    assert_eq!(tree_contents(src.path()), tree_contents(dst.path()));
}

fn name_strategy() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "a", "B", "données", "日本", "ü", "x y", "file.bin", "z-9", "Ω",
    ])
    .prop_map(str::to_owned)
}

fn size_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(0),
        Just(1),
        1u64..5000,
        Just(CHUNK_SIZE - 1),
        Just(CHUNK_SIZE),
        Just(CHUNK_SIZE + 1),
        (2 * CHUNK_SIZE..3 * CHUNK_SIZE),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, .. ProptestConfig::default() })]

    #[test]
    fn random_trees_round_trip(
        entries in prop::collection::btree_map(
            (name_strategy(), prop::option::of(name_strategy())),
            (size_strategy(), any::<bool>()),
            0..12,
        ),
        auto in any::<bool>(),
    ) {
        // Files at "dir/name" or "name"; drop names that collide with a folder or by case.
        let mut files: Vec<(String, u64, bool)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for ((a, b), (size, compressible)) in entries {
            let path = match b { Some(b) => format!("d-{a}/{b}"), None => a };
            if seen.insert(path.to_lowercase()) {
                files.push((path, size, compressible));
            }
        }
        let compression = if auto { Compression::Auto } else { Compression::None };
        assert_round_trip(&files, &["empty/inner"], compression);
    }
}

#[test]
fn ten_thousand_tiny_files() {
    let files: Vec<(String, u64, bool)> = (0..10_000)
        .map(|i| {
            (
                format!("t/{:02}/{i:05}.txt", i % 50),
                (i % 7) as u64,
                i % 2 == 0,
            )
        })
        .collect();
    assert_round_trip(&files, &[], Compression::Auto);
}

#[test]
fn spans_several_packs() {
    // 70 chunks of 4 MiB plus a shared tail: two packs with raw storage.
    let files = vec![
        ("big.bin".to_owned(), 70 * CHUNK_SIZE + 5, false),
        ("small.txt".to_owned(), 100, true),
    ];
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), &files);
    let packed = pack_folder(src.path(), Compression::None);
    assert_eq!(packed.packs.len(), 2);
    assert!(packed.packs.iter().all(|p| p.len() as u64 <= PACK_SIZE));
    let dst = tempfile::tempdir().unwrap();
    reassemble(&packed.manifest, &packed.packs, dst.path());
    assert_eq!(tree_contents(src.path()), tree_contents(dst.path()));
}

#[test]
fn same_tree_gives_same_manifest() {
    let files = vec![
        ("Game/bin/game.exe".to_owned(), 9 * MIB + 3, false),
        ("Game/config.ini".to_owned(), 4096, true),
        ("Game/empty.flag".to_owned(), 0, false),
    ];
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write_tree(a.path(), &files);
    write_tree(b.path(), &files);
    // Same content, different mtimes: the manifest does not depend on them.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(b.path().join("Game/config.ini"), content(2, 4096, true)).unwrap();
    for compression in [Compression::None, Compression::Auto] {
        assert_eq!(
            pack_folder(a.path(), compression).manifest,
            pack_folder(b.path(), compression).manifest
        );
    }
}

#[test]
fn resumed_pack_stream_matches_the_tail() {
    // Mixed zstd and raw chunks, so resume points fall inside both kinds.
    let files = vec![
        ("a.bin".to_owned(), 3 * CHUNK_SIZE + 17, true),
        ("b.bin".to_owned(), 2 * CHUNK_SIZE, false),
        ("c.txt".to_owned(), 5000, true),
    ];
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), &files);
    let packed = pack_folder(src.path(), Compression::Auto);
    let full = &packed.packs[0];
    let len = full.len() as u64;
    for start in [
        0,
        1,
        700,
        len / 3,
        len / 2 + 1,
        len - CHUNK_SIZE,
        len - 1,
        len,
    ] {
        let mut tail = Vec::new();
        packed
            .source
            .open_pack(0, start)
            .unwrap()
            .read_to_end(&mut tail)
            .unwrap();
        assert_eq!(tail, full[start as usize..], "start {start}");
        // The pack hash still covers the whole pack.
        let hash = packed.source.hashes.pack_hash(0).unwrap();
        assert_eq!(hash, *blake3::hash(full).as_bytes());
    }
}

#[test]
fn changed_source_file_aborts() {
    let files = vec![("a.bin".to_owned(), CHUNK_SIZE + 10, false)];
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), &files);
    let scanned = scan::scan(src.path()).unwrap();
    let plan = Arc::new(Plan::new(scanned.files, scanned.directories).unwrap());
    let source = PackSource::new(
        plan.clone(),
        Arc::new(plan.raw_packing(PACK_SIZE).unwrap()),
        Arc::new(FsReader::new(src.path())),
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(src.path().join("a.bin"), content(9, CHUNK_SIZE + 10, false)).unwrap();
    let error = source
        .open_pack(0, 0)
        .unwrap()
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    let inner = error
        .into_inner()
        .unwrap()
        .downcast::<SourceError>()
        .unwrap();
    assert!(matches!(*inner, SourceError::Changed { ref path } if path == "a.bin"));
}

#[test]
fn flipped_byte_is_reported_for_its_chunk_only() {
    let files = vec![("a.bin".to_owned(), 2 * CHUNK_SIZE, false)];
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), &files);
    let packed = pack_folder(src.path(), Compression::None);
    let mut pack = packed.packs[0].clone();
    pack[CHUNK_SIZE as usize + 5] ^= 0x01;
    let expectation = verify::PackExpectation::from_manifest(&packed.manifest, 0).unwrap();
    let mut verifier = PackStreamVerifier::new(expectation);
    let mut results = Vec::new();
    verifier
        .push(&pack, |v| results.push((v.index, v.result.is_ok())))
        .unwrap();
    assert_eq!(results, vec![(0, true), (1, false)]);
    assert_eq!(verifier.finish(), Err(verify::PackError::HashMismatch));
}

#[test]
fn truncated_and_overlong_packs_are_rejected() {
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), &[("a".to_owned(), 1000, false)]);
    let packed = pack_folder(src.path(), Compression::None);
    let expectation = verify::PackExpectation::from_manifest(&packed.manifest, 0).unwrap();
    let mut short = PackStreamVerifier::new(expectation.clone());
    short.push(&packed.packs[0][..999], |_| {}).unwrap();
    assert!(matches!(
        short.finish(),
        Err(verify::PackError::Truncated { .. })
    ));
    let mut long = PackStreamVerifier::new(expectation);
    let mut bytes = packed.packs[0].clone();
    bytes.push(0);
    assert!(matches!(
        long.push(&bytes, |_| {}),
        Err(verify::PackError::TooLong { .. })
    ));
}

#[test]
fn manifest_snapshot() {
    let files = vec![
        (
            "Game/Binaries/Win64/Game.exe".to_owned(),
            CHUNK_SIZE + 10,
            false,
        ),
        ("Game/config.ini".to_owned(), 4096, true),
        ("Game/readme.txt".to_owned(), 4096, true),
        ("Game/empty.flag".to_owned(), 0, false),
    ];
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), &files);
    std::fs::create_dir_all(src.path().join("Game/Saved")).unwrap();
    let packed = pack_folder(src.path(), Compression::Auto);
    let hashes = packed.source.hashes.finish().unwrap();
    let execution = Execution {
        launch: Some(Launch {
            default: "play".into(),
            targets: vec![LaunchTarget {
                id: "play".into(),
                label: "Play".into(),
                executable: "Game/Binaries/Win64/Game.exe".into(),
                args: vec!["-dx12".into()],
                working_dir: Some("Game/Binaries/Win64".into()),
                env: BTreeMap::from([("GAME_LANG".into(), "en".into())]),
            }],
        }),
        ..Execution::default()
    };
    let bytes = manifest::build(
        &identity(),
        &execution,
        &packed.plan,
        &packed.source.packing,
        &hashes,
    )
    .unwrap();
    insta::assert_snapshot!(String::from_utf8(bytes).unwrap());
}
