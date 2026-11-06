use std::{fs, path::PathBuf};

use neuromita::settings::{
    AppSettings, JsonSettingsRepository, Language, PythonSettingsMigration, PythonUnmappedPolicy,
    SettingsImportFormat, SettingsRepository, SettingsTransferError, StoredSettings, Volume,
};
use serde_json::json;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("python-settings-import-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source(value: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

fn baseline() -> StoredSettings {
    let mut settings = AppSettings::initial_profile();
    settings.presets.active = neuromita::settings::PresetId(88);
    settings.voice.voice = "existing-rust-voice".to_string();
    settings.interface.hide_private = false;
    StoredSettings {
        revision: 7,
        settings,
    }
}

#[test]
fn preview_overlays_only_confirmed_python_fields_and_lists_omissions() {
    let current = baseline();
    let input = source(json!({
        "LANGUAGE": "EN", "CHAT_FONT_SIZE": 21, "HIDE_PRIVATE": true,
        "VOICEOVER_LOCAL_VOLUME": 42,
        "PROMPT_SET_Crazy": "My Custom Set",
        "LAST_API_PRESET_ID": 17,
        "CHAR_PROVIDER_Crazy": 93,
        "GOOGLE_API_KEY": "not copied or displayed",
        "SCREEN_CAPTURE_QUALITY": "25"
    }));
    let preview = PythonSettingsMigration.preview(&input, &current).unwrap();
    assert_eq!(preview.imported_keys.len(), 5);
    assert!(
        preview
            .skipped_keys
            .contains(&"LAST_API_PRESET_ID".to_string())
    );
    assert!(preview.skipped_keys.contains(&"GOOGLE_API_KEY".to_string()));
    assert!(preview.changed);
    let migrated = preview.transfer.stored();
    assert_eq!(migrated.revision, current.revision);
    assert_eq!(migrated.settings.interface.language, Language::English);
    assert_eq!(migrated.settings.interface.font_size, 21);
    assert!(migrated.settings.interface.hide_private);
    assert_eq!(migrated.settings.voice.volume, Volume::new(42).unwrap());
    assert_eq!(
        migrated.settings.characters.crazy.prompt_set.as_deref(),
        Some("My Custom Set")
    );
    assert_eq!(
        migrated.settings.presets.active,
        current.settings.presets.active
    );
    assert_eq!(migrated.settings.voice.voice, "existing-rust-voice");
    assert!(
        !String::from_utf8(serde_json::to_vec(migrated).unwrap())
            .unwrap()
            .contains("not copied")
    );
}

#[test]
fn preview_rejects_malformed_or_invalid_known_values_without_defaults() {
    let current = baseline();
    for input in [
        b"not json".to_vec(),
        source(json!([])),
        source(json!({"LANGUAGE":"DE"})),
        source(json!({"HIDE_PRIVATE":"perhaps"})),
        source(json!({"CHAT_FONT_SIZE":0})),
        source(json!({"CHAT_FONT_SIZE":true})),
        source(json!({"VOICEOVER_LOCAL_VOLUME":125})),
        source(json!({"VOICEOVER_LOCAL_VOLUME":-2})),
        source(json!({"LAST_API_PRESET_ID":3})),
        source(json!({"revision":1,"settings":{}})),
        source(json!({"PROMPT_SET_Crazy":42})),
        br#"{"LANGUAGE":"EN","LANGUAGE":"RU"}"#.to_vec(),
        source(json!({"CHAT_FONT_SIZE":"12.5"})),
    ] {
        assert!(
            PythonSettingsMigration.preview(&input, &current).is_err(),
            "{input:?}"
        );
    }
    assert!(matches!(
        PythonSettingsMigration.preview(b"{", &current),
        Err(SettingsTransferError::MalformedJson(_))
    ));
    assert_eq!(baseline(), current);
}

#[test]
fn offline_import_creates_prewrite_backup_and_is_idempotent() {
    let temp = Directory::new();
    let path = temp.path("user data ура.json");
    let repository = JsonSettingsRepository::new(path.clone());
    let current = baseline();
    repository.save(&current).unwrap();
    let original = fs::read(&path).unwrap();
    let bytes = source(json!({
        "LANGUAGE":"EN", "HIDE_PRIVATE":true, "CHAT_FONT_SIZE":19,
        "LAST_API_PRESET_ID":1234
    }));
    let readonly_preview = repository.preview_python(&bytes).unwrap();
    assert_eq!(readonly_preview.transfer.stored().revision, 7);
    assert_eq!(fs::read(&path).unwrap(), original);
    let result = repository
        .import_python_with_policy(&bytes, PythonUnmappedPolicy::SkipAfterReview)
        .unwrap();
    assert!(result.preview.changed);
    assert_eq!(result.preview.transfer.stored().revision, 8);
    let backup = result.backup_path.unwrap();
    assert_eq!(fs::read(&backup).unwrap(), original);
    let restored = repository.load().unwrap();
    assert_eq!(restored.revision, 8);
    assert_eq!(
        restored.settings.presets.active,
        current.settings.presets.active
    );
    let after = fs::read(&path).unwrap();
    let repeated = repository
        .import_python_with_policy(&bytes, PythonUnmappedPolicy::SkipAfterReview)
        .unwrap();
    assert!(!repeated.preview.changed);
    assert!(repeated.backup_path.is_none());
    assert_eq!(fs::read(&path).unwrap(), after);
    assert_eq!(repository.load().unwrap().revision, 8);
}

#[test]
fn invalid_import_and_failed_publication_leave_original_bytes_intact() {
    let temp = Directory::new();
    let path = temp.path("settings.json");
    let repository = JsonSettingsRepository::new(path.clone());
    let current = baseline();
    repository.save(&current).unwrap();
    let before = fs::read(&path).unwrap();
    for input in [
        source(json!({"LANGUAGE":"unsupported"})),
        source(json!({"HIDE_PRIVATE":null})),
        b"{".to_vec(),
    ] {
        assert!(repository.import_python(&input).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert_eq!(repository.load().unwrap(), current);
}

#[test]
fn new_target_uses_declared_initial_profile_and_unrelated_fields_are_not_invented() {
    let temp = Directory::new();
    let path = temp.path("fresh.json");
    let repository = JsonSettingsRepository::new(path.clone());
    let before = fs::read_dir(&temp.0).unwrap().count();
    let input = source(json!({"LANGUAGE":"RU", "HIDE_PRIVATE":false}));
    let preview = repository.preview_python(&input).unwrap();
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), before);
    assert_eq!(preview.transfer.stored().revision, 0);
    let applied = repository.import_python(&input).unwrap();
    assert!(applied.backup_path.is_none());
    assert_eq!(repository.load().unwrap().revision, 1);
    assert_eq!(
        repository.load().unwrap().settings.interface.hide_private,
        false
    );
}

#[test]
fn python_entry_text_values_are_supported_without_coercing_invalid_strings() {
    let current = baseline();
    let bytes = source(
        json!({"CHAT_FONT_SIZE":" 22 ","VOICEOVER_LOCAL_VOLUME":"65","PROMPT_SET_Crazy":null}),
    );
    let preview = PythonSettingsMigration.preview(&bytes, &current).unwrap();
    assert_eq!(preview.transfer.stored().settings.interface.font_size, 22);
    assert_eq!(preview.transfer.stored().settings.voice.volume.get(), 65);
    assert_eq!(
        preview
            .transfer
            .stored()
            .settings
            .characters
            .crazy
            .prompt_set,
        None
    );
}

#[test]
fn python_prior_eula_consent_preserved_and_malformed_value_rejected() {
    let current = baseline();
    let preview = PythonSettingsMigration
        .preview(
            &source(json!({
                "EULA_ACCEPTED": true
            })),
            &current,
        )
        .unwrap();
    assert!(preview.transfer.stored().settings.onboarding.eula_accepted);
    assert_eq!(preview.imported_keys, vec!["EULA_ACCEPTED"]);
    assert!(
        PythonSettingsMigration
            .preview(
                &source(json!({
                    "EULA_ACCEPTED": "true"
                })),
                &current
            )
            .is_err()
    );
}

#[test]
fn fresh_target_is_created_even_when_python_values_match_initial_defaults() {
    let temp = Directory::new();
    let path = temp.path("new.json");
    let repository = JsonSettingsRepository::new(path.clone());
    let input = source(json!({"LANGUAGE":"RU"}));
    assert!(repository.preview_python(&input).unwrap().changed);
    let applied = repository.import_python(&input).unwrap();
    assert!(applied.preview.changed);
    assert_eq!(repository.load().unwrap().revision, 1);
    assert!(path.exists());
}

#[test]
fn pr109_unique_flags_merge_into_current_rust_values() {
    let current = baseline();
    let python = source(json!({
        "REMINDERS_ENABLED": "off",
        "VOICE_LANGUAGE": "EN",
        "LOCAL_VOICE_LOAD_LAST": 1,
        "LOCAL_VOICE_INIT_ON_REQUEST": "no",
        "LANGUAGE": "EN",
        "CHAT_FONT_SIZE": "20",
        "VOICEOVER_LOCAL_VOLUME": "33",
        "PROMPT_SET_Crazy": "My Custom Set"
    }));
    let planned = PythonSettingsMigration.preview(&python, &current).unwrap();
    let after = &planned.transfer.stored().settings;
    assert!(!after.reminders.enabled);
    assert_eq!(after.speech.tts.local_language, "en");
    assert!(after.speech.tts.local_auto_load);
    assert!(!after.speech.tts.local_lazy_init);
    assert_eq!(after.voice.volume.get(), 33);
    assert_eq!(after.interface.font_size, 20);
    assert_eq!(
        after.characters.crazy.prompt_set.as_deref(),
        Some("My Custom Set")
    );
    assert_eq!(after.presets, current.settings.presets);
    assert_eq!(planned.imported_keys.len(), 8);
    assert!(planned.skipped_keys.is_empty());
}

#[test]
fn unknown_fields_are_rejected_by_default_and_never_overwrite() {
    let dir = Directory::new();
    let target = dir.path("settings.json");
    let repo = JsonSettingsRepository::new(target.clone());
    repo.save(&baseline()).unwrap();
    let before = fs::read(&target).unwrap();
    let python = source(json!({"LANGUAGE":"EN", "API_KEY":"private-token"}));
    let preview = repo.preview_python(&python).unwrap();
    assert_eq!(preview.skipped_keys, vec!["API_KEY"]);
    let err = repo.import_python(&python).unwrap_err().to_string();
    assert!(err.contains("API_KEY"));
    assert!(!err.contains("private-token"));
    assert_eq!(fs::read(&target).unwrap(), before);
    let applied = repo
        .import_python_with_policy(&python, PythonUnmappedPolicy::SkipAfterReview)
        .unwrap();
    assert!(applied.preview.changed);
    assert_eq!(
        repo.load().unwrap().settings.interface.language,
        Language::English
    );
    assert_eq!(fs::read(&applied.backup_path.unwrap()).unwrap(), before);
}

#[test]
fn generic_python_replacement_is_forbidden() {
    let dir = Directory::new();
    let target = dir.path("settings.json");
    let repo = JsonSettingsRepository::new(target.clone());
    repo.save(&baseline()).unwrap();
    let before = fs::read(&target).unwrap();
    assert!(
        repo.import_from(
            SettingsImportFormat::Python,
            &source(json!({"LANGUAGE":"EN"}))
        )
        .is_err()
    );
    assert_eq!(fs::read(target).unwrap(), before);
}

#[test]
fn invalid_new_fields_are_rejected_without_coercing_defaults() {
    let current = baseline();
    for source in [
        source(json!({"REMINDERS_ENABLED": "what"})),
        source(json!({"VOICE_LANGUAGE": "fr"})),
        source(json!({"LOCAL_VOICE_LOAD_LAST": 3})),
        source(json!({"LOCAL_VOICE_INIT_ON_REQUEST": null})),
        source(json!({"EULA_ACCEPTED": "true"})),
    ] {
        assert!(PythonSettingsMigration.preview(&source, &current).is_err());
    }
}
