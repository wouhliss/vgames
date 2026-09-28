//! Thin OS wrappers: preallocation, free space and filesystem limits.
//!
//! Every `unsafe` block here is a single FFI call on a handle or a
//! NUL-terminated path we own, with a `// SAFETY:` note (AGENTS.md §5).

use std::fs::File;
use std::io;
use std::path::Path;

/// Largest file FAT32 can hold (4 GiB − 1).
pub const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;

/// Sets `file` to exactly `len` bytes and asks the filesystem to reserve the
/// blocks, so a full disk surfaces now and the file is not fragmented. Data
/// already in the file (a resumed install) is kept.
pub fn preallocate(file: &File, len: u64) -> io::Result<()> {
    if file.metadata()?.len() > len {
        file.set_len(len)?;
    }
    if len == 0 {
        return Ok(());
    }
    reserve(file, len)
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn reserve(file: &File, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let off =
        libc::off_t::try_from(len).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `file` owns a valid open descriptor for the whole call; mode 0
    // allocates [0, len) and extends the size without touching existing data.
    let rc = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, off) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // Filesystems without fallocate (some network and FUSE mounts).
        Some(libc::EOPNOTSUPP | libc::ENOSYS | libc::EINVAL) => file.set_len(len),
        _ => Err(error),
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn reserve(file: &File, len: u64) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALLOCATION_INFO, FileAllocationInfo, SetFileInformationByHandle,
    };
    file.set_len(len)?;
    let info = FILE_ALLOCATION_INFO {
        AllocationSize: i64::try_from(len)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
    };
    // SAFETY: the handle is valid for the call and `info` is a properly sized
    // FILE_ALLOCATION_INFO that outlives it.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileAllocationInfo,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        // FAT and some network shares do not support it; the size is set.
        if error.kind() == io::ErrorKind::StorageFull {
            return Err(error);
        }
        tracing::debug!(%error, "FileAllocationInfo not supported");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn reserve(file: &File, len: u64) -> io::Result<()> {
    file.set_len(len)
}

/// Bytes available to the current user on the filesystem holding `path`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn available_space(path: &Path) -> io::Result<u64> {
    let c = c_path(path)?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` is NUL-terminated and `st` is a writable statvfs.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, so it initialized `st`.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// Total bytes on the filesystem holding `path`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn total_space(path: &Path) -> io::Result<u64> {
    let c = c_path(path)?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` is NUL-terminated and `st` is a writable statvfs.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, so it initialized `st`.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Ok((st.f_blocks as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn available_space(path: &Path) -> io::Result<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide = wide_path(path);
    let mut available = 0u64;
    // SAFETY: `wide` is NUL-terminated; the out pointer is valid, the others may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn total_space(path: &Path) -> io::Result<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide = wide_path(path);
    let mut total = 0u64;
    // SAFETY: `wide` is NUL-terminated; the out pointer is valid, the others may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut total,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(total)
}

/// The largest file the filesystem holding `path` accepts, when it is small
/// enough to matter (FAT32: 4 GiB − 1). `None` means no practical limit.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn max_file_size(path: &Path) -> io::Result<Option<u64>> {
    const MSDOS_SUPER_MAGIC: i64 = 0x4d44;
    let c = c_path(path)?;
    let mut st = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `c` is NUL-terminated and `st` is a writable statfs.
    let rc = unsafe { libc::statfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statfs succeeded, so it initialized `st`.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    let fat = st.f_type as i64 == MSDOS_SUPER_MAGIC;
    Ok(fat.then_some(FAT32_MAX_FILE))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn max_file_size(path: &Path) -> io::Result<Option<u64>> {
    let c = c_path(path)?;
    let mut st = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `c` is NUL-terminated and `st` is a writable statfs.
    let rc = unsafe { libc::statfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statfs succeeded, so it initialized `st`.
    let st = unsafe { st.assume_init() };
    let name: Vec<u8> = st
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Ok((name == b"msdos").then_some(FAT32_MAX_FILE))
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn max_file_size(path: &Path) -> io::Result<Option<u64>> {
    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
    let wide = wide_path(path);
    let mut volume = [0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `volume` is a writable buffer of the given length.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fs_name = [0u16; 64];
    // SAFETY: `volume` was NUL-terminated by the previous call; unused outputs are null.
    let ok = unsafe {
        GetVolumeInformationW(
            volume.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let end = fs_name
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(fs_name.len());
    let name = String::from_utf16_lossy(fs_name.get(..end).unwrap_or_default());
    Ok(match name.as_str() {
        "FAT32" => Some(FAT32_MAX_FILE),
        "FAT" | "FAT16" | "FAT12" => Some(2 * 1024 * 1024 * 1024 - 1),
        _ => None,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn max_file_size(_path: &Path) -> io::Result<Option<u64>> {
    Ok(None)
}

#[cfg(unix)]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// True when the error means "the disk is full".
pub fn is_disk_full(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::StorageFull || error.kind() == io::ErrorKind::QuotaExceeded {
        return true;
    }
    #[cfg(unix)]
    if matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT)) {
        return true;
    }
    #[cfg(windows)]
    // ERROR_HANDLE_DISK_FULL, ERROR_DISK_FULL.
    if matches!(error.raw_os_error(), Some(39 | 112)) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preallocate_sets_size_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"hello").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        preallocate(&file, 1 << 20).unwrap();
        drop(file);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 1 << 20);
        assert_eq!(&bytes[..5], b"hello");
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let blocks = std::fs::metadata(&path).unwrap().blocks() * 512;
            assert!(blocks >= 1 << 20, "blocks are reserved ({blocks})");
        }
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        preallocate(&file, 3).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hel");
    }

    #[test]
    fn free_space_and_limits_are_readable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(available_space(dir.path()).unwrap() > 0);
        assert!(total_space(dir.path()).unwrap() >= available_space(dir.path()).unwrap());
        assert_eq!(max_file_size(dir.path()).unwrap(), None);
    }

    #[test]
    fn disk_full_detection() {
        assert!(is_disk_full(&io::Error::from(io::ErrorKind::StorageFull)));
        assert!(!is_disk_full(&io::Error::from(io::ErrorKind::NotFound)));
        #[cfg(unix)]
        assert!(is_disk_full(&io::Error::from_raw_os_error(libc::ENOSPC)));
    }
}
