//! The bundle path comes only from the native current executable.
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use plist::{Dictionary, Value};
use tokio_util::sync::CancellationToken;

const LIMIT: u64 = 128 * 1024;
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

fn bundle(executable: &Path) -> Result<&Path, ()> {
    let macos = executable.parent().ok_or(())?;
    let contents = macos.parent().ok_or(())?;
    let bundle = contents.parent().ok_or(())?;
    if !executable.is_absolute()
        || macos.file_name() != Some("MacOS".as_ref())
        || contents.file_name() != Some("Contents".as_ref())
        || bundle.extension() != Some("app".as_ref())
    {
        return Err(());
    }
    Ok(bundle)
}

/// Repair only our known URL declaration in an unsigned bundle. Retain every
/// other property and all other URL schemes, including binary plist input.
fn ensure_types(executable: &Path, identifier: &str, scheme: &str) -> Result<PathBuf, ()> {
    let bundle = bundle(executable)?;
    let contents = bundle.join("Contents");
    let path = contents.join("Info.plist");
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|_| ())?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > LIMIT {
        return Err(());
    }
    let mut value = Value::from_reader(std::io::Cursor::new(bytes)).map_err(|_| ())?;
    let dictionary = value.as_dictionary_mut().ok_or(())?;
    if dictionary
        .get("CFBundleIdentifier")
        .and_then(Value::as_string)
        != Some(identifier)
        || dictionary
            .get("CFBundleExecutable")
            .and_then(Value::as_string)
            != executable.file_name().and_then(|name| name.to_str())
    {
        return Err(());
    }
    let found = dictionary
        .get("CFBundleURLTypes")
        .and_then(Value::as_array)
        .is_some_and(|types| {
            types.iter().any(|entry| {
                entry
                    .as_dictionary()
                    .and_then(|entry| entry.get("CFBundleURLSchemes"))
                    .and_then(Value::as_array)
                    .is_some_and(|schemes| {
                        schemes.iter().any(|item| item.as_string() == Some(scheme))
                    })
            })
        });
    if found {
        return Ok(bundle.to_owned());
    }
    // Do not invalidate an optional OS signature. Unsigned is the project's
    // current distribution policy; signed bundle damage needs reinstallation.
    if contents
        .join("_CodeSignature")
        .try_exists()
        .map_err(|_| ())?
    {
        return Err(());
    }
    if !dictionary.contains_key("CFBundleURLTypes") {
        dictionary.insert("CFBundleURLTypes".into(), Value::Array(Vec::new()));
    }
    let types = dictionary
        .get_mut("CFBundleURLTypes")
        .ok_or(())?
        .as_array_mut()
        .ok_or(())?;
    let mut entry = Dictionary::new();
    entry.insert("CFBundleURLName".into(), Value::String(identifier.into()));
    entry.insert(
        "CFBundleURLSchemes".into(),
        Value::Array(vec![Value::String(scheme.into())]),
    );
    types.push(Value::Dictionary(entry));
    let temporary = contents.join(format!(".vgames-links-{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| ())?;
        file.set_permissions(metadata.permissions())
            .map_err(|_| ())?;
        value.to_writer_xml(&mut file).map_err(|_| ())?;
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|_| ())?;
        std::fs::rename(&temporary, &path).map_err(|_| ())?;
        #[cfg(unix)]
        std::fs::File::open(&contents)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ())?;
        Ok(bundle.to_owned())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) async fn repair(
    executable: PathBuf,
    identifier: String,
    scheme: String,
    cancel: &CancellationToken,
) -> Result<(), ()> {
    if cancel.is_cancelled() {
        return Err(());
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let bundle =
        tokio::task::spawn_blocking(move || ensure_types(&executable, &identifier, &scheme))
            .await
            .map_err(|_| ())??;
    let mut command = tokio::process::Command::new(LSREGISTER);
    command
        .arg("-f")
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(()),
        result = tokio::time::timeout_at(deadline, command.status()) => match result {
            Ok(Ok(status)) if status.success() => Ok(()),
            _ => Err(()),
        },
    }
}

#[cfg(test)]
mod tests;
