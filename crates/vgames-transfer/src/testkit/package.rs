//! Signed test packages: a folder of files → packs in memory, manifest bytes,
//! a publisher signature and the trust state that accepts it.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;
use vgames_core::manifest::Platform;
use vgames_core::trust::{
    PublisherKey, Revocation, RootPin, TrustBundle, TrustState, sign_bundle, verify_bundle,
};
use vgames_core::verify::{ExpectedRelease, VerifyMode};
use vgames_core::{Context, Envelope, SecretKey, Timestamp};
use vgames_pack::manifest::{self, Execution, VersionIdentity};
use vgames_pack::scan::{self, FsReader};
use vgames_pack::{Compression, PACK_SIZE, PackSource, Plan, source};

use crate::install::{Release, verify_release};

pub const SERVER_ID: Uuid = Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0001);
pub const PACKAGE_ID: Uuid = Uuid::from_u128(0x0192_a6f0_1c2d_7e3f_8a9b_0c1d_2e3f_4a5b);
pub const ADMIN_ID: Uuid = Uuid::from_u128(0x0192_aaaa_0000_7000_8000_0000_0000_0001);

/// What a test file contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Incompressible pseudo-random bytes.
    Random(u64),
    /// A repeating pattern (stored as zstd with `Compression::Auto`).
    Compressible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub path: String,
    pub size: u64,
    pub content: Content,
    pub executable: bool,
}

impl FileSpec {
    pub fn random(path: &str, size: u64, seed: u64) -> Self {
        Self {
            path: path.to_owned(),
            size,
            content: Content::Random(seed),
            executable: false,
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        content_bytes(self.content, self.size)
    }
}

pub fn content_bytes(content: Content, size: u64) -> Vec<u8> {
    match content {
        Content::Compressible => (0..size)
            .map(|i| b"vgames-save-data-"[(i % 17) as usize])
            .collect(),
        Content::Random(seed) => {
            let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut out = Vec::with_capacity(size as usize);
            while (out.len() as u64) < size {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                out.extend_from_slice(&x.to_le_bytes());
            }
            out.truncate(size as usize);
            out
        }
    }
}

/// Writes `files` (and empty `dirs`) under `root`.
pub fn write_tree(root: &Path, files: &[FileSpec], dirs: &[&str]) {
    for file in files {
        let path = file.path.split('/').fold(root.to_owned(), |p, c| p.join(c));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, file.bytes()).unwrap();
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    for dir in dirs {
        std::fs::create_dir_all(dir.split('/').fold(root.to_owned(), |p, c| p.join(c))).unwrap();
    }
}

/// A random tree: sizes include 0, chunk boundaries and multi-chunk files.
pub fn random_files(seed: u64, count: usize, max_size: u64) -> Vec<FileSpec> {
    const MIB: u64 = 1024 * 1024;
    let mut x = seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let special = [0, 1, 4 * MIB - 1, 4 * MIB, 4 * MIB + 1, 8 * MIB];
    (0..count)
        .map(|i| {
            let r = next();
            let size = if r % 5 == 0 {
                special[(r / 5 % special.len() as u64) as usize].min(max_size)
            } else if r % 3 == 0 {
                next() % max_size.max(1)
            } else {
                next() % 70_000
            };
            let dir = ["Game", "Game/bin", "Game/data/ünïcode", "Engine/Content"][(r % 4) as usize];
            FileSpec {
                path: format!("{dir}/file-{i:04}.dat"),
                size,
                content: if r % 7 == 0 {
                    Content::Compressible
                } else {
                    Content::Random(next())
                },
                executable: r % 11 == 0,
            }
        })
        .collect()
}

/// Version identity of a test package.
#[derive(Debug, Clone)]
pub struct Identity {
    pub version_id: Uuid,
    pub sequence: u64,
    pub platform: Platform,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            version_id: Uuid::from_u128(0x0192_a6f1_aaaa_7bbb_8ccc_0000_0000_0001),
            sequence: 1,
            platform: Platform::LinuxX86_64,
        }
    }
}

pub fn root_key() -> SecretKey {
    SecretKey::from_seed(&[1; 32])
}

pub fn publisher_key() -> SecretKey {
    SecretKey::from_seed(&[2; 32])
}

/// A trust state (bundle v1) trusting [`publisher_key`].
pub fn trust_state() -> TrustState {
    trust_state_with(1, false)
}

/// Bundle `version`, optionally revoking [`publisher_key`].
pub fn trust_state_with(version: u64, revoke_publisher: bool) -> TrustState {
    let root = root_key();
    let publisher = publisher_key();
    let ts = |s: &str| -> Timestamp { s.parse().unwrap() };
    let bundle = TrustBundle {
        format: "vgames.trust/1".into(),
        server_id: SERVER_ID,
        version,
        issued_at: ts("2026-09-24T10:00:00Z"),
        expires_at: None,
        root_key_id: root.public_key().key_id(),
        publishers: vec![PublisherKey {
            key_id: publisher.public_key().key_id(),
            public_key: publisher.public_key(),
            holder_user_id: ADMIN_ID,
            label: "test@rig".into(),
            not_before: ts("2026-01-01T00:00:00Z"),
            not_after: ts("2036-01-01T00:00:00Z"),
        }],
        revoked: if revoke_publisher {
            vec![Revocation {
                key_id: publisher.public_key().key_id(),
                revoked_at: ts("2026-09-25T10:00:00Z"),
                reason: "test".into(),
            }]
        } else {
            vec![]
        },
        next_root: None,
    };
    let bytes = bundle.to_bytes();
    let signature = sign_bundle(&root, &bytes);
    verify_bundle(
        &bytes,
        &signature,
        &RootPin::new(root.public_key()),
        None,
        SERVER_ID,
    )
    .unwrap()
    .state
}

