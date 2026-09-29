//! The overlay hotkey (05-social §6.3, default `Shift+F3`).
//!
//! It is registered with the OS only while a game launched with the overlay runs, so the
//! launcher never holds the key while idle. Pressing it toggles the panel (the in-game
//! renderer gets `Panel`, the overlay window its view). Saving a new hotkey checks it first:
//! it must parse, and it must carry a modifier other than Shift or be a function key (a plain
//! or Shift+letter key would steal typing in games); then it is registered once to prove no
//! other application holds it.

use std::str::FromStr;
use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Why a hotkey cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyError {
    #[error("not a usable key combination")]
    Invalid,
    #[error("another application uses this key combination")]
    InUse { by: Option<String> },
}

fn is_function_key(code: Code) -> bool {
    matches!(
        code,
        Code::F1
            | Code::F2
            | Code::F3
            | Code::F4
            | Code::F5
            | Code::F6
            | Code::F7
            | Code::F8
            | Code::F9
            | Code::F10
            | Code::F11
            | Code::F12
            | Code::F13
            | Code::F14
            | Code::F15
            | Code::F16
            | Code::F17
            | Code::F18
            | Code::F19
            | Code::F20
            | Code::F21
            | Code::F22
            | Code::F23
            | Code::F24
    )
}

/// Parses and checks an accelerator such as `Shift+F3` or `Ctrl+Alt+O`.
pub fn validate(accelerator: &str) -> Result<Shortcut, HotkeyError> {
    let s = accelerator.trim();
    if s.is_empty() || s.len() > 64 {
        return Err(HotkeyError::Invalid);
    }
    let shortcut = Shortcut::from_str(s).map_err(|_| HotkeyError::Invalid)?;
    let strong = shortcut
        .mods
        .intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER | Modifiers::META);
    if !strong && !is_function_key(shortcut.key) {
        return Err(HotkeyError::Invalid);
    }
    Ok(shortcut)
}

/// Registers hotkeys with the OS (Tauri in the app, a fake in tests).
pub trait HotkeyHost: Send + Sync + 'static {
    fn register(&self, shortcut: Shortcut) -> Result<(), HotkeyError>;
    fn unregister(&self, shortcut: Shortcut);
}

/// The global-shortcut plugin.
pub struct TauriHotkeys<R: Runtime>(pub AppHandle<R>);

impl<R: Runtime> HotkeyHost for TauriHotkeys<R> {
    fn register(&self, shortcut: Shortcut) -> Result<(), HotkeyError> {
        let gs = self.0.global_shortcut();
        if gs.is_registered(shortcut) {
            return Ok(());
        }
        gs.register(shortcut).map_err(|error| {
            tracing::info!(%error, "overlay hotkey not registered");
            HotkeyError::InUse { by: None }
        })
    }

    fn unregister(&self, shortcut: Shortcut) {
        if let Err(error) = self.0.global_shortcut().unregister(shortcut) {
            tracing::debug!(%error, "overlay hotkey not unregistered");
        }
    }
}

/// The plugin, with a handler that toggles the overlay panel on key press.
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed
                && let Some(overlay) = app.try_state::<super::OverlayService>()
            {
                overlay.toggle_panel();
            }
        })
        .build()
}

/// Which hotkey is configured and whether it is currently held.
pub(super) struct HotkeyState {
    pub host: Option<Arc<dyn HotkeyHost>>,
    pub current: Shortcut,
    pub active: bool,
}

impl HotkeyState {
    pub fn new() -> Self {
        Self {
            host: None,
            current: validate("Shift+F3")
                .unwrap_or_else(|_| Shortcut::new(Some(Modifiers::SHIFT), Code::F3)),
            active: false,
        }
    }

