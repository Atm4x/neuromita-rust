pub mod guide;
pub mod wiki;

use std::{io, path::Path};

use crate::settings::{AppSettings, JsonSettingsRepository, SettingsRepository, StoredSettings};

const RU: &str = include_str!("../assets/onboarding/eula_ru.txt");
const EN: &str = include_str!("../assets/onboarding/eula_en.txt");

pub fn eula_text(code: &str) -> &'static str {
    match code.to_ascii_lowercase().as_str() {
        "ru" => RU,
        _ => EN,
    }
}

pub fn pending(data_dir: &Path) -> Result<bool, io::Error> {
    let path = data_dir.join("settings.json");
    if !path.exists() {
        return Ok(true);
    }
    let stored = JsonSettingsRepository::new(path)
        .load()
        .map_err(io::Error::other)?;
    Ok(!stored.settings.onboarding.eula_accepted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupState {
    pub requires_consent: bool,
    pub requires_guide: bool,
    pub guide_level: guide::GuideLevel,
    pub guide_page: u8,
}

pub fn startup_state(data_dir: &Path) -> Result<StartupState, io::Error> {
    let path = data_dir.join("settings.json");
    let onboarding = if path.exists() {
        JsonSettingsRepository::new(path)
            .load()
            .map_err(io::Error::other)?
            .settings
            .onboarding
    } else {
        AppSettings::initial_profile().onboarding
    };
    Ok(StartupState {
        requires_consent: !onboarding.eula_accepted,
        requires_guide: onboarding.eula_accepted && !onboarding.guide_completed,
        guide_level: onboarding.guide_level,
        guide_page: onboarding
            .guide_page
            .min(guide::count(onboarding.guide_level).saturating_sub(1) as u8),
    })
}

pub fn guide_reopen_position(data_dir: &Path) -> Result<(guide::GuideLevel, u8), io::Error> {
    let state = startup_state(data_dir)?;
    if state.requires_consent {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Guide cannot open before EULA acceptance",
        ));
    }
    Ok((
        state.guide_level,
        if state.requires_guide {
            state.guide_page
        } else {
            0
        },
    ))
}

pub fn selected_guide_level(data_dir: &Path) -> Result<guide::GuideLevel, io::Error> {
    let path = data_dir.join("settings.json");
    if !path.exists() {
        return Ok(guide::GuideLevel::default());
    }
    let stored = JsonSettingsRepository::new(path)
        .load()
        .map_err(io::Error::other)?;
    Ok(stored.settings.onboarding.guide_level)
}

