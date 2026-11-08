use std::{collections::BTreeMap, sync::Arc};

use tokio::sync::watch;

use crate::{
    character::CharacterId,
    settings::{
        AppSettings, AsrPreferences, CharacterSettings, CharactersSettings, InterfaceSettings,
        OperationPresets,
        PresetId, PresetSelection, PresetSettings, SettingsArea, SettingsError, SettingsOwner,
        SettingsSnapshot, SettingsTransfer, SpeechSettings, TtsPreferences, VoiceSettings, Volume,
    },
};

type PropertyReader<T> = Arc<dyn Fn(&AppSettings) -> T + Send + Sync>;
type PropertyWriter<T> = Arc<dyn Fn(&SettingsOwner, T) -> Result<(), SettingsError> + Send + Sync>;

pub struct BoundProperty<T> {
    owner: Arc<SettingsOwner>,
    source: watch::Receiver<Arc<SettingsSnapshot>>,
    project: PropertyReader<T>,
    write: PropertyWriter<T>,
}

impl<T: Clone> Clone for BoundProperty<T> {
    fn clone(&self) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
            source: self.source.clone(),
            project: Arc::clone(&self.project),
            write: Arc::clone(&self.write),
        }
    }
}

impl<T: Clone + PartialEq> BoundProperty<T> {
    fn new(
        owner: Arc<SettingsOwner>,
        project: impl Fn(&AppSettings) -> T + Send + Sync + 'static,
        write: impl Fn(&SettingsOwner, T) -> Result<(), SettingsError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            source: owner.subscribe(),
            owner,
            project: Arc::new(project),
            write: Arc::new(write),
        }
    }
    pub fn get(&self) -> T {
        (self.project)(&self.source.borrow().values)
    }
    pub fn set(&self, value: T) -> Result<(), SettingsError> {
        (self.write)(&self.owner, value)
    }
    pub fn observe(&self) -> PropertyObserver<T> {
        PropertyObserver {
            source: self.source.clone(),
            project: Arc::clone(&self.project),
            previous: None,
        }
    }
}

pub struct PropertyObserver<T> {
    source: watch::Receiver<Arc<SettingsSnapshot>>,
    project: PropertyReader<T>,
    previous: Option<T>,
}

impl<T: Clone + PartialEq> PropertyObserver<T> {
    pub async fn next(&mut self) -> Result<T, SettingsError> {
        loop {
            let value = {
                let snapshot = self.source.borrow_and_update();
                if snapshot.closed {
                    return Err(SettingsError::Stopped);
                }
                (self.project)(&snapshot.values)
            };
            if self.previous.as_ref() != Some(&value) {
                self.previous = Some(value.clone());
                return Ok(value);
            }
            self.source
                .changed()
                .await
                .map_err(|_| SettingsError::Stopped)?;
        }
    }
}

pub struct VoiceSettingsPresenter {
    pub volume: BoundProperty<Volume>,
    pub in_launcher: BoundProperty<bool>,
    pub in_game: BoundProperty<bool>,
    pub voice: BoundProperty<String>,
}

pub struct TtsSettingsPresenter {
    pub preferences: BoundProperty<TtsPreferences>,
    pub model_options: BoundProperty<BTreeMap<String, serde_json::Value>>,
}

pub struct AsrSettingsPresenter {
    pub preferences: BoundProperty<AsrPreferences>,
    pub model_options: BoundProperty<BTreeMap<String, serde_json::Value>>,
}

pub struct PresetsSettingsPresenter {
    pub active: BoundProperty<PresetId>,
}

#[derive(Debug, Clone, Copy)]
pub enum PresetSlot {
    Reply,
    React,
    Initiate,
}

pub struct CharacterSettingsPresenter {
    pub prompt_set: BoundProperty<Option<String>>,
    pub assignments: BoundProperty<OperationPresets>,
    pub reply: BoundProperty<PresetSelection>,
    pub react: BoundProperty<PresetSelection>,
    pub initiate: BoundProperty<PresetSelection>,
}

