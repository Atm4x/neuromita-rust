use neuromita::generation::PresetId;
use neuromita::settings::{
    AppSettings, PresetActivation, PresetActivationError, PresetRecovery, PythonPresetStage,
    SettingsTransfer, StoredSettings,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("neuromita-three-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn python_inputs() -> (Vec<u8>, Vec<u8>) {
    let template = json!({"templates":{"3":{"name":"Google","protocol_id":"google_gemini_default","url":"","default_model":"gemini-2.5-flash"}}});
    let preset = json!({
        "id":18,"name":"Personal","base":3,"protocol_id":"","protocol_overrides":{},
        "url":"","default_model":"gemini-2.5-flash","key":"private-fixture",
        "model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},"enabled":["max_tokens","thinking_level"]}
    });
    (
        serde_json::to_vec(&json!({"presets":{"18":preset},"order":[18]})).unwrap(),
        serde_json::to_vec(&template).unwrap(),
    )
}

fn fixture(root: &Temp, python_settings: &[u8]) -> (PathBuf, PathBuf, Vec<u8>) {
    let (p, t) = python_inputs();
    let stage = PythonPresetStage::prepare(&p, &t).unwrap();
    let (stage, _) = stage.with_preferences(python_settings).unwrap();
    let review = root.path().join("review с пробелами");
    let active = root.path().join("UserData");
    fs::create_dir(&active).unwrap();
    stage.stage_new_directory(&review).unwrap();
    let mut settings = AppSettings::initial_profile();
    settings.interface.font_size = 27;
    settings.reminders.enabled = false;
    let old = SettingsTransfer::new(StoredSettings {
        revision: 9,
        settings,
    })
    .unwrap()
    .encode()
    .unwrap();
    fs::write(active.join("settings.json"), &old).unwrap();
    (review, active, old)
}

#[test]
fn activates_three_documents_and_only_changes_selected_preset_and_revision() {
    let root = Temp::new();
    let (review, active, old) = fixture(&root, br#"{"LAST_API_PRESET_ID":"18"}"#);
    let before = SettingsTransfer::decode(&old).unwrap().into_stored();
    let python_source = fs::read(review.join("python-source-settings.json")).unwrap();
    PresetActivation::apply_with_preferences(&review, &active).unwrap();
    let mut after = SettingsTransfer::decode(&fs::read(active.join("settings.json")).unwrap())
        .unwrap()
        .into_stored();
    assert_eq!(after.settings.presets.active, PresetId(18));
    assert_eq!(after.revision, before.revision + 1);
    after.settings.presets.active = before.settings.presets.active;
    after.revision = before.revision;
    assert_eq!(after, before);
    for name in ["presets.json", "preset-credentials.json"] {
        assert_eq!(
            fs::read(active.join(name)).unwrap(),
            fs::read(review.join(name)).unwrap()
        );
    }
    assert_eq!(
        fs::read(review.join("python-source-settings.json")).unwrap(),
        python_source
    );
    assert_eq!(
        PresetActivation::recover(&active).unwrap(),
        PresetRecovery::NoJournal
    );
    assert!(
        !active
            .join("python-preset-preferences-review.json")
            .exists()
    );
}

#[test]
fn no_settings_document_no_write_and_no_journal() {
    let root = Temp::new();
    let (review, active, _) = fixture(&root, br#"{"LAST_API_PRESET_ID":18}"#);
    fs::remove_file(active.join("settings.json")).unwrap();
    assert!(matches!(
        PresetActivation::apply_with_preferences(&review, &active),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert!(!active.join("presets.json").exists());
    assert!(!active.join(".preset-activation-journal").exists());
}

#[test]
fn absent_selected_user_id_cannot_implicitly_select_first_preset() {
    for py in [br#"{}"#.as_slice(), br#"{"LAST_API_PRESET_ID":null}"#] {
        let root = Temp::new();
        let (review, active, old) = fixture(&root, py);
        assert!(matches!(
            PresetActivation::apply_with_preferences(&review, &active),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
        assert!(!active.join("presets.json").exists());
    }
}

#[test]
fn tampered_preference_review_is_rejected_before_any_write() {
    let root = Temp::new();
    let (review, active, old) = fixture(&root, br#"{"LAST_API_PRESET_ID":18}"#);
    let path = review.join("python-preset-preferences-review.json");
    let mut data: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    data["selected_user_id"] = json!(999);
    fs::write(&path, serde_json::to_vec_pretty(&data).unwrap()).unwrap();
    assert!(matches!(
        PresetActivation::apply_with_preferences(&review, &active),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
    assert!(!active.join(".preset-activation-journal").exists());
}

#[test]
fn legacy_two_file_apply_does_not_mutate_settings_even_with_preference_review() {
    let root = Temp::new();
    let (review, active, old) = fixture(&root, br#"{"LAST_API_PRESET_ID":18}"#);
    PresetActivation::apply(&review, &active).unwrap();
    assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
}
