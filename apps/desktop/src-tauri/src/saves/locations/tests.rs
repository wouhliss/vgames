use std::collections::BTreeMap;

use tokio_util::sync::CancellationToken;

use super::*;
use crate::saves::scan;

fn folders(root: &Path) -> KnownFolders {
    let at = |name: &str| Some(root.join(name));
    KnownFolders {
        home: at("home"),
        documents: at("documents"),
        saved_games: at("saved_games"),
        appdata: at("appdata"),
        localappdata: at("localappdata"),
        xdg_data: at("xdg_data"),
        xdg_config: at("xdg_config"),
    }
}

fn location(id: &str, base: SaveBase) -> SaveLocation {
    SaveLocation {
        id: id.into(),
        base,
        path: "Studio/Game".into(),
        include: vec![],
        exclude: vec![],
    }
}

#[test]
fn native_bases_map_to_their_known_folder_and_install_to_the_install_root() {
    let root = tempfile::tempdir().unwrap();
    let known = folders(root.path());
    let install = root.path().join("install");
    for (base, folder) in [
        (SaveBase::Home, "home"),
        (SaveBase::Documents, "documents"),
        (SaveBase::SavedGames, "saved_games"),
        (SaveBase::Appdata, "appdata"),
        (SaveBase::Localappdata, "localappdata"),
        (SaveBase::XdgData, "xdg_data"),
        (SaveBase::XdgConfig, "xdg_config"),
    ] {
        assert_eq!(
            base_dir(base, &known, &install, &Target::Native).unwrap(),
            root.path().join(folder),
            "{base:?}"
        );
    }
    assert_eq!(
        base_dir(SaveBase::Install, &known, &install, &Target::Native).unwrap(),
        install
    );
}

#[test]
fn compat_bases_resolve_inside_the_prefix_with_windows_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    let prefix = root.path().join("prefix");
    let install = root.path().join("install");
    let target = Target::Compat {
        prefix: prefix.clone(),
        runner: RunnerKind::Proton,
    };
    let user = prefix.join("drive_c/users").join(compat::PROTON_USER);
    // Native folders are never consulted for a compat launch.
    let empty = KnownFolders::default();
    assert_eq!(
        base_dir(SaveBase::SavedGames, &empty, &install, &target).unwrap(),
        user.join("Saved Games")
    );
    assert_eq!(
        base_dir(SaveBase::Appdata, &empty, &install, &target).unwrap(),
        user.join("AppData/Roaming")
    );
    assert_eq!(
        base_dir(SaveBase::Install, &empty, &install, &target).unwrap(),
        install
    );
    // A save written through Proton keeps its Windows-relative snapshot name.
    let save = user.join("Saved Games/Studio/Game/slot.sav");
    std::fs::create_dir_all(save.parent().unwrap()).unwrap();
    std::fs::write(&save, b"proton").unwrap();
    let opened = open_all(
        &[location("game", SaveBase::SavedGames)],
        &empty,
        &install,
        &target,
    )
    .unwrap();
    let scan = scan::scan(&opened, &BTreeMap::new(), &CancellationToken::new()).unwrap();
    assert_eq!(scan.files.keys().collect::<Vec<_>>(), ["game/slot.sav"]);
}

#[test]
fn an_undefined_known_folder_refuses_the_whole_set_and_creates_nothing() {
    let root = tempfile::tempdir().unwrap();
    let mut known = folders(root.path());
    known.documents = None;
    let install = root.path().join("install");
    assert!(matches!(
        open_all(
            &[
                location("a", SaveBase::Home),
                location("b", SaveBase::Documents)
            ],
            &known,
            &install,
            &Target::Native,
        ),
        Err(Error::Invalid)
    ));
    known.home = Some("relative/home".into());
    assert!(base_dir(SaveBase::Home, &known, &install, &Target::Native).is_err());
    // Resolving and opening missing folders never creates them.
    let opened = open_all(
        &[location("a", SaveBase::Appdata)],
        &folders(root.path()),
        &install,
        &Target::Native,
    )
    .unwrap();
    assert_eq!(opened.len(), 1);
    assert!(!root.path().join("appdata").exists());
}

#[test]
fn this_machine_follows_the_known_folder_table() {
    let known = KnownFolders::native();
    let home = known.home.clone().expect("a home folder");
    assert!(home.is_absolute());
    for folder in [
        &known.saved_games,
        &known.appdata,
        &known.localappdata,
        &known.xdg_data,
        &known.xdg_config,
    ] {
        assert!(folder.as_ref().is_some_and(|path| path.is_absolute()));
    }
    #[cfg(target_os = "macos")]
    {
        let support = home.join("Library/Application Support");
        assert_eq!(known.documents, Some(home.join("Documents")));
        for folder in [&known.saved_games, &known.appdata, &known.localappdata] {
            assert_eq!(folder.as_deref(), Some(support.as_path()));
        }
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(known.saved_games, known.xdg_data);
        assert_eq!(known.localappdata, known.xdg_data);
        assert_eq!(known.appdata, known.xdg_config);
    }
    #[cfg(windows)]
    {
        assert_eq!(known.xdg_config, known.appdata);
        assert_eq!(known.xdg_data, known.localappdata);
        assert!(
            known
                .saved_games
                .as_ref()
                .is_some_and(|path| path.ends_with("Saved Games")),
            "{:?}",
            known.saved_games
        );
    }
}
