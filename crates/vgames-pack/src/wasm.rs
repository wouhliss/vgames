//! Browser API (feature `wasm`) for the admin-web upload worker.
//!
//! The browser reads `Blob` slices asynchronously, so JS drives the loop:
//!
//! ```js
//! const packer = WasmPacker.plan(files, emptyDirs);        // [{path, size, mtime_ms}]
//! for (let p = 0; p < packer.packCount(); p++) {           // packs may run in parallel
//!   for (const c of packer.packChunks(p)) {                // in order within a pack
//!     const bytes = await readExtents(packer.chunkExtents(c)); // [{file, offset, len}]
//!     packer.addChunk(c, bytes);                           // hashes; returns bytes to upload (raw)
//!   }
//! }
//! const manifest = packer.buildManifest(identity, execution); // Uint8Array to sign
//! ```
//!
//! The browser packer stores every chunk raw (02 §4), so the stored bytes are
//! the decoded bytes. Key-file decryption and signing come from `vgames-core`
//! (re-exported here once A5-T03 lands).

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::layout::PACK_SIZE;
use crate::manifest::{self, Execution, VersionIdentity};
use crate::plan::{Packing, Plan, SourceFile};
use crate::source::HashCollector;

fn js_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

#[derive(Serialize)]
struct Extent {
    file: u32,
    offset: u64,
    len: u64,
}

#[derive(Deserialize)]
struct InputFile {
    path: String,
    size: u64,
    mtime_ms: i64,
}

/// Incremental BLAKE3 for JS.
#[wasm_bindgen]
pub struct Blake3Hasher(blake3::Hasher);

#[wasm_bindgen]
impl Blake3Hasher {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Lowercase hex digest.
    #[wasm_bindgen(js_name = finalizeHex)]
    pub fn finalize_hex(&self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

impl Default for Blake3Hasher {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
pub struct WasmPacker {
    plan: Plan,
    packing: Packing,
    hashes: HashCollector,
    /// Per pack: next chunk expected and the running pack hash.
    packs: Vec<(u32, blake3::Hasher)>,
}

#[wasm_bindgen]
impl WasmPacker {
    /// Plans `files` (`[{path, size, mtime_ms}]`) and `directories` (empty
    /// folders). Throws with the list of invalid paths.
    pub fn plan(files: JsValue, directories: JsValue) -> Result<WasmPacker, JsError> {
        let files: Vec<InputFile> = serde_wasm_bindgen::from_value(files).map_err(js_error)?;
        let directories: Vec<String> =
            serde_wasm_bindgen::from_value(directories).map_err(js_error)?;
        let files = files
            .into_iter()
            .map(|f| SourceFile {
                path: f.path,
                size: f.size,
                mtime_ms: f.mtime_ms,
                executable: false,
            })
            .collect();
        let plan = Plan::new(files, directories).map_err(|e| match e {
            crate::plan::PlanError::InvalidPaths(list) => js_error(
                list.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            other => js_error(other),
        })?;
        let packing = plan.raw_packing(PACK_SIZE).map_err(js_error)?;
        let hashes = HashCollector::new(&plan, packing.pack_count());
        let packs = packing
            .packs
            .iter()
            .map(|p| (p.first_chunk, blake3::Hasher::new()))
            .collect();
        Ok(Self {
            plan,
            packing,
            hashes,
            packs,
        })
    }

    /// Planned files in manifest order: `[{path, size, mtime_ms}]`.
    pub fn files(&self) -> Result<JsValue, JsError> {
        serde_wasm_bindgen::to_value(self.plan.files()).map_err(js_error)
    }

    #[wasm_bindgen(js_name = packCount)]
    pub fn pack_count(&self) -> u32 {
        self.packing.pack_count()
    }

    #[wasm_bindgen(js_name = packSize)]
    pub fn pack_size(&self, pack: u32) -> Option<f64> {
        self.packing.packs.get(pack as usize).map(|p| p.size as f64)
    }

    /// Chunk indexes of a pack, in order.
    #[wasm_bindgen(js_name = packChunks)]
    pub fn pack_chunks(&self, pack: u32) -> Vec<u32> {
        self.packing
            .packs
            .get(pack as usize)
            .map(|p| (p.first_chunk..p.first_chunk + p.chunk_count).collect())
            .unwrap_or_default()
    }

    /// `[{file, offset, len}]` to read (file = index into `files()`).
    #[wasm_bindgen(js_name = chunkExtents)]
    pub fn chunk_extents(&self, chunk: u32) -> Result<JsValue, JsError> {
        let extents: Vec<Extent> = self
            .plan
            .extents(chunk)
            .into_iter()
            .map(|(file, offset, len)| Extent { file, offset, len })
            .collect();
        serde_wasm_bindgen::to_value(&extents).map_err(js_error)
    }

    /// Records a chunk's bytes. Chunks of one pack must arrive in order,
    /// starting again from the pack's first chunk to resume an upload.
    #[wasm_bindgen(js_name = addChunk)]
    pub fn add_chunk(&mut self, chunk: u32, bytes: &[u8]) -> Result<(), JsError> {
        if bytes.len() as u64 != self.plan.chunk_len(chunk) {
            return Err(js_error(format!(
                "chunk {chunk}: expected {} bytes",
                self.plan.chunk_len(chunk)
            )));
        }
        let placement = self
            .packing
            .chunks
            .get(chunk as usize)
            .ok_or_else(|| js_error(format!("no chunk {chunk}")))?
            .placement;
        let slot = self.packing.packs.get(placement.pack as usize).copied();
        let (next, hasher) = self
            .packs
            .get_mut(placement.pack as usize)
            .ok_or_else(|| js_error("no such pack"))?;
        let Some(slot) = slot else {
            return Err(js_error("no such pack"));
        };
        if chunk == slot.first_chunk {
            *next = slot.first_chunk;
            *hasher = blake3::Hasher::new();
        }
        if chunk != *next {
            return Err(js_error(format!(
                "pack {}: expected chunk {next}, got {chunk}",
                placement.pack
            )));
        }
        let digest = *blake3::hash(bytes).as_bytes();
        self.hashes
            .record_chunk(&self.plan, chunk, bytes, digest)
            .map_err(js_error)?;
        hasher.update(bytes);
        *next += 1;
        if *next == slot.first_chunk + slot.chunk_count {
            self.hashes
                .record_pack(placement.pack, *hasher.finalize().as_bytes());
        }
        Ok(())
    }

    /// Manifest bytes, once every chunk was added. `identity` is
    /// `{server_id, package_id, version_id, sequence, version_label, platform, created_at}`
    /// (created_at in Unix seconds); `execution` holds `launch`, `saves`, ….
    #[wasm_bindgen(js_name = buildManifest)]
    pub fn build_manifest(
        &self,
        identity: JsValue,
        execution: JsValue,
    ) -> Result<Vec<u8>, JsError> {
        let identity: VersionIdentity =
            serde_wasm_bindgen::from_value(identity).map_err(js_error)?;
        let execution: Execution = if execution.is_undefined() || execution.is_null() {
            Execution::default()
        } else {
            serde_wasm_bindgen::from_value(execution).map_err(js_error)?
        };
        let hashes = self.hashes.finish().map_err(js_error)?;
        manifest::build(&identity, &execution, &self.plan, &self.packing, &hashes).map_err(js_error)
    }
}
