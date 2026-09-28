//! Desktop shortcuts (A2-T10): small files that open `vgames://launch/<id>`,
//! so the launcher runs its usual checks. Windows `.url`, Linux `.desktop`
//! (`xdg-open`), macOS `.webloc`. The renderers are pure and tested on every
//! platform; [`create`] and [`remove`] block (run them on a blocking pool).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Longest shortcut name, in characters (before the extension).
const MAX_NAME_CHARS: usize = 80;
/// Shortcuts are tiny; anything larger at our path is not ours.
const MAX_SHORTCUT_BYTES: u64 = 16 * 1024;
const FALLBACK_NAME: &str = "vgames game";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutFormat {
    /// Windows Internet shortcut.
    Url,
    /// freedesktop.org desktop entry.
    Desktop,
    /// macOS web location (property list).
    Webloc,
}

impl ShortcutFormat {
    /// The format for the running OS.
    pub const fn native() -> Self {
        if cfg!(windows) {
            Self::Url
        } else if cfg!(target_os = "macos") {
            Self::Webloc
        } else {
            Self::Desktop
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Desktop => "desktop",
            Self::Webloc => "webloc",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShortcutError {
    #[error("the icon path cannot be written into a shortcut")]
    IconPath,
    #[error("no free shortcut name")]
    NoFreeName,
    #[error("cannot {action} {path}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn launch_url(package_id: Uuid) -> String {
    format!("vgames://launch/{}", package_id.hyphenated())
}

/// A file name stem that is safe on every OS: no separators, control or
/// reserved characters, no leading/trailing dots or spaces, no Windows device
/// names, and at most [`MAX_NAME_CHARS`] characters.
pub fn sanitize_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = collapsed.chars().take(MAX_NAME_CHARS).collect();
    let trimmed = truncated.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        return FALLBACK_NAME.to_owned();
    }
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes().get(3).is_some_and(u8::is_ascii_digit));
    // Windows checks the part before the first dot, so prefix rather than suffix.
    if reserved {
        format!("vgames {trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// The file contents. `icon` is a launcher-rendered image in app data.
pub fn render(
    format: ShortcutFormat,
    name: &str,
    package_id: Uuid,
    icon: Option<&Path>,
) -> Result<String, ShortcutError> {
    let url = launch_url(package_id);
    let icon = icon
        .map(|path| {
            path.to_str()
                .filter(|s| !s.chars().any(char::is_control))
                .ok_or(ShortcutError::IconPath)
        })
        .transpose()?;
    Ok(match format {
        ShortcutFormat::Url => {
            let mut text = format!("[InternetShortcut]\r\nURL={url}\r\n");
            if let Some(icon) = icon {
                text.push_str(&format!("IconFile={icon}\r\nIconIndex=0\r\n"));
            }
            text
        }
        ShortcutFormat::Desktop => {
            let mut text = format!(
                "[Desktop Entry]\nType=Application\nVersion=1.0\nName={}\nExec=xdg-open {url}\nTerminal=false\nCategories=Game;\n",
                escape_desktop(name)
            );
            if let Some(icon) = icon {
                text.push_str(&format!("Icon={}\n", escape_desktop(icon)));
            }
            text
        }
        ShortcutFormat::Webloc => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\t<key>URL</key>\n\t<string>{url}</string>\n</dict>\n</plist>\n"
        ),
    })
}

/// Desktop entry string values: `\` must be escaped; names are single-line already.
fn escape_desktop(value: &str) -> String {
    value.replace('\\', "\\\\")
}

/// Writes a new shortcut in `dir` named after `title`, never replacing an
/// existing file (`Name (2).ext` and so on). Returns its path.
pub fn create(
    dir: &Path,
    format: ShortcutFormat,
    title: &str,
    package_id: Uuid,
    icon: Option<&Path>,
) -> Result<PathBuf, ShortcutError> {
    let name = sanitize_name(title);
    let contents = render(format, &name, package_id, icon)?;
    for n in 1..=100u32 {
        let file_name = match n {
            1 => format!("{name}.{}", format.extension()),
            _ => format!("{name} ({n}).{}", format.extension()),
        };
        let path = dir.join(file_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Desktop environments only run desktop entries marked executable.
            let mode = if format == ShortcutFormat::Desktop {
                0o755
            } else {
                0o644
            };
            options.mode(mode);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ShortcutError::Io {
                    action: "create",
                    path,
                    source,
                });
            }
        };
        let written = file
            .write_all(contents.as_bytes())
            .and_then(|()| file.sync_all());
        if let Err(source) = written {
            let _ = fs::remove_file(&path);
            return Err(ShortcutError::Io {
                action: "write",
                path,
                source,
            });
        }
        return Ok(path);
    }
    Err(ShortcutError::NoFreeName)
}

/// Removes a shortcut this launcher created, if it is still a regular file
/// that opens `package_id`. Anything else at that path (a replaced file, a
/// link, another package's shortcut) is left alone. Returns whether it was removed.
pub fn remove(path: &Path, package_id: Uuid) -> Result<bool, ShortcutError> {
    let io_error = |action, source| ShortcutError::Io {
        action,
        path: path.to_owned(),
        source,
    };
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error("inspect", error)),
    };
    if !metadata.is_file() || metadata.len() > MAX_SHORTCUT_BYTES {
        return Ok(false);
    }
    let text = fs::read_to_string(path).map_err(|e| io_error("read", e))?;
    let url = launch_url(package_id);
    let ours = text.lines().any(|line| {
        let line = line.trim();
        line == format!("URL={url}")
            || line == format!("Exec=xdg-open {url}")
            || line == format!("<string>{url}</string>")
    });
    if !ours {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error("remove", error)),
    }
}

/// The user's desktop folder, if the OS has one.
pub fn desktop_dir() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests;
