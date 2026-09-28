use super::*;

const ID: Uuid = Uuid::from_u128(0x0192_a6f0_1c2d_7e3f_8a9b_0c1d_2e3f_4a5b);

#[test]
fn names_are_safe_on_every_os() {
    let cases = [
        ("Half-Life 2", "Half-Life 2"),
        ("  Portal: Still Alive  ", "Portal Still Alive"),
        ("a/b\\c<d>e\"f|g?h*i", "a b c d e f g h i"),
        ("Line\nbreak\tand\u{7}bell", "Line break and bell"),
        ("...hidden...", "hidden"),
        ("", "vgames game"),
        ("???", "vgames game"),
        ("CON", "vgames CON"),
        ("nul.txt", "vgames nul.txt"),
        ("com7", "vgames com7"),
        ("COM", "COM"),
        ("Console", "Console"),
        ("Café 東京", "Café 東京"),
    ];
    for (input, expected) in cases {
        assert_eq!(sanitize_name(input), expected, "{input:?}");
    }
    let long = sanitize_name(&"é".repeat(500));
    assert_eq!(long.chars().count(), 80);
}

#[test]
fn windows_url_file() {
    let text = render(
        ShortcutFormat::Url,
        "Game",
        ID,
        Some(Path::new("C:\\icons\\game.ico")),
    )
    .unwrap();
    assert_eq!(
        text,
        "[InternetShortcut]\r\nURL=vgames://launch/0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b\r\n\
         IconFile=C:\\icons\\game.ico\r\nIconIndex=0\r\n"
    );
}

#[test]
fn linux_desktop_entry() {
    let text = render(
        ShortcutFormat::Desktop,
        "My \\ Game",
        ID,
        Some(Path::new("/icons/game.png")),
    )
    .unwrap();
    assert_eq!(
        text,
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName=My \\\\ Game\n\
         Exec=xdg-open vgames://launch/0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b\n\
         Terminal=false\nCategories=Game;\nIcon=/icons/game.png\n"
    );
}

#[test]
fn macos_webloc() {
    let text = render(ShortcutFormat::Webloc, "Game", ID, None).unwrap();
    assert!(text.contains("<string>vgames://launch/0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b</string>"));
    assert!(text.starts_with("<?xml"));
}

#[test]
fn icon_paths_cannot_inject_lines() {
    let result = render(
        ShortcutFormat::Url,
        "Game",
        ID,
        Some(Path::new("a\r\nURL=https://evil")),
    );
    assert!(matches!(result, Err(ShortcutError::IconPath)));
}

#[test]
fn create_never_overwrites_and_remove_only_deletes_ours() {
    let dir = tempfile::tempdir().unwrap();
    let format = ShortcutFormat::native();
    fs::write(
        dir.path().join(format!("Game.{}", format.extension())),
        b"user file",
    )
    .unwrap();

    let first = create(dir.path(), format, "Game", ID, None).unwrap();
    let second = create(dir.path(), format, "Game", ID, None).unwrap();
    assert_eq!(
        first.file_name().unwrap().to_str().unwrap(),
        format!("Game (2).{}", format.extension())
    );
    assert_eq!(
        second.file_name().unwrap().to_str().unwrap(),
        format!("Game (3).{}", format.extension())
    );
    assert_eq!(
        fs::read_to_string(&first).unwrap(),
        render(format, "Game", ID, None).unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&first).unwrap().permissions().mode() & 0o777;
        let expected = if format == ShortcutFormat::Desktop {
            0o755
        } else {
            0o644
        };
        assert_eq!(mode & !0o022, expected & !0o022);
    }

    // The user's file and another package's shortcut are kept.
    assert!(!remove(&dir.path().join(format!("Game.{}", format.extension())), ID).unwrap());
    assert!(!remove(&first, Uuid::from_u128(7)).unwrap());
    assert!(remove(&first, ID).unwrap());
    assert!(!first.exists());
    assert!(!remove(&first, ID).unwrap());

    // A shortcut the user replaced with their own content is kept.
    fs::write(&second, b"[Desktop Entry]\nExec=something-else\n").unwrap();
    assert!(!remove(&second, ID).unwrap());
    assert!(second.exists());
}

#[cfg(unix)]
#[test]
fn remove_never_follows_links() {
    let dir = tempfile::tempdir().unwrap();
    let target = create(dir.path(), ShortcutFormat::Desktop, "Real", ID, None).unwrap();
    let link = dir.path().join("Link.desktop");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(!remove(&link, ID).unwrap());
    assert!(link.exists() && target.exists());
}
