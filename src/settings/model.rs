use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, PathBuf},
};

use super::{SettingsError, SpeechSettings};
use crate::generation::{ModelMemoryAddPolicy, PresetId, ToolSettings};
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

/// Model-proposed facts only; manual edits use their explicit MemoryCommand.
/// Disabled by default to preserve existing Rust memory-write behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    pub model_add_dedup_enabled: bool,
    pub threshold_per_mille: u16,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            model_add_dedup_enabled: false,
            threshold_per_mille: 800,
        }
    }
}

impl MemorySettings {
    pub fn validate(self) -> Result<(), SettingsError> {
        if !(500..=1000).contains(&self.threshold_per_mille) {
            return Err(SettingsError::Validation(
                "model memory dedup threshold must be 500..1000 per mille".into(),
            ));
        }
        Ok(())
    }

    pub fn model_add_policy(self) -> ModelMemoryAddPolicy {
        if self.model_add_dedup_enabled {
            ModelMemoryAddPolicy::Lexical {
                threshold_per_mille: self.threshold_per_mille,
            }
        } else {
            ModelMemoryAddPolicy::Ordinary
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryGenerationMode {
    #[default]
    Off,
    Keyword,
    Fts,
    Vector,
    Fusion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryGenerationSettings {
    pub mode: HistoryGenerationMode,
    pub max_results: u8,
    pub max_content_chars: u16,
    /// Recent conversational messages retained verbatim when History RAG is enabled.
    pub active_messages: u8,
}

impl Default for HistoryGenerationSettings {
    fn default() -> Self {
        Self {
            mode: HistoryGenerationMode::Off,
            max_results: 4,
            max_content_chars: 600,
            active_messages: 8,
        }
    }
}

impl HistoryGenerationSettings {
    pub fn validate(self) -> Result<(), SettingsError> {
        if !(1..=8).contains(&self.max_results)
            || !(64..=1200).contains(&self.max_content_chars)
            || !(1..=64).contains(&self.active_messages)
        {
            return Err(SettingsError::Validation(
                "history generation requires 1..=8 results, 64..=1200 characters and 1..=64 active messages".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentGenerationSource {
    pub root: PathBuf,
    pub index: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DocumentGenerationSettings {
    pub sources: BTreeMap<crate::identity::CharacterId, DocumentGenerationSource>,
    pub max_results: u8,
    pub max_content_chars: u16,
}

impl Default for DocumentGenerationSettings {
    fn default() -> Self {
        Self {
            sources: BTreeMap::new(),
            max_results: 3,
            max_content_chars: 500,
        }
    }
}

impl DocumentGenerationSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if !(1..=4).contains(&self.max_results) || !(64..=800).contains(&self.max_content_chars) {
            return Err(SettingsError::Validation(
                "document generation requires 1..=4 results and 64..=800 characters per result"
                    .into(),
            ));
        }
        for source in self.sources.values() {
            if !source.root.is_absolute()
                || !source.index.is_absolute()
                || source.index.starts_with(&source.root)
                || source.index == source.root
                || [source.root.as_path(), source.index.as_path()]
                    .iter()
                    .any(|path| {
                        path.components().any(|component| {
                            matches!(component, Component::ParentDir | Component::CurDir)
                        })
                    })
            {
                return Err(SettingsError::Validation(
                    "document generation requires absolute canonical-form source and independent index paths".into(),
                ));
            }
        }
        Ok(())
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub presets: PresetSettings,
    pub characters: CharactersSettings,
    pub voice: VoiceSettings,
    pub capture: CaptureSettings,
    pub interface: InterfaceSettings,
    pub reminders: RemindersSettings,
    pub speech: SpeechSettings,
    #[serde(default)]
    pub memory: MemorySettings,
    #[serde(default)]
    pub history_generation: HistoryGenerationSettings,
    #[serde(default)]
    pub document_generation: DocumentGenerationSettings,
    #[serde(default)]
    pub tools: ToolSettings,
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
                visibility: SettingsVisibility::all_visible(),
                hide_private: true,
                structured_display: StructuredDisplayMode::Off,
            },
            reminders: RemindersSettings::initial_profile(),
            speech: SpeechSettings::default(),
            memory: MemorySettings::default(),
            history_generation: HistoryGenerationSettings::default(),
            document_generation: DocumentGenerationSettings::default(),
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
        self.memory.validate()?;
        self.history_generation.validate()?;
        self.document_generation.validate()?;
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
section!(CharactersSettings, characters);
section!(VoiceSettings, voice);
section!(SpeechSettings, speech);
section!(MemorySettings, memory);
section!(HistoryGenerationSettings, history_generation);
section!(DocumentGenerationSettings, document_generation);
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