pub fn accept(data_dir: &Path, language: &str) -> Result<(), io::Error> {
    use crate::settings::Language;

    let selected = match language.to_ascii_lowercase().as_str() {
        "ru" => Language::Russian,
        "en" => Language::English,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported EULA language",
            ));
        }
    };
    let path = data_dir.join("settings.json");
    let repo = JsonSettingsRepository::new(path.clone());
    let mut stored = if path.exists() {
        repo.load().map_err(io::Error::other)?
    } else {
        StoredSettings {
            revision: 0,
            settings: AppSettings::initial_profile(),
        }
    };
    if stored.settings.onboarding.eula_accepted {
        return Ok(());
    }
    stored.revision = stored
        .revision
        .checked_add(1)
        .ok_or_else(|| io::Error::other("settings revision exhausted"))?;
    stored.settings.interface.language = selected;
    stored.settings.onboarding.eula_accepted = true;
    stored.settings.onboarding.guide_completed = false;
    stored.settings.onboarding.guide_page = 0;
    repo.save(&stored).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Language;

    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("neuromita-consent-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fresh_install_is_pending_without_writing_a_file() {
        let dir = Directory::new();
        assert!(pending(&dir.0).unwrap());
        assert!(!dir.0.join("settings.json").exists());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    #[test]
    fn accepting_english_persists_across_restart_and_preserves_existing_values() {
        let dir = Directory::new();
        let repo = JsonSettingsRepository::new(dir.0.join("settings.json"));
        let mut stored = StoredSettings {
            revision: 13,
            settings: AppSettings::initial_profile(),
        };
        stored.settings.voice.voice = "existing-custom-voice".into();
        repo.save(&stored).unwrap();
        assert!(pending(&dir.0).unwrap());
        accept(&dir.0, "en").unwrap();
        assert!(!pending(&dir.0).unwrap());
        let accepted = repo.load().unwrap();
        assert_eq!(accepted.revision, 14);
        assert_eq!(accepted.settings.interface.language, Language::English);
        assert_eq!(accepted.settings.voice.voice, "existing-custom-voice");
        accept(&dir.0, "ru").unwrap();
        assert_eq!(repo.load().unwrap(), accepted);
    }

    #[test]
    fn guide_level_survives_restart_and_eula_acceptance() {
        let dir = Directory::new();
        assert_eq!(
            selected_guide_level(&dir.0).unwrap(),
            guide::GuideLevel::Basic
        );
        assert!(!dir.0.join("settings.json").exists());
        let repo = JsonSettingsRepository::new(dir.0.join("settings.json"));
        let mut stored = StoredSettings {
            revision: 4,
            settings: AppSettings::initial_profile(),
        };
        stored.settings.onboarding.guide_level = guide::GuideLevel::Full;
        repo.save(&stored).unwrap();
        assert!(pending(&dir.0).unwrap());
        assert_eq!(
            selected_guide_level(&dir.0).unwrap(),
            guide::GuideLevel::Full
        );
        accept(&dir.0, "ru").unwrap();
        assert_eq!(repo.load().unwrap().revision, 5);
        assert_eq!(
            selected_guide_level(&dir.0).unwrap(),
            guide::GuideLevel::Full
        );
        assert!(!pending(&dir.0).unwrap());
    }

    #[test]
    fn unsupported_language_or_corrupt_storage_never_grants_consent() {
        let dir = Directory::new();
        assert!(accept(&dir.0, "de").is_err());
        assert!(!dir.0.join("settings.json").exists());
        std::fs::write(dir.0.join("settings.json"), b"{bad json").unwrap();
        let before = std::fs::read(dir.0.join("settings.json")).unwrap();
        assert!(accept(&dir.0, "en").is_err());
        assert!(pending(&dir.0).is_err());
        assert_eq!(std::fs::read(dir.0.join("settings.json")).unwrap(), before);
    }

    #[test]
    fn fresh_acceptance_creates_pending_guide_and_restart_resumes_page() {
        let dir = Directory::new();
        assert_eq!(
            startup_state(&dir.0).unwrap(),
            StartupState {
                requires_consent: true,
                requires_guide: false,
                guide_level: guide::GuideLevel::Basic,
                guide_page: 0,
            }
        );
        accept(&dir.0, "ru").unwrap();
        let starting = startup_state(&dir.0).unwrap();
        assert!(!starting.requires_consent);
        assert!(starting.requires_guide);
        let repo = JsonSettingsRepository::new(dir.0.join("settings.json"));
        let mut stored = repo.load().unwrap();
        stored.settings.onboarding.guide_level = guide::GuideLevel::Full;
        stored.settings.onboarding.guide_page = 7;
        repo.save(&stored).unwrap();
        let resumed = startup_state(&dir.0).unwrap();
        assert!(resumed.requires_guide);
        assert_eq!(resumed.guide_level, guide::GuideLevel::Full);
        assert_eq!(resumed.guide_page, 7);
        stored.settings.onboarding.guide_completed = true;
        repo.save(&stored).unwrap();
        let finished = startup_state(&dir.0).unwrap();
        assert!(!finished.requires_guide);
        assert!(!finished.requires_consent);
        accept(&dir.0, "en").unwrap();
        assert_eq!(startup_state(&dir.0).unwrap(), finished);
    }

    #[test]
    fn manual_reopen_resumes_incomplete_guide_but_starts_completed_help_at_first_page() {
        let dir = Directory::new();
        let path = dir.0.join("settings.json");
        assert_eq!(
            guide_reopen_position(&dir.0).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!path.exists());
        accept(&dir.0, "en").unwrap();
        let repo = JsonSettingsRepository::new(path);
        let mut stored = repo.load().unwrap();
        stored.settings.onboarding.guide_level = guide::GuideLevel::Full;
        stored.settings.onboarding.guide_page = 8;
        repo.save(&stored).unwrap();
        assert_eq!(
            guide_reopen_position(&dir.0).unwrap(),
            (guide::GuideLevel::Full, 8)
        );
        stored.settings.onboarding.guide_completed = true;
        repo.save(&stored).unwrap();
        assert_eq!(
            guide_reopen_position(&dir.0).unwrap(),
            (guide::GuideLevel::Full, 0)
        );
        assert_eq!(repo.load().unwrap(), stored);
    }

    #[test]
    fn legacy_accepted_profile_without_guide_progress_does_not_reopen_guide() {
        let dir = Directory::new();
        let mut old = serde_json::to_value(AppSettings::initial_profile()).unwrap();
        old["onboarding"]["eula_accepted"] = serde_json::json!(true);
        old["onboarding"]
            .as_object_mut()
            .unwrap()
            .remove("guide_completed");
        old["onboarding"]
            .as_object_mut()
            .unwrap()
            .remove("guide_page");
        let old: AppSettings = serde_json::from_value(old).unwrap();
        assert!(old.onboarding.guide_completed);
        let repo = JsonSettingsRepository::new(dir.0.join("settings.json"));
        repo.save(&StoredSettings {
            revision: 3,
            settings: old,
        })
        .unwrap();
        let result = startup_state(&dir.0).unwrap();
        assert!(!result.requires_consent);
        assert!(!result.requires_guide);
    }

    #[test]
    fn startup_failure_on_corrupted_profile_does_not_create_replacement() {
        let dir = Directory::new();
        std::fs::write(dir.0.join("settings.json"), b"{truncated").unwrap();
        let before = std::fs::read(dir.0.join("settings.json")).unwrap();
        assert!(startup_state(&dir.0).is_err());
        assert_eq!(std::fs::read(dir.0.join("settings.json")).unwrap(), before);
    }

    #[test]
    fn unknown_locales_use_english_eula_text_without_accepting_implicitly() {
        assert!(eula_text("ru").contains("ЛИЦЕНЗИОННОЕ СОГЛАШЕНИЕ"));
        assert!(eula_text("en").contains("END USER LICENSE AGREEMENT"));
        assert_eq!(eula_text("de"), eula_text("en"));
    }
}