impl CharacterSettingsPresenter {
    fn new(owner: Arc<SettingsOwner>, character: CharacterId) -> Self {
        Self {
            prompt_set: BoundProperty::new(
                Arc::clone(&owner),
                move |settings| {
                    settings
                        .characters
                        .for_character(character)
                        .prompt_set
                        .clone()
                },
                move |owner, value| {
                    owner
                        .edit::<CharactersSettings>(|settings| {
                            settings.for_character_mut(character).prompt_set = value;
                        })
                        .map(|_| ())
                },
            ),
            assignments: BoundProperty::new(
                Arc::clone(&owner),
                move |settings| settings.characters.for_character(character).presets,
                move |owner, value| {
                    owner
                        .edit::<CharactersSettings>(|settings| {
                            settings.for_character_mut(character).presets = value
                        })
                        .map(|_| ())
                },
            ),
            reply: BoundProperty::new(
                Arc::clone(&owner),
                move |settings| settings.characters.for_character(character).presets.reply,
                move |owner, value| {
                    owner
                        .edit::<CharactersSettings>(|settings| {
                            settings.for_character_mut(character).presets.reply = value;
                        })
                        .map(|_| ())
                },
            ),
            react: BoundProperty::new(
                Arc::clone(&owner),
                move |settings| settings.characters.for_character(character).presets.react,
                move |owner, value| {
                    owner
                        .edit::<CharactersSettings>(|settings| {
                            settings.for_character_mut(character).presets.react = value;
                        })
                        .map(|_| ())
                },
            ),
            initiate: BoundProperty::new(
                owner,
                move |settings| {
                    settings
                        .characters
                        .for_character(character)
                        .presets
                        .initiate
                },
                move |owner, value| {
                    owner
                        .edit::<CharactersSettings>(|settings| {
                            settings.for_character_mut(character).presets.initiate = value;
                        })
                        .map(|_| ())
                },
            ),
        }
    }
}

/// A draft based on one authoritative Settings Owner revision and one stable identity.
/// It is not a second profile store, and a successful edit is not a disk-save receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterProfileSnapshot {
    character: CharacterId,
    revision: u64,
    settings: CharacterSettings,
}

impl CharacterProfileSnapshot {
    pub fn character(&self) -> CharacterId {
        self.character
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn settings(&self) -> &CharacterSettings {
        &self.settings
    }
}

pub struct CharactersSettingsPresenter {
    owner: Arc<SettingsOwner>,
    characters: BTreeMap<CharacterId, CharacterSettingsPresenter>,
}

impl CharactersSettingsPresenter {
    fn new(owner: Arc<SettingsOwner>) -> Self {
        Self {
            characters: CharacterId::ALL
                .into_iter()
                .map(|id| (id, CharacterSettingsPresenter::new(Arc::clone(&owner), id)))
                .collect(),
            owner,
        }
    }

    pub fn for_character(&self, character: CharacterId) -> &CharacterSettingsPresenter {
        &self.characters[&character]
    }

    /// Capture an editable per-character profile from the canonical settings snapshot.
    pub fn profile(&self, character: CharacterId) -> Result<CharacterProfileSnapshot, SettingsError> {
        let snapshot = self.owner.snapshot::<CharactersSettings>()?;
        Ok(CharacterProfileSnapshot {
            character,
            revision: snapshot.revision,
            settings: snapshot.value.for_character(character).clone(),
        })
    }

