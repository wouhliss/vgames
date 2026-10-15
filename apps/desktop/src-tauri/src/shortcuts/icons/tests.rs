use super::*;
use crate::images::{ImageCache, ImageKey};

fn package_ref() -> PackageRef {
    PackageRef {
        server_id: uuid::Uuid::now_v7(),
        package_id: uuid::Uuid::now_v7(),
    }
}
fn cover() -> Vec<u8> {
    let image = RgbaImage::from_pixel(40, 60, image::Rgba([240, 20, 30, 255]));
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(image.as_raw(), 40, 60, image::ExtendedColorType::Rgba8)
        .unwrap();
    bytes
}
#[test]
fn cached_cover_becomes_a_persistent_png_or_windows_ico_with_valid_pixels() {
    let data = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = ImageCache::open(cache_dir.path().to_owned()).unwrap();
    let package = package_ref();
    let key = ImageKey::for_asset(package.server_id, "fixture-cover").unwrap();
    cache.insert(&key, "image/png", &cover()).unwrap();
    let cached = cache.get(&key).unwrap().unwrap();
    for format in [ShortcutFormat::Desktop, ShortcutFormat::Url] {
        let path = prepare(data.path(), package, format, Some(&cached.bytes))
            .unwrap()
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let png = if format == ShortcutFormat::Url {
            assert_eq!(&bytes[..14], &[0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 32, 0]);
            assert_eq!(u32::from_le_bytes(bytes[18..22].try_into().unwrap()), 22);
            assert_eq!(
                u32::from_le_bytes(bytes[14..18].try_into().unwrap()) as usize,
                bytes.len() - 22
            );
            &bytes[22..]
        } else {
            bytes.as_slice()
        };
        let decoded = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.dimensions(), (256, 256));
        assert_eq!(decoded.get_pixel(128, 128).0, [240, 20, 30, 255]);
        let text = super::super::render(format, "A game", package.package_id, Some(&path)).unwrap();
        assert!(text.contains(&path.to_string_lossy().to_string()));
        assert_eq!(
            prepare(data.path(), package, format, Some(&cached.bytes))
                .unwrap()
                .unwrap(),
            path
        );
    }
    // The retained shortcut icons do not depend on the evictable image cache.
    drop(cache);
    cache_dir.close().unwrap();
    assert_eq!(
        std::fs::read_dir(data.path().join("shortcut-icons"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn missing_corrupt_and_oversized_covers_use_the_app_icon_and_webloc_needs_no_icon_file() {
    let data = tempfile::tempdir().unwrap();
    let package = package_ref();
    let fallback = prepare(data.path(), package, ShortcutFormat::Desktop, None)
        .unwrap()
        .unwrap();
    let oversized = vec![0; MAX_ENCODED + 1];
    let wide = RgbaImage::new(4097, 1);
    let mut wide_png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut wide_png)
        .write_image(wide.as_raw(), 4097, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    for invalid in [
        b"not an image".as_slice(),
        b"\x89PNG\r\n\x1a\n".as_slice(),
        oversized.as_slice(),
        wide_png.as_slice(),
    ] {
        let path = prepare(data.path(), package, ShortcutFormat::Desktop, Some(invalid))
            .unwrap()
            .unwrap();
        assert_eq!(path, fallback);
    }
    let other = prepare(data.path(), package_ref(), ShortcutFormat::Desktop, None)
        .unwrap()
        .unwrap();
    assert_ne!(fallback, other);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        prepare(empty.path(), package, ShortcutFormat::Webloc, None).unwrap(),
        None
    );
    assert_eq!(std::fs::read_dir(empty.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn icon_storage_links_are_refused() {
    let data = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), data.path().join("shortcut-icons")).unwrap();
    assert!(prepare(data.path(), package_ref(), ShortcutFormat::Desktop, None).is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn generated_desktop_entry_passes_the_native_validator() {
    let data = tempfile::tempdir().unwrap();
    let package = package_ref();
    let icon = prepare(
        data.path(),
        package,
        ShortcutFormat::Desktop,
        Some(&cover()),
    )
    .unwrap()
    .unwrap();
    let entry = super::super::create(
        data.path(),
        ShortcutFormat::Desktop,
        "Game / with spaces",
        package.package_id,
        Some(&icon),
    )
    .unwrap();
    let output = std::process::Command::new("desktop-file-validate")
        .arg(&entry)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn generated_webloc_passes_the_native_property_list_validator() {
    let data = tempfile::tempdir().unwrap();
    let package = package_ref();
    let entry = super::super::create(
        data.path(),
        ShortcutFormat::Webloc,
        "Game",
        package.package_id,
        None,
    )
    .unwrap();
    let output = std::process::Command::new("/usr/bin/plutil")
        .arg("-lint")
        .arg(&entry)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        std::fs::read_to_string(entry)
            .unwrap()
            .contains(&super::super::launch_url(package.package_id))
    );
}

#[cfg(windows)]
#[test]
#[allow(unsafe_code)]
fn generated_ico_loads_through_the_native_windows_icon_decoder() {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    #[link(name = "user32")]
    unsafe extern "system" {
        fn LoadImageW(
            instance: *mut c_void,
            name: *const u16,
            kind: u32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> *mut c_void;
        fn DestroyIcon(icon: *mut c_void) -> i32;
    }
    let data = tempfile::tempdir().unwrap();
    let package = package_ref();
    let icon = prepare(data.path(), package, ShortcutFormat::Url, Some(&cover()))
        .unwrap()
        .unwrap();
    let path: Vec<_> = icon.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: the live path is NUL-terminated UTF-16. IMAGE_ICON and
    // LR_LOADFROMFILE return an owned, non-shared HICON destroyed below.
    let handle = unsafe { LoadImageW(std::ptr::null_mut(), path.as_ptr(), 1, 0, 0, 0x10) };
    assert!(!handle.is_null());
    // SAFETY: LoadImageW returned one owned icon and no LR_SHARED flag.
    assert_ne!(unsafe { DestroyIcon(handle) }, 0);
    let entry = super::super::create(
        data.path(),
        ShortcutFormat::Url,
        "Game",
        package.package_id,
        Some(&icon),
    )
    .unwrap();
    assert!(
        std::fs::read_to_string(entry)
            .unwrap()
            .contains("IconFile=")
    );
}
