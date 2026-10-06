//! Audit the documented package limits through the public parser.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use vgames_core::manifest::{
    EnvFault, LaunchFault, Manifest, ManifestError, SaveFault, parse_and_validate,
};

fn example() -> Manifest {
    let snapshot = include_str!(
        "../../crates/vgames-core/src/manifest/snapshots/vgames_core__manifest__tests__wire_format_snapshot.snap"
    );
    let json = snapshot.lines().find(|line| line.starts_with('{')).unwrap();
    parse_and_validate(json.as_bytes()).unwrap()
}

fn check(manifest: &Manifest) -> Result<Manifest, ManifestError> {
    parse_and_validate(&serde_json::to_vec(manifest).unwrap())
}

#[test]
fn bounded_counts() {
    let mut m = example();
    let t = m
        .launch
        .as_mut()
        .unwrap()
        .targets
        .first_mut()
        .unwrap()
        .clone();
    let launch = m.launch.as_mut().unwrap();
    launch.targets = (0..64)
        .map(|i| {
            let mut t = t.clone();
            if i > 0 {
                t.id = format!("target_{i}");
            }
            t
        })
        .collect();
    check(&m).unwrap();
    m.launch.as_mut().unwrap().targets.push(t);
    assert!(matches!(
        check(&m),
        Err(ManifestError::Launch(LaunchFault::TooManyTargets))
    ));

    let mut m = example();
    let location = m
        .saves
        .as_mut()
        .unwrap()
        .locations
        .first_mut()
        .unwrap()
        .clone();
    m.saves.as_mut().unwrap().locations = (0..64)
        .map(|i| {
            let mut l = location.clone();
            l.id = format!("save_{i}");
            l
        })
        .collect();
    check(&m).unwrap();
    m.saves.as_mut().unwrap().locations.push(location);
    assert!(matches!(
        check(&m),
        Err(ManifestError::Save {
            fault: SaveFault::TooMany,
            ..
        })
    ));

    let mut m = example();
    m.saves
        .as_mut()
        .unwrap()
        .locations
        .first_mut()
        .unwrap()
        .include = vec!["*.sav".into(); 256];
    m.saves
        .as_mut()
        .unwrap()
        .locations
        .first_mut()
        .unwrap()
        .exclude
        .clear();
    check(&m).unwrap();
    m.saves
        .as_mut()
        .unwrap()
        .locations
        .first_mut()
        .unwrap()
        .exclude
        .push("*.bak".into());
    assert!(matches!(
        check(&m),
        Err(ManifestError::Save {
            fault: SaveFault::TooManyPatterns,
            ..
        })
    ));

    let mut m = example();
    m.launch.as_mut().unwrap().targets.first_mut().unwrap().env = (0..256)
        .map(|i| (format!("GAME_{i}"), "v".into()))
        .collect();
    check(&m).unwrap();
    m.launch
        .as_mut()
        .unwrap()
        .targets
        .first_mut()
        .unwrap()
        .env
        .insert("EXTRA".into(), "v".into());
    assert!(matches!(
        check(&m),
        Err(ManifestError::Env {
            fault: EnvFault::TooMany,
            ..
        })
    ));
}
