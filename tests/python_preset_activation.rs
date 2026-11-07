use neuromita::settings::{
    PresetActivation, PresetActivationError, PresetRecovery, PythonPresetStage,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct TestRoot(PathBuf);
impl TestRoot {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "neuromita-activation-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn dir(&self) -> &Path {
        &self.0
    }
}
impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn review(root: &TestRoot) -> PathBuf {
    let presets = json!({"presets":{"18":{
        "id":18,"name":"Local import","base":3,"protocol_id":"","protocol_overrides":{},
        "url":"","default_model":"gemini-2.5-flash","key":"test-secret-only",
        "model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},
        "enabled":["max_tokens","thinking_level"]}
    }}});
    let templates = json!({"templates":{"3":{"name":"Google","protocol_id":"google_gemini_default","url":"","default_model":"gemini-2.5-flash"}}});
    let package = PythonPresetStage::prepare(
        &serde_json::to_vec(&presets).unwrap(),
        &serde_json::to_vec(&templates).unwrap(),
    )
    .unwrap();
    let path = root.dir().join("Ревью с пробелами");
    package.stage_new_directory(&path).unwrap();
    path
}

#[test]
fn approved_offline_activation_installs_both_files_without_touching_review() {
    let root = TestRoot::new();
    let output = root.dir().join("UserData");
    fs::create_dir(&output).unwrap();
    let review = review(&root);
    let new_catalog = fs::read(review.join("presets.json")).unwrap();
    let new_keys = fs::read(review.join("preset-credentials.json")).unwrap();
    PresetActivation::apply(&review, &output).unwrap();
    assert_eq!(fs::read(output.join("presets.json")).unwrap(), new_catalog);
    assert_eq!(
        fs::read(output.join("preset-credentials.json")).unwrap(),
        new_keys
    );
    assert_eq!(fs::read(review.join("presets.json")).unwrap(), new_catalog);
    assert_eq!(
        PresetActivation::recover(&output).unwrap(),
        PresetRecovery::NoJournal
    );
    PresetActivation::ensure_ready(&output).unwrap();
    assert!(matches!(
        PresetActivation::apply(&review, &output),
        Err(PresetActivationError::SnapshotConflict)
    ));
}

#[test]
fn refusing_conflicting_active_snapshot_leaves_profile_unchanged() {
    let root = TestRoot::new();
    let output = root.dir().join("UserData");
    fs::create_dir(&output).unwrap();
    let review = review(&root);
    fs::write(output.join("presets.json"), b"current profile").unwrap();
    let result = PresetActivation::apply(&review, &output);
    assert!(matches!(
        result,
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert_eq!(
        fs::read(output.join("presets.json")).unwrap(),
        b"current profile"
    );
    assert!(!output.join(".preset-activation-journal").exists());
}

#[test]
fn tampered_review_files_are_refused_before_creating_journal() {
    let root = TestRoot::new();
    let output = root.dir().join("UserData");
    fs::create_dir(&output).unwrap();
    let review = review(&root);
    let catalog = review.join("presets.json");
    let mut modified = fs::read(&catalog).unwrap();
    modified.extend_from_slice(b" ");
    fs::write(&catalog, modified).unwrap();
    assert!(matches!(
        PresetActivation::apply(&review, &output),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert!(!output.join(".preset-activation-journal").exists());
    assert!(!output.join("presets.json").exists());
}

#[test]
fn missing_python_source_backups_block_activation() {
    let root = TestRoot::new();
    let output = root.dir().join("UserData");
    fs::create_dir(&output).unwrap();
    let review = review(&root);
    fs::remove_file(review.join("python-source-api-templates.json")).unwrap();
    assert!(matches!(
        PresetActivation::apply(&review, &output),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert!(!output.join(".preset-activation-journal").exists());
}

#[test]
fn unsafe_review_and_profile_paths_are_blocked() {
    let root = TestRoot::new();
    let review = review(&root);
    assert!(matches!(
        PresetActivation::apply(&review, Path::new("relative")),
        Err(PresetActivationError::UnsafePath)
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.dir(), root.dir().join("alias")).unwrap();
        assert!(matches!(
            PresetActivation::apply(&review, &root.dir().join("alias")),
            Err(PresetActivationError::UnsafePath)
        ));
    }
}
