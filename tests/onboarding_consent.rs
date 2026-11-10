use std::{fs, path::PathBuf};

use neuromita::settings::{
    AppSettings, JsonSettingsRepository, SettingsRepository, StoredSettings,
};

fn scratch() -> PathBuf {
    let path = std::env::temp_dir().join(format!("neuromita-onboarding-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn old_profile_without_onboarding_has_explicit_unaccepted_default() {
    let settings = AppSettings::initial_profile();
    assert!(!settings.onboarding.eula_accepted);
    let mut raw = serde_json::to_value(&settings).unwrap();
    raw.as_object_mut().unwrap().remove("onboarding");
    let old: AppSettings = serde_json::from_value(raw).unwrap();
    assert!(!old.onboarding.eula_accepted);
}

#[test]
fn consent_persists_without_changing_unrelated_user_preferences() {
    let path = scratch();
    let repository = JsonSettingsRepository::new(path.join("settings.json"));
    let mut settings = AppSettings::initial_profile();
    settings.voice.voice = "custom-voice".into();
    settings.onboarding.eula_accepted = true;
    repository
        .save(&StoredSettings {
            revision: 3,
            settings,
        })
        .unwrap();
    let restored = repository.load().unwrap();
    assert!(restored.settings.onboarding.eula_accepted);
    assert_eq!(restored.settings.voice.voice, "custom-voice");
    fs::remove_dir_all(path).unwrap();
}
