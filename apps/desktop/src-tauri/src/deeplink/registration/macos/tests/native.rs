#![allow(unsafe_code)]
use super::*;
use std::{ffi::c_void, ptr::NonNull};

type Ref = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> Ref;
    fn CFRelease(value: Ref);
    fn CFEqual(left: Ref, right: Ref) -> u8;
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
}
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSCopyAllHandlersForURLScheme(scheme: Ref) -> Ref;
}
struct Owned(NonNull<c_void>);
impl Owned {
    fn string(text: &str) -> Self {
        // SAFETY: CF copies this live UTF-8 byte slice; allocator is default.
        let value = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                text.as_ptr(),
                text.len().try_into().unwrap(),
                0x0800_0100,
                0,
            )
        };
        Self(NonNull::new(value.cast_mut()).unwrap())
    }
    fn as_ref(&self) -> Ref {
        self.0.as_ptr()
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: each non-null Create/Copy reference has exactly one owner.
        unsafe { CFRelease(self.as_ref()) };
    }
}
fn registered(scheme: &str, identifier: &str) -> bool {
    let scheme = Owned::string(scheme);
    let identifier = Owned::string(identifier);
    // SAFETY: the live CFString arguments and Copy return follow CF ownership.
    let array = unsafe { LSCopyAllHandlersForURLScheme(scheme.as_ref()) };
    let Some(array) = NonNull::new(array.cast_mut()).map(Owned) else {
        return false;
    };
    // SAFETY: Launch Services returns a CFArray of CFStrings. Borrowed entries
    // remain alive until the array owner drops after this loop.
    unsafe {
        (0..CFArrayGetCount(array.as_ref())).any(|index| {
            CFEqual(
                CFArrayGetValueAtIndex(array.as_ref(), index),
                identifier.as_ref(),
            ) != 0
        })
    }
}
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let Ok(mut child) = std::process::Command::new(LSREGISTER)
            .arg("-u")
            .arg(&self.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    }
}
#[tokio::test]
async fn launch_services_restores_a_deleted_registration_for_a_repaired_bundle() {
    let (_root, executable) = fixture();
    std::fs::copy("/usr/bin/true", &executable).unwrap();
    let bundle = bundle(&executable).unwrap().to_owned();
    let _cleanup = Cleanup(bundle.clone());
    let id = format!("app.vgames.fixture.{}", uuid::Uuid::now_v7().simple());
    let scheme = format!("vgames-fixture-{}", uuid::Uuid::now_v7().simple());
    let path = bundle.join("Contents/Info.plist");
    let mut value = Value::from_file(&path).unwrap();
    value
        .as_dictionary_mut()
        .unwrap()
        .insert("CFBundleIdentifier".into(), Value::String(id.clone()));
    value.to_file_xml(&path).unwrap();
    let cancel = CancellationToken::new();
    repair(executable.clone(), id.clone(), scheme.clone(), &cancel)
        .await
        .unwrap();
    assert!(registered(&scheme, &id));
    let mut command = tokio::process::Command::new(LSREGISTER);
    command
        .arg("-u")
        .arg(&bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(5), command.status())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert!(!registered(&scheme, &id));
    let mut value = Value::from_file(&path).unwrap();
    value
        .as_dictionary_mut()
        .unwrap()
        .remove("CFBundleURLTypes");
    value.to_file_xml(&path).unwrap();
    repair(executable, id.clone(), scheme.clone(), &cancel)
        .await
        .unwrap();
    assert!(registered(&scheme, &id));
}
