use neuromita::generation::PresetId;
use neuromita::settings::{
    AppSettings, PresetActivation, PresetActivationError, PresetRecovery, PythonPresetStage,
    PythonUnmappedPolicy, SettingsTransfer, StoredSettings,
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

#[test]
fn composite_python_import_merges_supported_fields_and_selected_preset_atomically() {
    let root = Temp::new();
    let (review, active, old) = fixture(
        &root,
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","CHAT_FONT_SIZE":18,"REMINDERS_ENABLED":true}"#,
    );
    PresetActivation::apply_with_imported_settings(&review, &active, PythonUnmappedPolicy::Reject)
        .unwrap();
    let before = SettingsTransfer::decode(&old).unwrap().into_stored();
    let after = SettingsTransfer::decode(&fs::read(active.join("settings.json")).unwrap())
        .unwrap()
        .into_stored();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.settings.presets.active, PresetId(18));
    assert_eq!(
        after.settings.interface.language,
        neuromita::settings::Language::English
    );
    assert_eq!(after.settings.interface.font_size, 18);
    assert!(after.settings.reminders.enabled);
    assert_eq!(
        after.settings.interface.hide_private,
        before.settings.interface.hide_private
    );
    assert_eq!(
        fs::read(review.join("python-source-settings.json")).unwrap(),
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","CHAT_FONT_SIZE":18,"REMINDERS_ENABLED":true}"#,
    );
    assert_eq!(
        PresetActivation::recover(&active).unwrap(),
        PresetRecovery::NoJournal
    );
}

