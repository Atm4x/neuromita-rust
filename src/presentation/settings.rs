use std::{collections::BTreeMap, sync::Arc};

use tokio::sync::watch;

use crate::{
    character::CharacterId,
    generation::ToolSettings,
    settings::{
        AppSettings, AsrPreferences, CharactersSettings, GuideLevel, InterfaceSettings,
        OnboardingSettings, OperationPresets, PresetId, PresetSelection, PresetSettings,
        SettingsArea, SettingsError, SettingsOwner, SettingsSnapshot, SettingsTransfer,
        SpeechSettings, StructuredDisplayMode, TtsPreferences, VoiceSettings, Volume,
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

pub struct ToolsSettingsPresenter {
    pub state: BoundProperty<ToolSettings>,
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

pub struct CharactersSettingsPresenter {
    characters: BTreeMap<CharacterId, CharacterSettingsPresenter>,
}

impl CharactersSettingsPresenter {
    fn new(owner: Arc<SettingsOwner>) -> Self {
        Self {
            characters: CharacterId::ALL
                .into_iter()
                .map(|id| (id, CharacterSettingsPresenter::new(Arc::clone(&owner), id)))
                .collect(),
        }
    }
    pub fn for_character(&self, character: CharacterId) -> &CharacterSettingsPresenter {
        &self.characters[&character]
    }
}

pub struct OnboardingSettingsPresenter {
    pub guide_level: BoundProperty<GuideLevel>,
    pub guide_page: BoundProperty<u8>,
    pub guide_completed: BoundProperty<bool>,
}

pub struct InterfaceSettingsPresenter {
    owner: Arc<SettingsOwner>,
    pub hide_private: BoundProperty<bool>,
    pub structured_display: BoundProperty<StructuredDisplayMode>,
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
    pub onboarding: OnboardingSettingsPresenter,
    pub interface: InterfaceSettingsPresenter,
    pub voice: VoiceSettingsPresenter,
    pub tts: TtsSettingsPresenter,
    pub asr: AsrSettingsPresenter,
    pub presets: PresetsSettingsPresenter,
    pub tools: ToolsSettingsPresenter,
    pub characters: CharactersSettingsPresenter,
}
impl SettingsPresenter {
    pub(crate) fn new(owner: Arc<SettingsOwner>) -> Self {
        Self {
            owner: Arc::clone(&owner),
            onboarding: OnboardingSettingsPresenter {
                guide_level: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.onboarding.guide_level,
                    |owner, level| {
                        owner
                            .edit::<OnboardingSettings>(|settings| {
                                let page = crate::onboarding::guide::remap_index(
                                    settings.guide_level,
                                    usize::from(settings.guide_page),
                                    level,
                                );
                                settings.guide_level = level;
                                settings.guide_page = page as u8;
                            })
                            .map(|_| ())
                    },
                ),
                guide_page: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.onboarding.guide_page,
                    |owner, page| {
                        owner
                            .edit::<OnboardingSettings>(|settings| settings.guide_page = page)
                            .map(|_| ())
                    },
                ),
                guide_completed: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.onboarding.guide_completed,
                    |owner, completed| {
                        owner
                            .edit::<OnboardingSettings>(|settings| {
                                settings.guide_completed = completed
                            })
                            .map(|_| ())
                    },
                ),
            },
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
                structured_display: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.interface.structured_display,
                    |owner, value| {
                        owner
                            .edit::<InterfaceSettings>(|s| s.structured_display = value)
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
            tools: ToolsSettingsPresenter {
                state: BoundProperty::new(
                    Arc::clone(&owner),
                    |settings| settings.tools.clone(),
                    |owner, value| {
                        owner
                            .edit::<ToolSettings>(|settings| *settings = value)
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
    async fn guide_level_changes_through_onboarding_owner_without_extra_settings_store() {
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let presenter = SettingsPresenter::new(Arc::clone(&owner));
        let mut observed = presenter.onboarding.guide_level.observe();
        assert_eq!(observed.next().await.unwrap(), GuideLevel::Basic);
        presenter
            .onboarding
            .guide_level
            .set(GuideLevel::Full)
            .unwrap();
        assert_eq!(observed.next().await.unwrap(), GuideLevel::Full);
        assert_eq!(owner.snapshot_all().unwrap().revision, 1);
        presenter
            .onboarding
            .guide_level
            .set(GuideLevel::Full)
            .unwrap();
        assert_eq!(owner.snapshot_all().unwrap().revision, 1);
        assert!(
            !owner
                .snapshot_all()
                .unwrap()
                .values
                .onboarding
                .eula_accepted
        );
    }

    #[tokio::test]
    async fn guide_page_and_completion_share_the_settings_owner() {
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let presenter = SettingsPresenter::new(Arc::clone(&owner));
        assert!(!presenter.onboarding.guide_completed.get());
        assert_eq!(presenter.onboarding.guide_page.get(), 0);
        presenter
            .onboarding
            .guide_level
            .set(GuideLevel::Full)
            .unwrap();
        presenter.onboarding.guide_page.set(7).unwrap();
        let snapshot = owner.snapshot_all().unwrap();
        assert_eq!(snapshot.values.onboarding.guide_page, 7);
        assert!(!snapshot.values.onboarding.guide_completed);
        presenter.onboarding.guide_completed.set(true).unwrap();
        assert!(presenter.onboarding.guide_completed.get());
        assert_eq!(owner.snapshot_all().unwrap().revision, 3);
        presenter.onboarding.guide_completed.set(true).unwrap();
        assert_eq!(owner.snapshot_all().unwrap().revision, 3);
    }

    #[tokio::test]
    async fn persisted_guide_completion_survives_real_settings_lifecycle_restart() {
        use crate::{lifecycle::Lifecycle, onboarding, settings::JsonSettingsRepository};
        use tokio_util::sync::CancellationToken;

        let directory =
            std::env::temp_dir().join(format!("neuromita-guide-progress-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut initial = AppSettings::initial_profile();
        initial.onboarding.eula_accepted = true;
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(initial),
            repository: Arc::new(JsonSettingsRepository::new(directory.join("settings.json"))),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let task = tokio::spawn(async move { lifecycle.run(cancelled).await });
        let presenter = SettingsPresenter::new(Arc::clone(&owner));
        presenter
            .onboarding
            .guide_level
            .set(GuideLevel::Full)
            .unwrap();
        presenter.onboarding.guide_page.set(8).unwrap();
        owner.flush().await.unwrap();
        let resumed = onboarding::startup_state(&directory).unwrap();
        assert!(resumed.requires_guide);
        assert_eq!(resumed.guide_page, 8);
        assert_eq!(resumed.guide_level, GuideLevel::Full);
        presenter.onboarding.guide_completed.set(true).unwrap();
        owner.flush().await.unwrap();
        let completed = onboarding::startup_state(&directory).unwrap();
        assert!(!completed.requires_guide);
        assert!(!completed.requires_consent);
        assert_eq!(completed.guide_page, 8);
        stop.cancel();
        task.await.unwrap().unwrap();
        assert_eq!(onboarding::startup_state(&directory).unwrap(), completed);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn guide_level_switch_remaps_page_in_one_revision_and_survives_restart() {
        use crate::{lifecycle::Lifecycle, onboarding, settings::JsonSettingsRepository};
        use tokio_util::sync::CancellationToken;

        let directory = std::env::temp_dir().join(format!(
            "neuromita-guide-level-switch-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut initial = AppSettings::initial_profile();
        initial.onboarding.eula_accepted = true;
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(initial),
            repository: Arc::new(JsonSettingsRepository::new(directory.join("settings.json"))),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let task = tokio::spawn(async move { lifecycle.run(cancelled).await });
        let presenter = SettingsPresenter::new(Arc::clone(&owner));
        presenter
            .onboarding
            .guide_level
            .set(GuideLevel::Full)
            .unwrap();
        presenter.onboarding.guide_page.set(12).unwrap();
        owner.flush().await.unwrap();
        let before = owner.snapshot_all().unwrap().revision;

        presenter
            .onboarding
            .guide_level
            .set(GuideLevel::Basic)
            .unwrap();
        let snapshot = owner.snapshot_all().unwrap();
        assert_eq!(snapshot.revision, before + 1);
        assert_eq!(snapshot.values.onboarding.guide_level, GuideLevel::Basic);
        assert_eq!(snapshot.values.onboarding.guide_page, 2);
        owner.flush().await.unwrap();
        let resumed = onboarding::startup_state(&directory).unwrap();
        assert!(resumed.requires_guide);
        assert_eq!(resumed.guide_level, GuideLevel::Basic);
        assert_eq!(resumed.guide_page, 2);

        let before = snapshot.revision;
        presenter
            .onboarding
            .guide_level
            .set(GuideLevel::Basic)
            .unwrap();
        assert_eq!(owner.snapshot_all().unwrap().revision, before);
        stop.cancel();
        task.await.unwrap().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

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
