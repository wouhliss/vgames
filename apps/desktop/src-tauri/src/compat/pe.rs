//! Which Direct3D a Windows executable imports (09-compatibility §3, GAME-06).
//!
//! A bounded scan of the PE import and delay-import tables of the launch
//! executable, run on a blocking thread before the rest of the install downloads.
//! Untrusted input: every offset is checked, every read is bounded, and the scan
//! gives up after [`MAX_DESCRIPTORS`] descriptors or [`MAX_READ`] bytes.

use std::io::{Read, Seek, SeekFrom};

/// Bytes the scan reads in total.
pub const MAX_READ: u64 = 1024 * 1024;
/// Import descriptors per table.
pub const MAX_DESCRIPTORS: usize = 4096;
const MAX_SECTIONS: u16 = 96;
const MAX_NAME: usize = 256;

/// The Direct3D DLLs an executable imports.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, specta::Type)]
pub struct D3dImports {
    pub d3d9: bool,
    pub d3d10: bool,
    pub d3d11: bool,
    pub d3d12: bool,
}

impl D3dImports {
    fn note(&mut self, dll: &str) {
        match dll.to_ascii_lowercase().as_str() {
            "d3d9.dll" => self.d3d9 = true,
            "d3d10.dll" | "d3d10_1.dll" => self.d3d10 = true,
            "d3d11.dll" => self.d3d11 = true,
            "d3d12.dll" => self.d3d12 = true,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeError {
    #[error("not a Windows executable")]
    NotPe,
    #[error("the executable's headers are damaged: {0}")]
    Malformed(&'static str),
    #[error("cannot read the executable: {0}")]
    Io(String),
}

struct Bounded<'r, R> {
    inner: &'r mut R,
    len: u64,
    budget: u64,
}

impl<R: Read + Seek> Bounded<'_, R> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), PeError> {
        let n = buf.len() as u64;
        if offset.checked_add(n).is_none_or(|end| end > self.len) {
            return Err(PeError::Malformed(
                "a field points past the end of the file",
            ));
        }
        self.budget = self
            .budget
            .checked_sub(n)
            .ok_or(PeError::Malformed("the headers are too large"))?;
        self.inner
            .seek(SeekFrom::Start(offset))
            .map_err(|e| PeError::Io(e.to_string()))?;
        self.inner
            .read_exact(buf)
            .map_err(|e| PeError::Io(e.to_string()))
    }

    fn u16_at(&mut self, offset: u64) -> Result<u16, PeError> {
        let mut b = [0u8; 2];
        self.read_at(offset, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    fn u32_at(&mut self, offset: u64) -> Result<u32, PeError> {
        let mut b = [0u8; 4];
        self.read_at(offset, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// A NUL-terminated ASCII name of at most [`MAX_NAME`] bytes.
    fn name_at(&mut self, offset: u64) -> Result<String, PeError> {
        let available = self.len.saturating_sub(offset).min(MAX_NAME as u64);
        let mut buf = vec![0u8; usize::try_from(available).unwrap_or(MAX_NAME)];
        self.read_at(offset, &mut buf)?;
        let end = buf
            .iter()
            .position(|b| *b == 0)
            .ok_or(PeError::Malformed("an import name is too long"))?;
        let name = buf.get(..end).unwrap_or_default();
        if !name.iter().all(|b| b.is_ascii_graphic()) {
            return Err(PeError::Malformed("an import name is not ASCII"));
        }
        Ok(String::from_utf8_lossy(name).into_owned())
    }
}

struct Section {
    rva: u32,
    size: u32,
    raw: u32,
    raw_size: u32,
}

fn to_offset(sections: &[Section], rva: u32) -> Option<u64> {
    sections.iter().find_map(|s| {
        let span = s.size.max(s.raw_size);
        let delta = rva.checked_sub(s.rva)?;
        (delta < span && delta < s.raw_size).then(|| u64::from(s.raw) + u64::from(delta))
    })
}

/// Scans `exe` (of `len` bytes) for Direct3D imports.
pub fn scan<R: Read + Seek>(exe: &mut R, len: u64) -> Result<D3dImports, PeError> {
    let mut r = Bounded {
        inner: exe,
        len,
        budget: MAX_READ,
    };
    if r.u16_at(0).map_err(|_| PeError::NotPe)? != 0x5a4d {
        return Err(PeError::NotPe); // "MZ"
    }
    let pe = u64::from(r.u32_at(0x3c)?);
    if r.u32_at(pe)? != 0x0000_4550 {
        return Err(PeError::NotPe); // "PE\0\0"
    }
    let coff = pe + 4;
    let sections_count = r.u16_at(coff + 2)?;
    let optional_size = u64::from(r.u16_at(coff + 16)?);
    if sections_count > MAX_SECTIONS {
        return Err(PeError::Malformed("too many sections"));
    }
    let optional = coff + 20;
    let directories = match r.u16_at(optional)? {
        0x10b => optional + 96,  // PE32
        0x20b => optional + 112, // PE32+
        _ => return Err(PeError::Malformed("unknown optional header")),
    };
    let directory_count = r.u32_at(directories - 4)?;
    let mut sections = Vec::with_capacity(usize::from(sections_count));
    let table = optional + optional_size;
    for i in 0..u64::from(sections_count) {
        let at = table + i * 40;
        sections.push(Section {
            size: r.u32_at(at + 8)?,
            rva: r.u32_at(at + 12)?,
            raw_size: r.u32_at(at + 16)?,
            raw: r.u32_at(at + 20)?,
        });
    }
    let mut found = D3dImports::default();
    // Directory 1: imports (20-byte descriptors, name RVA at +12);
    // directory 13: delay imports (32-byte descriptors, name RVA at +4).
    for (index, stride, name_field) in [(1u32, 20u64, 12u64), (13, 32, 4)] {
        if index >= directory_count {
            continue;
        }
        let rva = r.u32_at(directories + u64::from(index) * 8)?;
        if rva == 0 {
            continue;
        }
        let start = to_offset(&sections, rva).ok_or(PeError::Malformed(
            "the import table is outside every section",
        ))?;
        for n in 0..MAX_DESCRIPTORS as u64 {
            let at = start + n * stride;
            let name_rva = r.u32_at(at + name_field)?;
            if name_rva == 0 {
                break;
            }
            let Some(offset) = to_offset(&sections, name_rva) else {
                return Err(PeError::Malformed(
                    "an import name is outside every section",
                ));
            };
            found.note(&r.name_at(offset)?);
        }
    }
    Ok(found)
}

/// Scans the file at `path` (call on a blocking thread).
pub fn scan_file(path: &std::path::Path) -> Result<D3dImports, PeError> {
    let mut file = std::fs::File::open(path).map_err(|e| PeError::Io(e.to_string()))?;
    let len = file
        .metadata()
        .map_err(|e| PeError::Io(e.to_string()))?
        .len();
    scan(&mut file, len)
}

#[cfg(test)]
pub(crate) mod tests;
