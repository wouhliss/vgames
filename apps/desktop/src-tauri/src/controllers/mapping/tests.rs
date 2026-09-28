use super::*;

fn press(buttons: &[Button]) -> PadState {
    let mut state = PadState::default();
    for &b in buttons {
        state.set(b, true);
    }
    state
}

fn mapped(kind: ControllerKind, profile: &Profile, button: Button) -> u16 {
    Mapper::new(kind, profile).map(&press(&[button])).buttons
}

#[test]
fn playstation_pads_map_by_position() {
    // Cross (south) → A, Circle (east) → B, Square (west) → X, Triangle (north) → Y.
    for kind in [ControllerKind::Dualsense, ControllerKind::Dualshock4] {
        let p = Profile::default();
        assert_eq!(mapped(kind, &p, Button::South), XButton::A.bit());
        assert_eq!(mapped(kind, &p, Button::East), XButton::B.bit());
        assert_eq!(mapped(kind, &p, Button::West), XButton::X.bit());
        assert_eq!(mapped(kind, &p, Button::North), XButton::Y.bit());
        // Touchpad click → View/Back, PS → Guide.
        assert_eq!(mapped(kind, &p, Button::Touchpad), XButton::Back.bit());
        assert_eq!(mapped(kind, &p, Button::Guide), XButton::Guide.bit());
        // `nintendo_labels` only affects Nintendo pads.
        let labels = Profile {
            nintendo_labels: true,
            ..Profile::default()
        };
        assert_eq!(mapped(kind, &labels, Button::South), XButton::A.bit());
    }
}

#[test]
fn switch_pro_maps_by_position_or_by_label() {
    let kind = ControllerKind::SwitchPro;
    // By position: the bottom button (labelled B) acts as A.
    let p = Profile::default();
    assert_eq!(mapped(kind, &p, Button::South), XButton::A.bit());
    assert_eq!(mapped(kind, &p, Button::East), XButton::B.bit());
    assert_eq!(mapped(kind, &p, Button::West), XButton::X.bit());
    assert_eq!(mapped(kind, &p, Button::North), XButton::Y.bit());
    // By label: A (east) → A, B (south) → B, X (north) → X, Y (west) → Y.
    let labels = Profile {
        nintendo_labels: true,
        ..Profile::default()
    };
    assert_eq!(mapped(kind, &labels, Button::East), XButton::A.bit());
    assert_eq!(mapped(kind, &labels, Button::South), XButton::B.bit());
    assert_eq!(mapped(kind, &labels, Button::North), XButton::X.bit());
    assert_eq!(mapped(kind, &labels, Button::West), XButton::Y.bit());
}

#[test]
fn every_button_maps_and_chords_combine() {
    let mapper = Mapper::new(ControllerKind::Dualsense, &Profile::default());
    for button in Button::ALL {
        assert_ne!(mapper.map(&press(&[button])).buttons, 0, "{button:?}");
    }
    let report = mapper.map(&press(&[Button::South, Button::DpadUp, Button::Start]));
    assert_eq!(
        report.buttons,
        XButton::A.bit() | XButton::DpadUp.bit() | XButton::Start.bit()
    );
    let mut state = press(&[Button::South]);
    state.set(Button::South, false);
    assert_eq!(mapper.map(&state).buttons, 0);
}

#[test]
fn remaps_apply_after_the_layout() {
    let profile = Profile {
        remap: vec![
            Remap {
                from: Button::South,
                to: Some(XButton::B),
            },
            Remap {
                from: Button::Touchpad,
                to: None,
            },
        ],
        ..Profile::default()
    };
    let mapper = Mapper::new(ControllerKind::Dualsense, &profile);
    assert_eq!(
        mapper.map(&press(&[Button::South])).buttons,
        XButton::B.bit()
    );
    assert_eq!(mapper.map(&press(&[Button::Touchpad])).buttons, 0);
}

