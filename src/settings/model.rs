use serde::{Deserialize, Serialize};

use super::SettingsError;
use crate::generation::PresetId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Volume(u8);

impl Volume {
    pub fn new(value: u8) -> Result<Self, SettingsError> {
        if value > 100 {
            return Err(SettingsError::Validation(
                "volume must be between 0 and 100".into(),
            ));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u8 {
        self.0
    }
}
impl TryFrom<u8> for Volume {
    type Error = SettingsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<Volume> for u8 {
    fn from(value: Volume) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceSettings {
    pub volume: Volume,
    pub in_launcher: bool,
    pub in_game: bool,
    pub voice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSettings {
    pub enabled: bool,
    pub interval_ms: u32,
    pub jpeg_quality: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Russian,
    English,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceSettings {
    pub language: Language,
    pub font_size: u16,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetSelection {
    #[default]
    Active,
    Specific(PresetId),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationPresets {
    pub reply: PresetSelection,
    pub react: PresetSelection,
    pub initiate: PresetSelection,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterSettings {
    pub presets: OperationPresets,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharactersSettings {
    pub mita: CharacterSettings,
    pub kind_mita: CharacterSettings,
}

impl CharactersSettings {
    pub fn for_character(&self, id: crate::character::CharacterId) -> &CharacterSettings {
        match id {
            crate::character::CharacterId::Mita => &self.mita,
            crate::character::CharacterId::KindMita => &self.kind_mita,
        }
    }

    pub fn for_character_mut(
        &mut self,
        id: crate::character::CharacterId,
    ) -> &mut CharacterSettings {
        match id {
            crate::character::CharacterId::Mita => &mut self.mita,
            crate::character::CharacterId::KindMita => &mut self.kind_mita,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetSettings {
    pub active: PresetId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub presets: PresetSettings,
    pub characters: CharactersSettings,
    pub voice: VoiceSettings,
    pub capture: CaptureSettings,
    pub interface: InterfaceSettings,
}

impl AppSettings {
    pub fn initial_profile() -> Self {
        Self {
            presets: PresetSettings::default(),
            characters: CharactersSettings::default(),
            voice: VoiceSettings {
                volume: Volume(80),
                in_launcher: false,
                in_game: false,
                voice: "default".into(),
            },
            capture: CaptureSettings {
                enabled: false,
                interval_ms: 1000,
                jpeg_quality: 80,
            },
            interface: InterfaceSettings {
                language: Language::Russian,
                font_size: 14,
            },
        }
    }
    pub fn selected_preset(
        &self,
        character: crate::character::CharacterId,
        intent: crate::interaction::TurnIntent,
    ) -> PresetId {
        let assigned = &self.characters.for_character(character).presets;
        let selection = match intent {
            crate::interaction::TurnIntent::Reply(_) => assigned.reply,
            crate::interaction::TurnIntent::React(_) => assigned.react,
            crate::interaction::TurnIntent::Initiate => assigned.initiate,
        };
        match selection {
            PresetSelection::Active => self.presets.active,
            PresetSelection::Specific(id) => id,
        }
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.voice.voice.trim().is_empty() {
            return Err(SettingsError::Validation(
                "voice identifier must not be empty".into(),
            ));
        }
        if self.capture.interval_ms == 0 || !(1..=100).contains(&self.capture.jpeg_quality) {
            return Err(SettingsError::Validation(
                "invalid capture interval or JPEG quality".into(),
            ));
        }
        if !(6..=96).contains(&self.interface.font_size) {
            return Err(SettingsError::Validation(
                "font size must be between 6 and 96".into(),
            ));
        }
        Ok(())
    }
}

mod sealed {
    pub trait Sealed {}
}
pub trait SettingsSection: sealed::Sealed + Clone + PartialEq + Send + Sync + 'static {
    fn read(settings: &AppSettings) -> Self;
    fn replace(self, settings: &mut AppSettings);
}

macro_rules! section {
    ($ty:ty, $field:ident) => {
        impl sealed::Sealed for $ty {}
        impl SettingsSection for $ty {
            fn read(settings: &AppSettings) -> Self {
                settings.$field.clone()
            }
            fn replace(self, settings: &mut AppSettings) {
                settings.$field = self;
            }
        }
    };
}
section!(PresetSettings, presets);
section!(CharactersSettings, characters);
section!(VoiceSettings, voice);
section!(CaptureSettings, capture);
section!(InterfaceSettings, interface);

#[derive(Debug, Clone)]
pub struct SectionSnapshot<T> {
    pub revision: u64,
    pub value: T,
}
