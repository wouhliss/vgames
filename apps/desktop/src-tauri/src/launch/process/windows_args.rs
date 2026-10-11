//! UTF-16 process inputs, with the MSVC argument escaping rules.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

use crate::launch::PreparedLaunch;

pub(super) fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<_> = value.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    units.push(0);
    Ok(units)
}

pub(super) fn command_line(launch: &PreparedLaunch) -> io::Result<Vec<u16>> {
    // argv[0] has a special grammar: a Windows executable path cannot contain quotes.
    let program = wide(launch.program.as_os_str())?;
    if program.contains(&u16::from(b'"')) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut result = vec![u16::from(b'"')];
    result.extend_from_slice(&program[..program.len() - 1]);
    result.push(u16::from(b'"'));
    for argument in &launch.args {
        result.push(u16::from(b' '));
        quote(argument, &mut result)?;
    }
    // CreateProcessW limits this buffer to 32,767 UTF-16 code units, including NUL.
    if result.len() >= 32_767 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    result.push(0);
    Ok(result)
}

fn quote(argument: &OsStr, result: &mut Vec<u16>) -> io::Result<()> {
    let units = wide(argument)?;
    result.push(u16::from(b'"'));
    let mut slashes = 0;
    for &unit in &units[..units.len() - 1] {
        if unit == u16::from(b'\\') {
            slashes += 1;
            continue;
        }
        if unit == u16::from(b'"') {
            result.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2 + 1));
        } else {
            result.extend(std::iter::repeat_n(u16::from(b'\\'), slashes));
        }
        result.push(unit);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2));
    result.push(u16::from(b'"'));
    Ok(())
}

pub(super) fn environment(launch: &PreparedLaunch) -> io::Result<Vec<u16>> {
    let mut result = Vec::new();
    for (key, value) in &launch.env {
        if key.is_empty() || key.contains('=') {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let key = wide(OsStr::new(key))?;
        result.extend_from_slice(&key[..key.len() - 1]);
        result.push(u16::from(b'='));
        result.extend(wide(value)?);
    }
    if result.is_empty() {
        result.push(0);
    }
    result.push(0);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use proptest::prelude::*;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    use super::*;

    fn parsed(command: &[u16]) -> Vec<OsString> {
        let mut count = 0;
        #[allow(unsafe_code)]
        // SAFETY: command is NUL-terminated and count points to writable storage.
        let pointer = unsafe { CommandLineToArgvW(command.as_ptr(), &mut count) };
        assert!(!pointer.is_null());
        #[allow(unsafe_code)]
        // SAFETY: the API returned count entries, each a NUL-terminated string.
        let result = unsafe {
            std::slice::from_raw_parts(pointer, usize::try_from(count).unwrap())
                .iter()
                .map(|&word| {
                    let mut length = 0;
                    while *word.add(length) != 0 {
                        length += 1;
                    }
                    OsString::from_wide(std::slice::from_raw_parts(word, length))
                })
                .collect()
        };
        #[allow(unsafe_code)]
        // SAFETY: CommandLineToArgvW transfers a LocalAlloc buffer, freed once here.
        unsafe {
            LocalFree(pointer.cast());
        }
        result
    }

    proptest! {
        #[test]
        fn argv_round_trips(arguments in prop::collection::vec("[^\u{0}]{0,40}", 0..12)) {
            let mut launch = crate::launch::testgame::launch(std::path::Path::new("."), "exit");
            launch.args = arguments.iter().map(OsString::from).collect();
            let words = parsed(&command_line(&launch).unwrap());
            prop_assert_eq!(&words[0], launch.program.as_os_str());
            prop_assert_eq!(&words[1..], launch.args.as_slice());
        }
    }

    #[test]
    fn quotes_empty_spaces_unicode_and_trailing_backslashes() {
        let mut launch = crate::launch::testgame::launch(std::path::Path::new("."), "exit");
        launch.args = ["", "a b", "\t", "é 🦀", "\\", "a\\\"b", "end\\\\"]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert_eq!(
            &parsed(&command_line(&launch).unwrap())[1..],
            launch.args.as_slice()
        );
        launch.args.push(OsString::from_wide(&[0]));
        assert!(command_line(&launch).is_err());
    }

    #[test]
    fn environment_is_explicit_and_double_terminated() {
        let mut launch = crate::launch::testgame::launch(std::path::Path::new("."), "exit");
        launch.env.clear();
        assert_eq!(environment(&launch).unwrap(), [0, 0]);
        launch.env.insert("FOO".into(), "bar".into());
        assert_eq!(
            environment(&launch).unwrap(),
            "FOO=bar\0\0".encode_utf16().collect::<Vec<_>>()
        );
        launch.env.insert("BAD=KEY".into(), "value".into());
        assert!(environment(&launch).is_err());
    }
}
