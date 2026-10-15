//! Offline, bounded icons copied out of the evictable cover cache.
use std::{
    io::{self, Cursor, Read, Write},
    path::{Path, PathBuf},
};

use image::{ImageEncoder, ImageReader, RgbaImage, imageops::FilterType};

use super::ShortcutFormat;
use crate::events::PackageRef;

const FALLBACK: &[u8] = include_bytes!("../../icons/128x128.png");
const MAX_ENCODED: usize = 8 * 1024 * 1024;

fn picture(bytes: &[u8]) -> Option<RgbaImage> {
    if bytes.len() > MAX_ENCODED {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Some(
        reader
            .decode()
            .ok()?
            .resize_to_fill(256, 256, FilterType::Triangle)
            .into_rgba8(),
    )
}
fn encoded(cover: Option<&[u8]>, format: ShortcutFormat) -> io::Result<Vec<u8>> {
    let image = cover
        .and_then(picture)
        .or_else(|| picture(FALLBACK))
        .ok_or_else(|| io::Error::other("Cannot render the shortcut icon"))?;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(image.as_raw(), 256, 256, image::ExtendedColorType::Rgba8)
        .map_err(|_| io::Error::other("Cannot encode the shortcut icon"))?;
    if format != ShortcutFormat::Url {
        return Ok(png);
    }
    // Windows Vista+ supports PNG-compressed ICO frames. A zero byte denotes
    // 256 pixels in the directory, followed by planes=1, depth=32 and offset=22.
    let mut ico = vec![0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 32, 0];
    let length = u32::try_from(png.len()).map_err(|_| io::Error::other("Icon too large"))?;
    ico.extend_from_slice(&length.to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(&png);
    Ok(ico)
}

/// Run on a blocking pool. All input paths come from native app paths; cover
/// bytes come from ImageCache. Existing user shortcuts never lose their icons
/// when the image cache evicts a cover or a later cover changes.
pub fn prepare(
    data: &Path,
    package: PackageRef,
    format: ShortcutFormat,
    cover: Option<&[u8]>,
) -> io::Result<Option<PathBuf>> {
    if format == ShortcutFormat::Webloc {
        return Ok(None);
    }
    let bytes = encoded(cover, format)?;
    let directory = data.join("shortcut-icons");
    crate::paths::create_private_dir(&directory)
        .map_err(|_| io::Error::other("Cannot create icon storage"))?;
    if !std::fs::symlink_metadata(&directory)?.is_dir() {
        return Err(io::Error::other("Icon storage is not a real directory"));
    }
    let extension = if format == ShortcutFormat::Url {
        "ico"
    } else {
        "png"
    };
    let digest = blake3::hash(&bytes).to_hex();
    let path = directory.join(format!(
        "{}-{}-{digest}.{extension}",
        package.server_id, package.package_id
    ));
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::other("Icon path is not a regular file"));
        }
        Ok(metadata) if metadata.len() == bytes.len() as u64 => {
            let mut existing = Vec::new();
            std::fs::File::open(&path)?
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            if existing == bytes {
                return Ok(Some(path));
            }
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        Ok(Some(path))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests;