/// A packed, signed package.
pub struct TestPackage {
    /// The source tree (kept alive for the test's duration).
    pub source: tempfile::TempDir,
    pub files: Vec<FileSpec>,
    pub manifest: Vec<u8>,
    pub envelope: Envelope,
    pub trust: TrustState,
    pub expected: ExpectedRelease,
    pub packs: Vec<Arc<Vec<u8>>>,
}

impl TestPackage {
    pub fn build(files: &[FileSpec], dirs: &[&str], compression: Compression) -> Self {
        Self::build_with(files, dirs, compression, PACK_SIZE, &Identity::default())
    }

    pub fn build_with(
        files: &[FileSpec],
        dirs: &[&str],
        compression: Compression,
        pack_size: u64,
        identity: &Identity,
    ) -> Self {
        Self::build_executable(
            files,
            dirs,
            compression,
            pack_size,
            identity,
            &Execution::default(),
        )
    }

    /// Like [`Self::build_with`], with launch targets, saves and the like.
    pub fn build_executable(
        files: &[FileSpec],
        dirs: &[&str],
        compression: Compression,
        pack_size: u64,
        identity: &Identity,
        execution: &Execution,
    ) -> Self {
        let source_dir = tempfile::tempdir().unwrap();
        write_tree(source_dir.path(), files, dirs);
        let scanned = scan::scan(source_dir.path()).unwrap();
        assert!(scanned.rejected.is_empty(), "{:?}", scanned.rejected);
        let plan = Arc::new(Plan::new(scanned.files, scanned.directories).unwrap());
        let reader = Arc::new(FsReader::new(source_dir.path()));
        let pack_source = match compression {
            Compression::None => PackSource::new(
                plan.clone(),
                Arc::new(plan.raw_packing(pack_size).unwrap()),
                reader,
            ),
            Compression::Auto => {
                source::analyze(plan.clone(), reader, compression, pack_size, 4).unwrap()
            }
        };
        let packs: Vec<Arc<Vec<u8>>> = (0..pack_source.packing.pack_count())
            .map(|i| {
                let mut out = Vec::new();
                pack_source
                    .open_pack(i, 0)
                    .unwrap()
                    .read_to_end(&mut out)
                    .unwrap();
                Arc::new(out)
            })
            .collect();
        let hashes = pack_source.hashes.finish().unwrap();
        let identity_out = VersionIdentity {
            server_id: SERVER_ID,
            package_id: PACKAGE_ID,
            version_id: identity.version_id,
            sequence: identity.sequence,
            version_label: format!("1.{}", identity.sequence),
            platform: identity.platform.as_str().to_owned(),
            created_at: 1_790_244_000,
        };
        let manifest = manifest::build(
            &identity_out,
            execution,
            &plan,
            &pack_source.packing,
            &hashes,
        )
        .unwrap();
        let envelope = Envelope::sign(&publisher_key(), Context::Manifest, &manifest);
        Self {
            source: source_dir,
            files: files.to_vec(),
            manifest,
            envelope,
            trust: trust_state(),
            expected: ExpectedRelease {
                server_id: SERVER_ID,
                package_id: PACKAGE_ID,
                version_id: identity.version_id,
                platform: identity.platform,
                sequence: identity.sequence,
            },
            packs,
        }
    }

    /// The verified release (as the launcher would get it).
    pub fn release(&self) -> Release {
        verify_release(
            &self.trust,
            self.envelope.clone(),
            self.manifest.clone(),
            &self.expected,
            None,
            install_mode(),
        )
        .unwrap()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

pub fn install_mode() -> VerifyMode {
    VerifyMode::Install {
        now: Timestamp::new(time::OffsetDateTime::now_utc()),
        allow_older: false,
    }
}

/// Every regular file under `root` (relative `/` paths → bytes), skipping `.vgames`.
pub fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if rel == ".vgames" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Empty directories under `root` (relative), skipping `.vgames`.
pub fn empty_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let mut empty = true;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            empty = false;
            if entry.file_type().unwrap().is_dir() && entry.file_name() != ".vgames" {
                stack.push(entry.path());
            }
        }
        if empty && dir != root {
            out.push(
                dir.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    out.sort();
    out
}

/// The release a default-identity [`TestPackage`] answers to.
pub fn default_expected() -> ExpectedRelease {
    let identity = Identity::default();
    ExpectedRelease {
        server_id: SERVER_ID,
        package_id: PACKAGE_ID,
        version_id: identity.version_id,
        platform: identity.platform,
        sequence: identity.sequence,
    }
}
