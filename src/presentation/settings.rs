use std::{collections::BTreeMap, sync::Arc};

use tokio::sync::watch;

use crate::{
    character::CharacterId,
    settings::{
        AppSettings, CharactersSettings, InterfaceSettings, OperationPresets, PresetId,
        PresetSelection, PresetSettings, SettingsArea, SettingsError, SettingsOwner,
        SettingsSnapshot, SettingsTransfer, VoiceSettings, Volume,
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
    pub assignments: BoundProperty<OperationPresets>,
    pub reply: BoundProperty<PresetSelection>,
    pub react: BoundProperty<PresetSelection>,
    pub initiate: BoundProperty<PresetSelection>,
}

impl CharacterSettingsPresenter {
    fn new(owner: Arc<SettingsOwner>, character: CharacterId) -> Self {
        Self {
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
    async fn character_preset_bindings_are_independent_and_observable() {
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::Initial(AppSettings::initial_profile()),
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
