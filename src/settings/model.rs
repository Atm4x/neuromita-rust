use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{SettingsError, SpeechSettings};
use crate::generation::{PresetId, ToolSettings};
use crate::reminders::{LocalTimePolicy, TimeZoneId};

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredDisplayMode {
    #[default]
    Off,
    Brief,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceSettings {
    pub language: Language,
    pub font_size: u16,
    pub visibility: SettingsVisibility,
    pub hide_private: bool,
    #[serde(default)]
    pub structured_display: StructuredDisplayMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsArea {
    Api,
    Characters,
    Voice,
    Language,
    Asr,
    AiEngine,
    Games,
    Models,
    Images,
    Updates,
    Datasets,
}

impl SettingsArea {
    pub const ALL: [Self; 11] = [
        Self::Api,
        Self::Characters,
        Self::Voice,
        Self::Language,
        Self::Asr,
        Self::AiEngine,
        Self::Games,
        Self::Models,
        Self::Images,
        Self::Updates,
        Self::Datasets,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "BTreeMap<SettingsArea, bool>",
    into = "BTreeMap<SettingsArea, bool>"
)]
pub struct SettingsVisibility(BTreeMap<SettingsArea, bool>);

impl SettingsVisibility {
    pub fn all_visible() -> Self {
        Self(
            SettingsArea::ALL
                .into_iter()
                .map(|area| (area, true))
                .collect(),
        )
    }
    pub fn is_visible(&self, area: SettingsArea) -> bool {
        self.0[&area]
    }
    pub fn set_visible(&mut self, area: SettingsArea, visible: bool) {
        self.0.insert(area, visible);
    }
}

impl TryFrom<BTreeMap<SettingsArea, bool>> for SettingsVisibility {
    type Error = String;
    fn try_from(values: BTreeMap<SettingsArea, bool>) -> Result<Self, Self::Error> {
        if values.len() != SettingsArea::ALL.len()
            || SettingsArea::ALL
                .iter()
                .any(|area| !values.contains_key(area))
        {
            return Err("visibility must explicitly contain every settings area".into());
        }
        Ok(Self(values))
    }
}

impl From<SettingsVisibility> for BTreeMap<SettingsArea, bool> {
    fn from(value: SettingsVisibility) -> Self {
        value.0
    }
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
    #[serde(default)]
    pub prompt_set: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharactersSettings {
    #[serde(rename = "Crazy")]
    pub crazy: CharacterSettings,
    #[serde(rename = "Kind")]
    pub kind: CharacterSettings,
    #[serde(rename = "Cappie")]
    pub cappie: CharacterSettings,
    #[serde(rename = "ShortHair")]
    pub short_hair: CharacterSettings,
    #[serde(rename = "Mila")]
    pub mila: CharacterSettings,
    #[serde(rename = "Sleepy")]
    pub sleepy: CharacterSettings,
    #[serde(rename = "Ghost")]
    pub ghost: CharacterSettings,
    #[serde(rename = "Creepy")]
    pub creepy: CharacterSettings,
    #[serde(rename = "GameMaster")]
    pub game_master: CharacterSettings,
}

impl CharactersSettings {
    pub fn for_character(&self, id: crate::character::CharacterId) -> &CharacterSettings {
        match id {
            crate::character::CharacterId::Crazy => &self.crazy,
            crate::character::CharacterId::Kind => &self.kind,
            crate::character::CharacterId::Cappie => &self.cappie,
            crate::character::CharacterId::ShortHair => &self.short_hair,
            crate::character::CharacterId::Mila => &self.mila,
            crate::character::CharacterId::Sleepy => &self.sleepy,
            crate::character::CharacterId::Ghost => &self.ghost,
            crate::character::CharacterId::Creepy => &self.creepy,
            crate::character::CharacterId::GameMaster => &self.game_master,
        }
    }

    pub fn for_character_mut(
        &mut self,
        id: crate::character::CharacterId,
    ) -> &mut CharacterSettings {
        match id {
            crate::character::CharacterId::Crazy => &mut self.crazy,
            crate::character::CharacterId::Kind => &mut self.kind,
            crate::character::CharacterId::Cappie => &mut self.cappie,
            crate::character::CharacterId::ShortHair => &mut self.short_hair,
            crate::character::CharacterId::Mila => &mut self.mila,
            crate::character::CharacterId::Sleepy => &mut self.sleepy,
            crate::character::CharacterId::Ghost => &mut self.ghost,
            crate::character::CharacterId::Creepy => &mut self.creepy,
            crate::character::CharacterId::GameMaster => &mut self.game_master,
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
pub struct RemindersSettings {
    pub enabled: bool,
    pub timezone: TimeZoneId,
    pub policy: LocalTimePolicy,
}

impl RemindersSettings {
    pub fn initial_profile() -> Self {
        // Deterministic declared profile: the initial zone is always UTC and
        // never depends on the host. A local zone is set explicitly through
        // SettingsApi or the `zone` terminal command.
        Self {
            enabled: true,
            timezone: TimeZoneId::utc(),
            policy: LocalTimePolicy::LENIENT,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuideLevel {
    #[default]
    Basic,
    Advanced,
    Full,
}

impl GuideLevel {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Advanced,
            2 => Self::Full,
            _ => Self::Basic,
        }
    }

    pub const fn index(self) -> i32 {
        match self {
            Self::Basic => 0,
            Self::Advanced => 1,
            Self::Full => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingSettings {
    pub eula_accepted: bool,
    #[serde(default)]
    pub guide_level: GuideLevel,
    #[serde(default = "legacy_guide_completed")]
    pub guide_completed: bool,
    #[serde(default)]
    pub guide_page: u8,
}

const fn legacy_guide_completed() -> bool {
    true
}

impl Default for OnboardingSettings {
    fn default() -> Self {
        Self {
            eula_accepted: false,
            guide_level: GuideLevel::default(),
            guide_completed: false,
            guide_page: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub presets: PresetSettings,
    #[serde(default)]
    pub onboarding: OnboardingSettings,
    pub characters: CharactersSettings,
    pub voice: VoiceSettings,
    pub capture: CaptureSettings,
    pub interface: InterfaceSettings,
    pub reminders: RemindersSettings,
    pub speech: SpeechSettings,
    #[serde(default)]
    pub tools: ToolSettings,
}

impl AppSettings {
    pub fn initial_profile() -> Self {
        Self {
            presets: PresetSettings::default(),
            onboarding: OnboardingSettings::default(),
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
                visibility: SettingsVisibility::all_visible(),
                hide_private: true,
                structured_display: StructuredDisplayMode::Off,
            },
            reminders: RemindersSettings::initial_profile(),
            speech: SpeechSettings::default(),
            tools: ToolSettings::default(),
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
        for character in crate::character::CharacterId::ALL {
            if let Some(name) = &self.characters.for_character(character).prompt_set {
                crate::prompts::PromptCatalog::validate_set_name(name)
                    .map_err(|error| SettingsError::Validation(error.to_string()))?;
            }
        }
        self.speech.validate()?;
        self.tools
            .validate()
            .map_err(|error| SettingsError::Validation(error.to_string()))?;
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
section!(OnboardingSettings, onboarding);
section!(CharactersSettings, characters);
section!(VoiceSettings, voice);
section!(SpeechSettings, speech);
section!(CaptureSettings, capture);
section!(InterfaceSettings, interface);
section!(RemindersSettings, reminders);
section!(ToolSettings, tools);

#[derive(Debug, Clone)]
pub struct SectionSnapshot<T> {
    pub revision: u64,
    pub value: T,
}

#[cfg(test)]
mod structured_display_tests {
    use super::*;

    #[test]
    fn historical_onboarding_without_guide_level_defaults_to_basic() {
        let mut value = serde_json::to_value(AppSettings::initial_profile()).unwrap();
        value["onboarding"]
            .as_object_mut()
            .unwrap()
            .remove("guide_level");
        let decoded: AppSettings = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.onboarding.guide_level, GuideLevel::Basic);
        for level in [GuideLevel::Basic, GuideLevel::Advanced, GuideLevel::Full] {
            let mut saved = decoded.clone();
            saved.onboarding.guide_level = level;
            let serialized = serde_json::to_value(&saved).unwrap();
            let restored: AppSettings = serde_json::from_value(serialized).unwrap();
            assert_eq!(restored.onboarding.guide_level, level);
        }
    }

    #[test]
    fn guide_progress_is_backward_compatible_but_fresh_profiles_start_unfinished() {
        let fresh = AppSettings::initial_profile();
        assert!(!fresh.onboarding.guide_completed);
        assert_eq!(fresh.onboarding.guide_page, 0);
        let mut legacy = serde_json::to_value(&fresh).unwrap();
        legacy["onboarding"]["eula_accepted"] = serde_json::json!(true);
        legacy["onboarding"]
            .as_object_mut()
            .unwrap()
            .remove("guide_completed");
        legacy["onboarding"]
            .as_object_mut()
            .unwrap()
            .remove("guide_page");
        let legacy: AppSettings = serde_json::from_value(legacy).unwrap();
        assert!(legacy.onboarding.eula_accepted);
        assert!(legacy.onboarding.guide_completed);
        let mut new = fresh;
        new.onboarding.guide_completed = true;
        new.onboarding.guide_page = 12;
        let roundtrip: AppSettings =
            serde_json::from_value(serde_json::to_value(&new).unwrap()).unwrap();
        assert_eq!(new, roundtrip);
    }

    #[test]
    fn invalid_persisted_guide_level_is_rejected() {
        let mut value = serde_json::to_value(AppSettings::initial_profile()).unwrap();
        value["onboarding"]["guide_level"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<AppSettings>(value).is_err());
    }

    #[test]
    fn historical_settings_without_structured_display_default_to_off() {
        let mut value = serde_json::to_value(AppSettings::initial_profile()).unwrap();
        value["interface"]
            .as_object_mut()
            .unwrap()
            .remove("structured_display");
        let decoded: AppSettings = serde_json::from_value(value).unwrap();
        assert_eq!(
            decoded.interface.structured_display,
            StructuredDisplayMode::Off
        );
        for mode in [StructuredDisplayMode::Brief, StructuredDisplayMode::Json] {
            let mut updated = decoded.clone();
            updated.interface.structured_display = mode;
            let again: AppSettings =
                serde_json::from_value(serde_json::to_value(updated).unwrap()).unwrap();
            assert_eq!(again.interface.structured_display, mode);
        }
    }
}
