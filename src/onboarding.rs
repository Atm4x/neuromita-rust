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
    fn unknown_locales_use_english_eula_text_without_accepting_implicitly() {
        assert!(eula_text("ru").contains("ЛИЦЕНЗИОННОЕ СОГЛАШЕНИЕ"));
        assert!(eula_text("en").contains("END USER LICENSE AGREEMENT"));
        assert_eq!(eula_text("de"), eula_text("en"));
    }
}