#[test]
fn triggers_scale_to_a_byte() {
    let mapper = Mapper::new(ControllerKind::Dualsense, &Profile::default());
    let report = |l, r| {
        let state = PadState {
            left_trigger: l,
            right_trigger: r,
            ..PadState::default()
        };
        let x = mapper.map(&state);
        (x.left_trigger, x.right_trigger)
    };
    assert_eq!(report(0, 32767), (0, 255));
    assert_eq!(report(16384, -5), (128, 0));
    assert_eq!(report(i16::MIN, i16::MAX), (0, 255));
}

#[test]
fn sticks_have_a_radial_deadzone_and_point_y_up() {
    let mapper = Mapper::new(ControllerKind::Dualsense, &Profile::default());
    let stick = |x, y| {
        let state = PadState {
            left: (x, y),
            ..PadState::default()
        };
        mapper.map(&state).left
    };
    // Inside 8 %: nothing, including a diagonal whose axes are each below it.
    assert_eq!(stick(2000, 0), (0, 0));
    assert_eq!(stick(1800, 1800), (0, 0));
    // Full deflection stays full; SDL "down" is XInput "down" (negative).
    assert_eq!(stick(32767, 0), (32767, 0));
    assert_eq!(stick(0, 32767), (0, -32767));
    assert_eq!(stick(0, -32768), (0, 32767));
    // Just outside the deadzone the output starts near zero (rescaled).
    let (x, _) = stick(3000, 0);
    assert!((0..500).contains(&x), "{x}");
    // Corners never exceed the XInput range.
    let (x, y) = stick(i16::MIN, i16::MIN);
    assert!(x >= -32767 && y >= -32767, "{x} {y}");
}

#[test]
fn stick_settings_apply() {
    let profile = Profile {
        right_stick: StickSettings {
            deadzone: 0,
            anti_deadzone: 20,
            invert_y: true,
        },
        ..Profile::default()
    };
    let mapper = Mapper::new(ControllerKind::Xinput, &profile);
    let state = PadState {
        right: (0, 100),
        ..PadState::default()
    };
    let (_, y) = mapper.map(&state).right;
    // Inverted: SDL down stays positive; the anti-deadzone lifts a tiny input to ≥ 20 %.
    assert!(y >= 6553, "{y}");
}

#[test]
fn profiles_are_validated() {
    let json = br#"{"nintendo_labels":true,"left_stick":{"deadzone":10},"remap":[{"from":"south","to":"b"}]}"#;
    let profile = Profile::parse(json).unwrap();
    assert!(profile.nintendo_labels);
    assert_eq!(profile.left_stick.deadzone, 10);
    assert_eq!(profile.right_stick, StickSettings::default());
    assert_eq!(Profile::parse(b"{}").unwrap(), Profile::default());

    assert!(matches!(
        Profile::parse(br#"{"left_stick":{"deadzone":100}}"#),
        Err(ProfileError::Range)
    ));
    assert!(matches!(
        Profile::parse(br#"{"unknown":1}"#),
        Err(ProfileError::Syntax(_))
    ));
    assert!(matches!(
        Profile::parse(br#"{"remap":[{"from":"south","to":"a"},{"from":"south","to":null}]}"#),
        Err(ProfileError::DuplicateRemap(Button::South))
    ));
    let huge = format!(r#"{{"remap":[{}]}}"#, "1,".repeat(10_000));
    assert!(Profile::parse(huge.as_bytes()).is_err());
}

proptest::proptest! {
    #[test]
    fn mapping_never_panics(buttons in proptest::prelude::any::<u32>(),
                            lx in proptest::prelude::any::<i16>(), ly in proptest::prelude::any::<i16>(),
                            lt in proptest::prelude::any::<i16>(), dz in 0u8..100, ad in 0u8..100) {
        let profile = Profile {
            left_stick: StickSettings { deadzone: dz, anti_deadzone: ad, invert_y: false },
            ..Profile::default()
        };
        let mapper = Mapper::new(ControllerKind::SwitchPro, &profile);
        let _ = mapper.map(&PadState { buttons, left: (lx, ly), right: (ly, lx), left_trigger: lt, right_trigger: lt });
    }
}