    /// Holds the hotkey (a game with the overlay started). Failures are logged: the game and
    /// the Guide/PS hold still work.
    pub fn activate(&mut self) {
        if self.active {
            return;
        }
        if let Some(host) = &self.host {
            match host.register(self.current) {
                Ok(()) => self.active = true,
                Err(error) => tracing::warn!(%error, "overlay hotkey unavailable for this game"),
            }
        }
    }

    pub fn deactivate(&mut self) {
        if self.active
            && let Some(host) = &self.host
        {
            host.unregister(self.current);
        }
        self.active = false;
    }

    /// Checks and adopts a new hotkey. While active, the old one is swapped for it (and kept
    /// when the new one is taken); otherwise it is registered once to prove it is free.
    pub fn change(&mut self, accelerator: &str) -> Result<(), HotkeyError> {
        let next = validate(accelerator)?;
        if next == self.current {
            return Ok(());
        }
        let Some(host) = self.host.clone() else {
            self.current = next;
            return Ok(());
        };
        if self.active {
            host.unregister(self.current);
            if let Err(e) = host.register(next) {
                let _ = host.register(self.current);
                return Err(e);
            }
        } else {
            host.register(next)?;
            host.unregister(next);
        }
        self.current = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Mutex;

    use super::*;

    #[test]
    fn accelerators_need_a_real_modifier_or_a_function_key() {
        for ok in [
            "Shift+F3",
            "F9",
            "Ctrl+Alt+O",
            "Alt+Shift+Tab",
            "Super+O",
            "CmdOrCtrl+Shift+O",
        ] {
            assert!(validate(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "   ",
            "O",
            "Shift+O",
            "Shift",
            "Ctrl+",
            "Ctrl+Nope",
            "a+b+c",
            &"x".repeat(80),
        ] {
            assert_eq!(validate(bad).err(), Some(HotkeyError::Invalid), "{bad}");
        }
    }

    /// A fake OS: some shortcuts belong to another application.
    #[derive(Default)]
    struct FakeOs {
        taken: Vec<Shortcut>,
        held: Mutex<Vec<Shortcut>>,
    }

    impl HotkeyHost for FakeOs {
        fn register(&self, s: Shortcut) -> Result<(), HotkeyError> {
            if self.taken.contains(&s) {
                return Err(HotkeyError::InUse { by: None });
            }
            self.held.lock().unwrap().push(s);
            Ok(())
        }
        fn unregister(&self, s: Shortcut) {
            self.held.lock().unwrap().retain(|h| *h != s);
        }
    }

    #[test]
    fn held_only_while_a_game_runs_and_changes_are_conflict_checked() {
        let os = Arc::new(FakeOs {
            taken: vec![validate("Ctrl+Alt+T").unwrap()],
            ..FakeOs::default()
        });
        let mut hk = HotkeyState::new();
        hk.host = Some(os.clone());
        assert!(
            os.held.lock().unwrap().is_empty(),
            "nothing held while idle"
        );

        // Idle: a taken key is refused; a free one is proven free and not kept.
        assert_eq!(
            hk.change("Ctrl+Alt+T"),
            Err(HotkeyError::InUse { by: None })
        );
        assert_eq!(hk.change("Shift+O"), Err(HotkeyError::Invalid));
        hk.change("Ctrl+Alt+O").unwrap();
        assert!(os.held.lock().unwrap().is_empty());

        // A game starts: held; a change swaps it; a taken key keeps the old one.
        hk.activate();
        assert_eq!(
            *os.held.lock().unwrap(),
            vec![validate("Ctrl+Alt+O").unwrap()]
        );
        assert!(hk.change("Ctrl+Alt+T").is_err());
        assert_eq!(
            *os.held.lock().unwrap(),
            vec![validate("Ctrl+Alt+O").unwrap()]
        );
        hk.change("Shift+F3").unwrap();
        assert_eq!(
            *os.held.lock().unwrap(),
            vec![validate("Shift+F3").unwrap()]
        );

        // The game stops: released.
        hk.deactivate();
        assert!(os.held.lock().unwrap().is_empty());
    }
}
