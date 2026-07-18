use splice_compat::{
    descriptors, discover, harness, matrix, Behavior, FakeTui, Observation, Probe, Tier,
};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{collections::BTreeMap, fs};
#[test]
fn fixed_descriptors_default_to_fallback_without_runtime_evidence() {
    assert_eq!(
        descriptors()
            .iter()
            .map(|descriptor| descriptor.id)
            .collect::<Vec<_>>(),
        [
            "codex",
            "claude",
            "opencode",
            "gemini",
            "aider",
            "agy",
            "generic-tui"
        ]
    );
    let report = matrix(&BTreeMap::new());
    let row = &report.rows[0];
    assert_eq!((row.os, row.shell), ("unknown", "unknown"));
    assert_eq!(row.constraints, ["runtime behavior is not evidenced"]);
}
#[test]
fn unrecognized_observations_keep_generic_fallback_and_image_paste_unsupported() {
    let report = matrix(&BTreeMap::from([(
        "unrecognized-cli".to_owned(),
        Observation::available("9.9.9", ["not a supported descriptor"]),
    )]));
    let generic = report.rows.last().unwrap();
    assert_eq!(
        (report.rows.len(), generic.tier, generic.skip.as_deref()),
        (7, Tier::Fallback, Some("no runtime evidence collected"))
    );
    assert!(report.rows.iter().all(|row| row.tier == Tier::Fallback));
    assert!(matches!(
        generic.probes.get(&Behavior::ImagePaste),
        Some(Probe::Unsupported { .. })
    ));
}
#[test]
fn deterministic_harness_discovers_fixture_and_renders_semantic_evidence() {
    let fixture = std::env::temp_dir().join(format!("splice-compat-{}", std::process::id()));
    let _ = fs::remove_dir_all(&fixture);
    fs::create_dir(&fixture).unwrap();
    fs::write(fixture.join("codex"), "fixture").unwrap();
    let path = std::env::join_paths([&fixture]).unwrap();
    #[cfg(unix)]
    {
        let missing = discover("codex", path.as_os_str(), None, vec![]);
        assert_eq!(missing, Observation::unavailable("codex not found on PATH"));
        fs::set_permissions(fixture.join("codex"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(windows)]
    {
        fs::rename(fixture.join("codex"), fixture.join("codex.CMD")).unwrap();
        std::env::set_var("PATHEXT", ".EXE;.CMD");
        let direct = discover("codex.CMD", path.as_os_str(), Some("direct".into()), vec![]);
        assert_eq!(direct, Observation::available("direct", [] as [&str; 0]));
    }
    let found = discover(
        "codex",
        path.as_os_str(),
        Some("0.42.0".into()),
        vec!["node >= 20".into()],
    );
    assert_eq!(found, Observation::available("0.42.0", ["node >= 20"]));
    assert_eq!(
        discover("claude", path.as_os_str(), None, vec![]),
        Observation::unavailable("claude not found on PATH")
    );
    let report = harness(
        path.as_os_str(),
        Some("0.42.0".into()),
        vec!["node >= 20".into()],
        FakeTui::new(true, true, false, true),
    );
    let human = report.human();
    assert!(
        human.contains("prerequisites=[\"node >= 20\"]")
            && human.contains("Startup: Passed")
            && human.contains("Input: Passed")
            && human.contains("Interrupt: Skipped")
            && human.contains("Liveness: Passed")
    );
    let json = report.json().unwrap();
    assert!(json.contains("\"evidence_confidence\": \"unevidenced\""));
    fs::remove_dir_all(fixture).unwrap();
}
