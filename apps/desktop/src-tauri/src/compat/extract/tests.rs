use tar::{EntryType, Header};

use super::*;

enum Item<'a> {
    Dir(&'a str),
    File(&'a str, &'a [u8], u32),
    Symlink(&'a str, &'a str),
    Hardlink(&'a str, &'a str),
    Special(&'a str, EntryType),
}

/// A gzipped tar written header by header, so names the `tar` builder would refuse
/// (`..`, absolute paths) can be produced.
fn tar_gz(items: &[Item<'_>]) -> Vec<u8> {
    let mut tar = Vec::new();
    for item in items {
        let mut header = Header::new_gnu();
        let (name, kind, data, link, mode): (&str, EntryType, &[u8], Option<&str>, u32) = match item
        {
            Item::Dir(n) => (n, EntryType::Directory, b"", None, 0o755),
            Item::File(n, d, m) => (n, EntryType::Regular, d, None, *m),
            Item::Symlink(n, t) => (n, EntryType::Symlink, b"", Some(t), 0o777),
            Item::Hardlink(n, t) => (n, EntryType::Link, b"", Some(t), 0o644),
            Item::Special(n, k) => (n, *k, b"", None, 0o644),
        };
        {
            let gnu = header.as_gnu_mut().unwrap();
            gnu.name.fill(0);
            gnu.name[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(link) = link {
                gnu.linkname.fill(0);
                gnu.linkname[..link.len()].copy_from_slice(link.as_bytes());
            }
        }
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.extend_from_slice(header.as_bytes());
        tar.extend_from_slice(data);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

struct Case {
    _dir: tempfile::TempDir,
    root: PathBuf,
    dest: PathBuf,
    archive: PathBuf,
}

fn case(bytes: &[u8], name: &str) -> Case {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let dest = root.join("box/runtime");
    fs::create_dir_all(&dest).unwrap();
    let archive = root.join(name);
    fs::write(&archive, bytes).unwrap();
    Case {
        _dir: dir,
        root,
        dest,
        archive,
    }
}

fn refused(items: &[Item<'_>]) -> (ExtractError, Case) {
    let c = case(&tar_gz(items), "a.tar.gz");
    let err = extract(&c.archive, ArchiveFormat::TarGz, &c.dest).unwrap_err();
    (err, c)
}

#[test]
fn a_well_formed_runtime_is_extracted() {
    let c = case(
        &tar_gz(&[
            Item::Dir("proton/"),
            Item::File("proton/proton", b"#!/bin/sh\n", 0o4755),
            Item::Dir("proton/files/lib"),
            Item::File("proton/files/lib/libx.so.1", b"elf", 0o644),
            Item::Symlink("proton/files/lib/libx.so", "libx.so.1"),
            Item::Symlink("proton/files/bin", "../files/lib"),
            Item::Hardlink(
                "proton/files/lib/libx.so.copy",
                "proton/files/lib/libx.so.1",
            ),
        ]),
        "ok.tar.gz",
    );
    extract(&c.archive, ArchiveFormat::TarGz, &c.dest).unwrap();
    assert_eq!(
        fs::read(c.dest.join("proton/proton")).unwrap(),
        b"#!/bin/sh\n"
    );
    assert_eq!(
        fs::read(c.dest.join("proton/files/lib/libx.so.copy")).unwrap(),
        b"elf"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(c.dest.join("proton/proton"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755, "setuid is dropped, execute kept");
        assert_eq!(
            fs::read_link(c.dest.join("proton/files/lib/libx.so")).unwrap(),
            Path::new("libx.so.1")
        );
    }
}

#[test]
fn parent_components_are_refused() {
    let (err, c) = refused(&[
        Item::File("ok", b"x", 0o644),
        Item::File("../escaped", b"x", 0o644),
    ]);
    assert!(
        matches!(
            err,
            ExtractError::Unsafe {
                reason: "contains `..`",
                ..
            }
        ),
        "{err}"
    );
    assert!(!c.root.join("box/escaped").exists());
    let (err, _) = refused(&[Item::File("a/../../escaped", b"x", 0o644)]);
    assert!(matches!(err, ExtractError::Unsafe { .. }), "{err}");
}

#[test]
fn absolute_paths_are_refused() {
    let (err, c) = refused(&[Item::File("/tmp/vgames-abs-escape", b"x", 0o644)]);
    assert!(
        matches!(
            err,
            ExtractError::Unsafe {
                reason: "is an absolute path",
                ..
            }
        ),
        "{err}"
    );
    assert!(!Path::new("/tmp/vgames-abs-escape").exists());
    assert!(fs::read_dir(&c.dest).unwrap().next().is_none());
}

#[test]
fn links_leaving_the_target_are_refused() {
    for target in ["../../outside", "/etc/passwd", "a/../../.."] {
        let (err, c) = refused(&[Item::Symlink("lib/link", target)]);
        assert!(
            matches!(err, ExtractError::Unsafe { .. }),
            "{target}: {err}"
        );
        assert!(
            fs::symlink_metadata(c.dest.join("lib/link")).is_err(),
            "{target}"
        );
    }
}

#[test]
fn nothing_is_written_through_an_extracted_link() {
    // A link that stays inside, then a file below it: refused (it could be swapped).
    let (err, c) = refused(&[
        Item::Dir("real"),
        Item::Symlink("via", "real"),
        Item::File("via/payload", b"x", 0o644),
    ]);
    assert!(
        matches!(
            err,
            ExtractError::Unsafe {
                reason: "is below a link",
                ..
            }
        ),
        "{err}"
    );
    assert!(!c.dest.join("real/payload").exists());
    // An entry replacing an earlier link or file is refused too.
    let (err, _) = refused(&[Item::File("a", b"1", 0o644), Item::Symlink("a", "b")]);
    assert!(
        matches!(
            err,
            ExtractError::Unsafe {
                reason: "replaces an earlier entry",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn device_nodes_and_fifos_are_refused() {
    for kind in [EntryType::Char, EntryType::Block, EntryType::Fifo] {
        let (err, c) = refused(&[Item::Special("dev/sda", kind)]);
        assert!(
            matches!(
                err,
                ExtractError::Unsafe {
                    reason: "is a device, FIFO or special file",
                    ..
                }
            ),
            "{kind:?}: {err}"
        );
        assert!(fs::symlink_metadata(c.dest.join("dev/sda")).is_err());
    }
}

#[test]
fn hard_links_only_to_extracted_files() {
    for target in ["/etc/passwd", "../outside", "missing"] {
        let (err, _) = refused(&[Item::Hardlink("h", target)]);
        assert!(
            matches!(err, ExtractError::Unsafe { .. }),
            "{target}: {err}"
        );
    }
}

fn zip_bytes(build: impl FnOnce(&mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    build(&mut w);
    w.finish().unwrap().into_inner()
}

fn stored(mode: u32) -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(mode)
}

#[test]
fn zip_archives_follow_the_same_rules() {
    let good = zip_bytes(|w| {
        w.add_directory("bin/", stored(0o755)).unwrap();
        w.start_file("bin/wine", stored(0o755)).unwrap();
        w.write_all(b"wine").unwrap();
        w.add_symlink("bin/wine64", "wine", stored(0o777)).unwrap();
    });
    let c = case(&good, "ok.zip");
    extract(&c.archive, ArchiveFormat::Zip, &c.dest).unwrap();
    assert_eq!(fs::read(c.dest.join("bin/wine")).unwrap(), b"wine");

    let escape = zip_bytes(|w| {
        w.start_file("../escaped", stored(0o644)).unwrap();
        w.write_all(b"x").unwrap();
    });
    let c = case(&escape, "escape.zip");
    assert!(matches!(
        extract(&c.archive, ArchiveFormat::Zip, &c.dest),
        Err(ExtractError::Unsafe { .. })
    ));
    assert!(!c.root.join("box/escaped").exists());

    let outward = zip_bytes(|w| {
        w.add_symlink("bin/sh", "/bin/sh", stored(0o777)).unwrap();
    });
    let c = case(&outward, "link.zip");
    assert!(matches!(
        extract(&c.archive, ArchiveFormat::Zip, &c.dest),
        Err(ExtractError::Unsafe { .. })
    ));
}

#[test]
fn every_accepted_format_extracts() {
    let tar = {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = Header::new_gnu();
        h.set_size(5);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "dir/file", &b"hello"[..]).unwrap();
        b.into_inner().unwrap()
    };
    let xz = {
        let mut e = liblzma::write::XzEncoder::new(Vec::new(), 1);
        e.write_all(&tar).unwrap();
        e.finish().unwrap()
    };
    let zst = zstd::stream::encode_all(&tar[..], 1).unwrap();
    for (bytes, format) in [(xz, ArchiveFormat::TarXz), (zst, ArchiveFormat::TarZst)] {
        let c = case(&bytes, "a");
        extract(&c.archive, format, &c.dest).unwrap();
        assert_eq!(
            fs::read(c.dest.join("dir/file")).unwrap(),
            b"hello",
            "{format:?}"
        );
    }
}

#[test]
fn garbage_is_corrupt_not_a_panic() {
    for format in [
        ArchiveFormat::TarGz,
        ArchiveFormat::TarXz,
        ArchiveFormat::TarZst,
        ArchiveFormat::Zip,
    ] {
        let c = case(b"definitely not an archive", "g");
        assert!(extract(&c.archive, format, &c.dest).is_err(), "{format:?}");
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Arbitrary bytes and mutated real archives never panic, and never write
    /// outside the target.
    #[test]
    fn arbitrary_archives_never_panic(
        raw in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..2048),
        flips in proptest::collection::vec((proptest::prelude::any::<proptest::sample::Index>(), proptest::prelude::any::<u8>()), 0..8),
    ) {
        let mut mutated = tar_gz(&[
            Item::Dir("d"),
            Item::File("d/f", b"data", 0o644),
            Item::Symlink("d/l", "f"),
        ]);
        for (at, byte) in flips {
            let i = at.index(mutated.len());
            mutated[i] ^= byte;
        }
        for (bytes, format) in [
            (raw.clone(), ArchiveFormat::TarGz),
            (raw.clone(), ArchiveFormat::TarXz),
            (raw.clone(), ArchiveFormat::TarZst),
            (raw, ArchiveFormat::Zip),
            (mutated, ArchiveFormat::TarGz),
        ] {
            let c = case(&bytes, "p");
            let _ = extract(&c.archive, format, &c.dest);
            let outside: Vec<_> = fs::read_dir(c.root.join("box")).unwrap().map(|e| e.unwrap().file_name()).collect();
            proptest::prop_assert_eq!(outside, vec![std::ffi::OsString::from("runtime")]);
        }
    }
}