#[test]
fn composite_import_unknown_fields_require_explicit_approval_before_any_write() {
    let root = Temp::new();
    let (review, active, old) = fixture(
        &root,
        br#"{"LAST_API_PRESET_ID":"18","LANGUAGE":"EN","UNSUPPORTED_SECRET":"never-print"}"#,
    );
    assert!(matches!(
        PresetActivation::apply_with_imported_settings(
            &review,
            &active,
            PythonUnmappedPolicy::Reject
        ),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
    assert!(!active.join("presets.json").exists());
    assert!(!active.join(".preset-activation-journal").exists());
    assert!(matches!(
        PresetActivation::apply_with_imported_settings(
            &review,
            &active,
            PythonUnmappedPolicy::SkipAfterReview,
        ),
        Err(PresetActivationError::SnapshotConflict)
    ));
    let inspected = PresetActivation::review_imported_settings(&review, &active).unwrap();
    assert_eq!(inspected.skipped_keys, ["UNSUPPORTED_SECRET"]);
    assert_eq!(inspected.imported_keys, ["LANGUAGE"]);
    PresetActivation::apply_with_reviewed_settings(&review, &active, &inspected).unwrap();
    let after = SettingsTransfer::decode(&fs::read(active.join("settings.json")).unwrap())
        .unwrap()
        .into_stored();
    assert_eq!(after.settings.presets.active, PresetId(18));
    assert_eq!(
        after.settings.interface.language,
        neuromita::settings::Language::English
    );
}

#[test]
fn composite_import_rejects_invalid_known_fields_even_if_unmapped_are_approved() {
    let root = Temp::new();
    let (review, active, old) = fixture(
        &root,
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"unknown","UNKNOWN_KEY":true}"#,
    );
    assert!(matches!(
        PresetActivation::apply_with_imported_settings(
            &review,
            &active,
            PythonUnmappedPolicy::SkipAfterReview,
        ),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
    assert!(!active.join("presets.json").exists());
}

#[test]
fn legacy_preference_only_activation_does_not_import_other_python_keys() {
    let root = Temp::new();
    let (review, active, old) = fixture(
        &root,
        br#"{"LAST_API_PRESET_ID":18,"CHAT_FONT_SIZE":18,"REMINDERS_ENABLED":true}"#,
    );
    PresetActivation::apply_with_preferences(&review, &active).unwrap();
    let before = SettingsTransfer::decode(&old).unwrap().into_stored();
    let after = SettingsTransfer::decode(&fs::read(active.join("settings.json")).unwrap())
        .unwrap()
        .into_stored();
    assert_eq!(
        after.settings.interface.font_size,
        before.settings.interface.font_size
    );
    assert_eq!(
        after.settings.reminders.enabled,
        before.settings.reminders.enabled
    );
    assert_eq!(after.settings.presets.active, PresetId(18));
}

#[test]
fn exact_review_rejects_stale_unmapped_value_before_journaling() {
    let root = Temp::new();
    let original = br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","UNKNOWN_TOKEN":"alpha"}"#;
    let (review, active, old) = fixture(&root, original);
    let inspected = PresetActivation::review_imported_settings(&review, &active).unwrap();
    assert_eq!(inspected.skipped_keys, ["UNKNOWN_TOKEN"]);
    assert_eq!(inspected.imported_keys, ["LANGUAGE"]);
    assert_eq!(inspected.source_sha256.len(), 64);
    assert_eq!(inspected.target_settings_sha256.len(), 64);
    fs::write(
        review.join("python-source-settings.json"),
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","UNKNOWN_TOKEN":"beta"}"#,
    )
    .unwrap();
    assert!(matches!(
        PresetActivation::apply_with_reviewed_settings(&review, &active, &inspected),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
    assert!(!active.join("presets.json").exists());
    assert!(!active.join("preset-credentials.json").exists());
    assert!(!active.join(".preset-activation-journal").exists());
    assert_eq!(
        fs::read(review.join("python-source-settings.json")).unwrap(),
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","UNKNOWN_TOKEN":"beta"}"#
    );
}

#[test]
fn exact_review_rejects_a_new_unmapped_name_and_a_changed_mapped_value() {
    for modified in [
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","UNKNOWN_TOKEN":1,"EXTRA":true}"#.as_slice(),
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"RU","UNKNOWN_TOKEN":1}"#,
    ] {
        let root = Temp::new();
        let (review, active, old) = fixture(
            &root,
            br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","UNKNOWN_TOKEN":1}"#,
        );
        let inspected = PresetActivation::review_imported_settings(&review, &active).unwrap();
        fs::write(review.join("python-source-settings.json"), modified).unwrap();
        assert!(matches!(
            PresetActivation::apply_with_reviewed_settings(&review, &active, &inspected),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
        assert!(!active.join(".preset-activation-journal").exists());
    }
}

#[test]
fn exact_review_rejects_missing_extra_or_reordered_approval() {
    let root = Temp::new();
    let (review, active, old) = fixture(
        &root,
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","X_UNKNOWN":1,"Y_UNKNOWN":false}"#,
    );
    let inspected = PresetActivation::review_imported_settings(&review, &active).unwrap();
    assert_eq!(inspected.skipped_keys, ["X_UNKNOWN", "Y_UNKNOWN"]);
    let mut approvals = Vec::new();
    let mut missing = inspected.clone();
    missing.skipped_keys.pop();
    approvals.push(missing);
    let mut extra = inspected.clone();
    extra.skipped_keys.push("Z_UNKNOWN".into());
    approvals.push(extra);
    let mut reordered = inspected.clone();
    reordered.skipped_keys.reverse();
    approvals.push(reordered);
    let mut duplicate = inspected.clone();
    duplicate.skipped_keys.push("X_UNKNOWN".into());
    approvals.push(duplicate);
    let mut tampered_digest = inspected.clone();
    tampered_digest.source_sha256.replace_range(..1, "0");
    tampered_digest.source_sha256.push('0');
    approvals.push(tampered_digest);
    for approval in &approvals {
        assert!(matches!(
            PresetActivation::apply_with_reviewed_settings(&review, &active, approval),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
        assert!(!active.join(".preset-activation-journal").exists());
    }
    PresetActivation::apply_with_reviewed_settings(&review, &active, &inspected).unwrap();
    assert_eq!(
        PresetActivation::recover(&active).unwrap(),
        PresetRecovery::NoJournal
    );
}

#[test]
fn exact_review_rejects_stale_target_settings_before_journaling() {
    let root = Temp::new();
    let (review, active, old) = fixture(
        &root,
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","UNKNOWN":7}"#,
    );
    let inspected = PresetActivation::review_imported_settings(&review, &active).unwrap();
    let mut target: serde_json::Value = serde_json::from_slice(&old).unwrap();
    target["revision"] = json!(13);
    let new_target = serde_json::to_vec_pretty(&target).unwrap();
    fs::write(active.join("settings.json"), &new_target).unwrap();
    assert!(matches!(
        PresetActivation::apply_with_reviewed_settings(&review, &active, &inspected),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert_eq!(fs::read(active.join("settings.json")).unwrap(), new_target);
    assert!(!active.join(".preset-activation-journal").exists());
}

#[test]
fn reviewed_import_rejects_invalid_mapped_values_even_with_exact_approval() {
    let root = Temp::new();
    let (review, active, old) = fixture(
        &root,
        br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"invalid","UNKNOWN_TOKEN":true}"#,
    );
    assert!(matches!(
        PresetActivation::review_imported_settings(&review, &active),
        Err(PresetActivationError::SnapshotConflict)
    ));
    assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
    assert!(!active.join(".preset-activation-journal").exists());
}

#[test]
fn staged_preference_review_binds_all_source_bytes_even_without_unmapped_keys() {
    for combined in [false, true] {
        let root = Temp::new();
        let (review, active, old) = fixture(
            &root,
            br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"EN","REMINDERS_ENABLED":true}"#,
        );
        fs::write(
            review.join("python-source-settings.json"),
            br#"{"LAST_API_PRESET_ID":18,"LANGUAGE":"RU","REMINDERS_ENABLED":true}"#,
        )
        .unwrap();
        let outcome = if combined {
            PresetActivation::apply_with_imported_settings(
                &review,
                &active,
                PythonUnmappedPolicy::Reject,
            )
        } else {
            PresetActivation::apply_with_preferences(&review, &active)
        };
        assert!(matches!(
            outcome,
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert_eq!(fs::read(active.join("settings.json")).unwrap(), old);
        assert!(!active.join(".preset-activation-journal").exists());
        assert!(!active.join("presets.json").exists());
    }
}