    /// Commit all supported profile fields together, or none of them.
    ///
    /// A concurrent edit to any settings area yields Conflict instead of
    /// overwriting it. Storage acknowledgement is observed separately through
    /// SettingsPresenter::observe / PersistenceState.
    pub fn save_profile(
        &self,
        expected: &CharacterProfileSnapshot,
        edited: CharacterSettings,
    ) -> Result<CharacterProfileSnapshot, SettingsError> {
        let snapshot = self.owner.snapshot::<CharactersSettings>()?;
        if snapshot.revision != expected.revision {
            return Err(SettingsError::Conflict {
                expected: expected.revision,
                actual: snapshot.revision,
            });
        }
        let mut values = snapshot.value;
        *values.for_character_mut(expected.character) = edited;
        let saved = self.owner.replace::<CharactersSettings>(expected.revision, values)?;
        Ok(CharacterProfileSnapshot {
            character: expected.character,
            revision: saved.revision,
            settings: saved.value.for_character(expected.character).clone(),
        })
    }
}

pub struct InterfaceSettingsPresenter {
    owner: Arc<SettingsOwner>,
    pub hide_private: BoundProperty<bool>,
}

impl InterfaceSettingsPresenter {
    pub fn set_visible(&self, area: SettingsArea, visible: bool) -> Result<(), SettingsError> {
        self.owner
            .edit::<InterfaceSettings>(|settings| settings.visibility.set_visible(area, visible))
            .map(|_| ())
    }
}

pub struct SettingsPresenter {
    owner: Arc<SettingsOwner>,
    pub interface: InterfaceSettingsPresenter,
    pub voice: VoiceSettingsPresenter,
    pub tts: TtsSettingsPresenter,
    pub asr: AsrSettingsPresenter,
    pub presets: PresetsSettingsPresenter,
    pub characters: CharactersSettingsPresenter,
}
impl SettingsPresenter {
    pub(crate) fn new(owner: Arc<SettingsOwner>) -> Self {
        Self {
            owner: Arc::clone(&owner),
            interface: InterfaceSettingsPresenter {
                owner: Arc::clone(&owner),
                hide_private: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.interface.hide_private,
                    |owner, value| {
                        owner
                            .edit::<InterfaceSettings>(|s| s.hide_private = value)
                            .map(|_| ())
                    },
                ),
            },
            presets: PresetsSettingsPresenter {
                active: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.presets.active,
                    |owner, value| {
                        owner
                            .edit::<PresetSettings>(|settings| settings.active = value)
                            .map(|_| ())
                    },
                ),
            },
            characters: CharactersSettingsPresenter::new(Arc::clone(&owner)),
            tts: TtsSettingsPresenter {
                preferences: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.speech.tts.clone(),
                    |owner, value| {
                        owner
                            .edit::<SpeechSettings>(|settings| settings.tts = value)
                            .map(|_| ())
                    },
                ),
                model_options: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.speech.tts_model_options.clone(),
                    |owner, value| {
                        owner
                            .edit::<SpeechSettings>(|settings| settings.tts_model_options = value)
                            .map(|_| ())
                    },
                ),
            },
            asr: AsrSettingsPresenter {
                preferences: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.speech.asr.clone(),
                    |owner, value| {
                        owner
                            .edit::<SpeechSettings>(|settings| settings.asr = value)
                            .map(|_| ())
                    },
                ),
                model_options: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.speech.asr_model_options.clone(),
                    |owner, value| {
                        owner
                            .edit::<SpeechSettings>(|settings| settings.asr_model_options = value)
                            .map(|_| ())
                    },
                ),
            },
            voice: VoiceSettingsPresenter {
                volume: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.voice.volume,
                    |owner, value| {
                        owner
                            .edit::<VoiceSettings>(|section| section.volume = value)
                            .map(|_| ())
                    },
                ),
                in_launcher: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.voice.in_launcher,
                    |owner, value| {
                        owner
                            .edit::<VoiceSettings>(|section| section.in_launcher = value)
                            .map(|_| ())
                    },
                ),
                in_game: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.voice.in_game,
                    |owner, value| {
                        owner
                            .edit::<VoiceSettings>(|section| section.in_game = value)
                            .map(|_| ())
                    },
                ),
                voice: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.voice.voice.clone(),
                    |owner, value| {
                        owner
                            .edit::<VoiceSettings>(|section| section.voice = value)
                            .map(|_| ())
                    },
                ),
            },
        }
    }
    pub fn snapshot(&self) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.owner.snapshot_all()
    }
    pub fn observe(&self) -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        self.owner.observe()
    }
    pub fn export(&self) -> Result<SettingsTransfer, SettingsError> {
        self.owner.export()
    }
    pub fn import(
        &self,
        expected_revision: u64,
        transfer: SettingsTransfer,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.owner.import(expected_revision, transfer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{
        MemorySettingsRepository, SettingsConfig, SettingsLifecycle, SettingsSource,
    };

    #[tokio::test]
    async fn tts_asr_preferences_round_trip_through_settings_owner() {
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let presenter = SettingsPresenter::new(Arc::clone(&owner));
        let mut tts_updates = presenter.tts.preferences.observe();
        let mut asr_updates = presenter.asr.preferences.observe();
        assert_eq!(tts_updates.next().await.unwrap(), TtsPreferences::default());
        assert_eq!(asr_updates.next().await.unwrap(), AsrPreferences::default());

        let mut tts = presenter.tts.preferences.get();
        tts.backend = crate::settings::TtsBackend::Local;
        tts.local_model_id = "f5_tts_ru".into();
        tts.character_voice_ids
            .insert(CharacterId::ShortHair, "voice3".into());
        presenter.tts.preferences.set(tts.clone()).unwrap();
        assert_eq!(tts_updates.next().await.unwrap(), tts);

        let mut asr = presenter.asr.preferences.get();
        asr.recognizer_id = "whisper".into();
        asr.model_languages.insert("whisper".into(), "en".into());
        asr.model_languages.insert("gigaam".into(), "ru".into());
        presenter.asr.preferences.set(asr.clone()).unwrap();
        assert_eq!(asr_updates.next().await.unwrap(), asr);
        assert_eq!(presenter.tts.preferences.get(), tts);
        assert_eq!(presenter.asr.preferences.get(), asr);
        assert_eq!(owner.snapshot_all().unwrap().values.speech.tts, tts);
        assert_eq!(owner.snapshot_all().unwrap().values.speech.asr, asr);
    }

    #[tokio::test]
    async fn character_profile_edit_is_atomic_isolated_and_persistent() {
        use crate::lifecycle::Lifecycle;
        use tokio_util::sync::CancellationToken;

        let repository = Arc::new(MemorySettingsRepository::new());
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: repository.clone(),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let presenter = SettingsPresenter::new(Arc::clone(&owner));
        let original = presenter.characters.profile(CharacterId::Crazy).unwrap();

        let mut edited = original.settings().clone();
        edited.prompt_set = Some("Default".into());
        edited.presets.react = PresetSelection::Specific(PresetId(42));
        let accepted = presenter
            .characters
            .save_profile(&original, edited.clone())
            .unwrap();
        assert_eq!(accepted.revision(), original.revision() + 1);
        assert_eq!(accepted.character(), CharacterId::Crazy);
        assert_eq!(accepted.settings(), &edited);
        assert_eq!(
            presenter.characters.profile(CharacterId::Kind).unwrap().settings(),
            &CharacterSettings::default()
        );

        let error = presenter
            .characters
            .save_profile(&original, CharacterSettings::default())
            .unwrap_err();
        assert!(matches!(error, SettingsError::Conflict { .. }));

        let mut invalid = accepted.settings().clone();
        invalid.prompt_set = Some("../invalid".into());
        invalid.presets.reply = PresetSelection::Specific(PresetId(24));
        assert!(matches!(
            presenter.characters.save_profile(&accepted, invalid),
            Err(SettingsError::Validation(_))
        ));
        assert_eq!(
            presenter.characters.profile(CharacterId::Crazy).unwrap().settings(),
            &edited
        );

        let stop = CancellationToken::new();
        let task = tokio::spawn(lifecycle.run(stop.clone()));
        owner.flush().await.unwrap();
        stop.cancel();
        task.await.unwrap().unwrap();

        let restored = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::Stored,
            repository,
        })
        .await
        .unwrap();
        let restored = SettingsPresenter::new(restored.owner());
        assert_eq!(
            restored.characters.profile(CharacterId::Crazy).unwrap().settings(),
            &edited
        );
        assert_eq!(
            restored.characters.profile(CharacterId::Kind).unwrap().settings(),
            &CharacterSettings::default()
        );
    }

    #[tokio::test]
    async fn character_preset_bindings_are_independent_and_observable() {
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let presenter = SettingsPresenter::new(Arc::clone(&owner));

        let react = &presenter.characters.for_character(CharacterId::Crazy).react;
        let mut updates = react.observe();
        assert_eq!(updates.next().await.unwrap(), PresetSelection::Active);
        react.set(PresetSelection::Specific(PresetId(7))).unwrap();
        assert_eq!(
            updates.next().await.unwrap(),
            PresetSelection::Specific(PresetId(7))
        );
        assert_eq!(react.get(), PresetSelection::Specific(PresetId(7)));
        assert_eq!(
            presenter
                .characters
                .for_character(CharacterId::Kind)
                .react
                .get(),
            PresetSelection::Active
        );
        assert_eq!(
            presenter
                .characters
                .for_character(CharacterId::Crazy)
                .reply
                .get(),
            PresetSelection::Active
        );

        owner
            .edit::<CharactersSettings>(|settings| {
                settings.crazy.presets.react = PresetSelection::Specific(PresetId(9));
            })
            .unwrap();
        assert_eq!(
            updates.next().await.unwrap(),
            PresetSelection::Specific(PresetId(9))
        );

        let mut active = presenter.presets.active.observe();
        assert_eq!(active.next().await.unwrap(), PresetId(0));
        presenter.presets.active.set(PresetId(5)).unwrap();
        assert_eq!(active.next().await.unwrap(), PresetId(5));
        assert_eq!(
            owner
                .snapshot_all()
                .unwrap()
                .values
                .selected_preset(CharacterId::Crazy, crate::interaction::TurnIntent::Initiate),
            PresetId(5)
        );
        assert_eq!(
            owner.snapshot_all().unwrap().values.selected_preset(
                CharacterId::Crazy,
                crate::interaction::TurnIntent::React(crate::identity::EventId::new())
            ),
            PresetId(9)
        );
    }
}
