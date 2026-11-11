mod instance_lease;
mod migration;
mod model;
mod preset_activation;
mod python_presets;
mod python_schemas;
mod speech;
mod storage;
mod transfer;

pub use crate::generation::PresetId;
pub(crate) use instance_lease::SettingsInstanceLease;
pub(crate) use migration::SettingsStartupMigration;
pub use migration::{PythonSettingsMigration, PythonSettingsPreview, RustSettingsMigration};
pub use model::*;
pub use preset_activation::{
    PresetActivation, PresetActivationError, PresetRecovery, PresetStorageLease,
    PythonSettingsImportReview,
};
pub use python_presets::{
    PythonPresetPreferencesReview, PythonPresetStage, PythonPresetStageError,
    PythonPresetStagePreview,
};
pub use python_schemas::{
    PythonSchemaAuditError, PythonSchemaDirectoryReview, PythonSchemaFileKind,
    PythonSchemaFileReview,
};
pub use speech::*;
pub(crate) use storage::LeasedJsonSettingsRepository;
pub use storage::*;
pub use transfer::{SettingsImportFormat, SettingsTransfer, SettingsTransferError};

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::lifecycle::Lifecycle;

/// Static settings entry point; its runtime-bound methods are implemented in `crate::api`.
pub struct SettingsApi;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("settings are not initialized")]
    Uninitialized,
    #[error("settings are stopping")]
    Stopped,
    #[error("settings are frozen for a maintenance inspection")]
    MaintenanceActive,
    #[error("maintenance requires the current settings revision to be durably saved")]
    MaintenanceUnpersisted,
    #[error("settings conflict: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("invalid settings: {0}")]
    Validation(String),
    #[error("settings storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceState {
    Pending,
    Saved(u64),
    Failed { revision: u64, error: SettingsError },
}

#[derive(Debug, Clone)]
pub struct SettingsSnapshot {
    pub closed: bool,
    pub revision: u64,
    pub values: AppSettings,
    pub persisted_revision: Option<u64>,
    pub persistence: PersistenceState,
}

pub enum SettingsSource {
    Initial(Box<AppSettings>),
    Stored,
}
impl SettingsSource {
    pub fn initial(settings: AppSettings) -> Self {
        Self::Initial(Box::new(settings))
    }
}

pub struct SettingsConfig {
    pub source: SettingsSource,
    pub repository: Arc<dyn SettingsRepository>,
}

struct State {
    snapshot: Arc<SettingsSnapshot>,
    closed: bool,
    save_generation: u64,
    maintenance_active: bool,
}
pub(crate) struct SettingsOwner {
    owned_root: Option<PathBuf>,
    state: Mutex<State>,
    updates: watch::Sender<Arc<SettingsSnapshot>>,
    save: Notify,
}

/// Holds the current persisted settings revision stable while a caller performs
/// a read-only maintenance inspection. Does not authorize removing any layer.
pub(crate) struct SettingsMaintenanceGuard<'a> {
    owner: &'a SettingsOwner,
    snapshot: Arc<SettingsSnapshot>,
}

impl SettingsMaintenanceGuard<'_> {
    pub(crate) fn snapshot(&self) -> &SettingsSnapshot {
        &self.snapshot
    }
}

impl Drop for SettingsMaintenanceGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().expect("settings mutex poisoned");
        state.maintenance_active = false;
    }
}

impl SettingsOwner {
    /// Freezes settings writes at an already-persisted revision. No async lock
    /// is retained: every mutating entry point checks this flag under `state`.
    pub(crate) fn freeze_persisted(&self) -> Result<SettingsMaintenanceGuard<'_>, SettingsError> {
        let mut state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        if state.maintenance_active {
            return Err(SettingsError::MaintenanceActive);
        }
        let snapshot = Arc::clone(&state.snapshot);
        if snapshot.persisted_revision != Some(snapshot.revision)
            || snapshot.persistence != PersistenceState::Saved(snapshot.revision)
        {
            return Err(SettingsError::MaintenanceUnpersisted);
        }
        state.maintenance_active = true;
        Ok(SettingsMaintenanceGuard {
            owner: self,
            snapshot,
        })
    }

    /// Binds this settings freeze to the *application-owned* repository for
    /// the same portable data root. In-memory and ad-hoc repositories are not
    /// acceptable evidence for inspecting an installed revision.
    pub(crate) fn freeze_persisted_for_root(
        &self,
        root: &SettingsInstanceLease,
    ) -> Result<SettingsMaintenanceGuard<'_>, SettingsError> {
        let owned = self.owned_root.as_ref().ok_or_else(|| {
            SettingsError::Storage(
                "maintenance requires an application-owned settings repository".into(),
            )
        })?;
        if !root
            .matches_root(owned)
            .map_err(|error| SettingsError::Storage(error.to_string()))?
        {
            return Err(SettingsError::Storage(
                "settings owner and revision root disagree".into(),
            ));
        }
        self.freeze_persisted()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Arc<SettingsSnapshot>> {
        self.updates.subscribe()
    }

    pub(crate) fn snapshot<T: SettingsSection>(&self) -> Result<SectionSnapshot<T>, SettingsError> {
        let snapshot = self.snapshot_all()?;
        Ok(SectionSnapshot {
            revision: snapshot.revision,
            value: T::read(&snapshot.values),
        })
    }
    pub(crate) fn snapshot_all(&self) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        let state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        Ok(Arc::clone(&state.snapshot))
    }
    pub(crate) fn observe(&self) -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        let state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        Ok(self.updates.subscribe())
    }

    pub(crate) fn export(&self) -> Result<SettingsTransfer, SettingsError> {
        let snapshot = self.snapshot_all()?;
        Ok(SettingsTransfer::new(StoredSettings {
            revision: snapshot.revision,
            settings: snapshot.values.clone(),
        })?)
    }

    pub(crate) fn import(
        &self,
        expected_revision: u64,
        transfer: SettingsTransfer,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.change(Some(expected_revision), move |settings| {
            *settings = transfer.into_stored().settings;
        })
    }
    pub(crate) fn replace<T: SettingsSection>(
        &self,
        expected_revision: u64,
        value: T,
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        let snapshot = self.change(Some(expected_revision), move |settings| {
            value.replace(settings)
        })?;
        Ok(SectionSnapshot {
            revision: snapshot.revision,
            value: T::read(&snapshot.values),
        })
    }
    /// The edit must be short and must not block or re-enter SettingsApi.
    pub(crate) fn edit<T: SettingsSection>(
        &self,
        edit: impl FnOnce(&mut T),
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        let snapshot = self.change(None, move |settings| {
            let mut value = T::read(settings);
            edit(&mut value);
            value.replace(settings);
        })?;
        Ok(SectionSnapshot {
            revision: snapshot.revision,
            value: T::read(&snapshot.values),
        })
    }
    pub(crate) fn edit_checked<T: SettingsSection>(
        &self,
        edit: impl FnOnce(&mut T) -> Result<(), SettingsError>,
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        let snapshot = self.try_change(None, move |settings| {
            let mut value = T::read(settings);
            edit(&mut value)?;
            value.replace(settings);
            Ok(())
        })?;
        Ok(SectionSnapshot {
            revision: snapshot.revision,
            value: T::read(&snapshot.values),
        })
    }
    pub(crate) async fn flush(&self) -> Result<(), SettingsError> {
        let mut updates = self.observe()?;
        let target = updates.borrow().revision;
        self.save.notify_one();
        loop {
            let snapshot = Arc::clone(&updates.borrow_and_update());
            if snapshot
                .persisted_revision
                .is_some_and(|revision| revision >= target)
            {
                return Ok(());
            }
            if let PersistenceState::Failed { revision, error } = &snapshot.persistence
                && *revision >= target
            {
                return Err(error.clone());
            }
            if snapshot.closed {
                return Err(SettingsError::Stopped);
            }
            updates
                .changed()
                .await
                .map_err(|_| SettingsError::Stopped)?;
        }
    }
}

impl SettingsOwner {
    pub(crate) fn request_save(&self) -> Result<(), SettingsError> {
        let mut state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        if state.maintenance_active {
            return Err(SettingsError::MaintenanceActive);
        }
        state.save_generation = state
            .save_generation
            .checked_add(1)
            .ok_or_else(|| SettingsError::Validation("save generation exhausted".into()))?;
        let mut snapshot = (*state.snapshot).clone();
        snapshot.persistence = PersistenceState::Pending;
        state.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&state.snapshot));
        self.save.notify_one();
        Ok(())
    }
    fn change(
        &self,
        expected: Option<u64>,
        change: impl FnOnce(&mut AppSettings),
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        self.try_change(expected, |settings| {
            change(settings);
            Ok(())
        })
    }
    fn try_change(
        &self,
        expected: Option<u64>,
        change: impl FnOnce(&mut AppSettings) -> Result<(), SettingsError>,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        let mut state = self.state.lock().expect("settings mutex poisoned");
        if state.closed {
            return Err(SettingsError::Stopped);
        }
        if state.maintenance_active {
            return Err(SettingsError::MaintenanceActive);
        }
        if let Some(expected) = expected
            && expected != state.snapshot.revision
        {
            return Err(SettingsError::Conflict {
                expected,
                actual: state.snapshot.revision,
            });
        }
        let mut values = state.snapshot.values.clone();
        change(&mut values)?;
        values.validate()?;
        if values == state.snapshot.values {
            return Ok(Arc::clone(&state.snapshot));
        }
        let revision = state
            .snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| SettingsError::Validation("revision exhausted".into()))?;
        let snapshot = Arc::new(SettingsSnapshot {
            closed: false,
            revision,
            values,
            persisted_revision: state.snapshot.persisted_revision,
            persistence: PersistenceState::Pending,
        });
        state.save_generation = state
            .save_generation
            .checked_add(1)
            .ok_or_else(|| SettingsError::Validation("save generation exhausted".into()))?;
        state.snapshot = Arc::clone(&snapshot);
        self.updates.send_replace(Arc::clone(&snapshot));
        self.save.notify_one();
        Ok(snapshot)
    }
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("settings mutex poisoned");
        state.closed = true;
        let mut snapshot = (*state.snapshot).clone();
        snapshot.closed = true;
        state.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&state.snapshot));
    }
}

pub struct SettingsLifecycle {
    owner: Arc<SettingsOwner>,
    repository: Arc<dyn SettingsRepository>,
}

impl SettingsLifecycle {
    pub(crate) async fn prepare(config: SettingsConfig) -> Result<Self, SettingsError> {
        let stored = match config.source {
            SettingsSource::Initial(settings) => {
                let settings = *settings;
                settings.validate()?;
                StoredSettings {
                    revision: 0,
                    settings,
                }
            }
            SettingsSource::Stored => {
                let repository = Arc::clone(&config.repository);
                tokio::task::spawn_blocking(move || repository.load())
                    .await
                    .map_err(|error| SettingsError::Storage(error.to_string()))??
            }
        };
        stored.settings.validate()?;
        let owned_root = if config.repository.has_application_root_owner() {
            Some(
                config
                    .repository
                    .lifecycle_root()
                    .ok_or_else(|| SettingsError::Storage("owned repository has no root".into()))?
                    .canonicalize()
                    .map_err(|error| SettingsError::Storage(error.to_string()))?,
            )
        } else {
            None
        };
        let snapshot = Arc::new(SettingsSnapshot {
            closed: false,
            revision: stored.revision,
            values: stored.settings,
            persisted_revision: None,
            persistence: PersistenceState::Pending,
        });
        let (updates, _) = watch::channel(Arc::clone(&snapshot));
        Ok(Self {
            owner: Arc::new(SettingsOwner {
                owned_root,
                state: Mutex::new(State {
                    snapshot,
                    closed: false,
                    save_generation: 0,
                    maintenance_active: false,
                }),
                updates,
                save: Notify::new(),
            }),
            repository: config.repository,
        })
    }
    pub(crate) fn owner(&self) -> Arc<SettingsOwner> {
        Arc::clone(&self.owner)
    }

    async fn persist(&self) -> Result<(), SettingsError> {
        let (snapshot, generation) = {
            let state = self.owner.state.lock().expect("settings mutex poisoned");
            (Arc::clone(&state.snapshot), state.save_generation)
        };
        if snapshot.persisted_revision == Some(snapshot.revision) {
            return Ok(());
        }
        let stored = StoredSettings {
            revision: snapshot.revision,
            settings: snapshot.values.clone(),
        };
        let repository = Arc::clone(&self.repository);
        let result = tokio::task::spawn_blocking(move || repository.save(&stored))
            .await
            .map_err(|error| SettingsError::Storage(error.to_string()))
            .and_then(|result| result);
        let mut state = self.owner.state.lock().expect("settings mutex poisoned");
        if result.is_err() && state.save_generation != generation {
            return result;
        }
        let mut next = (*state.snapshot).clone();
        match &result {
            Ok(()) => {
                next.persisted_revision = Some(snapshot.revision);
                next.persistence = if next.revision == snapshot.revision {
                    PersistenceState::Saved(snapshot.revision)
                } else {
                    PersistenceState::Pending
                };
            }
            Err(error) => {
                next.persistence = PersistenceState::Failed {
                    revision: snapshot.revision,
                    error: error.clone(),
                };
            }
        }
        state.snapshot = Arc::new(next);
        self.owner.updates.send_replace(Arc::clone(&state.snapshot));
        result
    }
}

impl Lifecycle for SettingsLifecycle {
    type Error = SettingsError;
    async fn run(self, stop: CancellationToken) -> Result<(), Self::Error> {
        let _ = self.persist().await;
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => { self.owner.close(); return self.persist().await; }
                _ = self.owner.save.notified() => { let _ = self.persist().await; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use tokio::sync::oneshot;

    struct GatedFailure {
        entered: Mutex<Option<oneshot::Sender<()>>>,
        release: Arc<Barrier>,
    }
    impl SettingsRepository for GatedFailure {
        fn migrate(&self) -> Result<(), SettingsError> {
            Err(SettingsError::Storage("not used".into()))
        }
        fn load(&self) -> Result<StoredSettings, SettingsError> {
            Err(SettingsError::Storage("not used".into()))
        }
        fn save(&self, _: &StoredSettings) -> Result<(), SettingsError> {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            self.release.wait();
            Err(SettingsError::Storage("old attempt failed".into()))
        }
    }

    #[tokio::test]
    async fn old_save_failure_cannot_overwrite_explicit_retry() {
        let (entered, started) = oneshot::channel();
        let release = Arc::new(Barrier::new(2));
        let repository = Arc::new(GatedFailure {
            entered: Mutex::new(Some(entered)),
            release: Arc::clone(&release),
        });
        let lifecycle = Arc::new(
            SettingsLifecycle::prepare(SettingsConfig {
                source: SettingsSource::initial(AppSettings::initial_profile()),
                repository,
            })
            .await
            .unwrap(),
        );
        let saving = Arc::clone(&lifecycle);
        let save = tokio::spawn(async move { saving.persist().await });
        started.await.unwrap();
        lifecycle.owner.request_save().unwrap();
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .unwrap();
        assert!(save.await.unwrap().is_err());
        assert_eq!(
            lifecycle.owner.state.lock().unwrap().snapshot.persistence,
            PersistenceState::Pending
        );
    }
    #[tokio::test]
    async fn maintenance_freeze_requires_a_durably_saved_revision() {
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        assert!(matches!(
            owner.freeze_persisted(),
            Err(SettingsError::MaintenanceUnpersisted)
        ));
        lifecycle.persist().await.unwrap();
        let guard = owner.freeze_persisted().unwrap();
        assert_eq!(guard.snapshot().persisted_revision, Some(0));
        assert!(matches!(
            owner.freeze_persisted(),
            Err(SettingsError::MaintenanceActive)
        ));
        assert_eq!(owner.request_save(), Err(SettingsError::MaintenanceActive));
        assert!(matches!(
            owner.edit::<SpeechSettings>(|speech| speech.tts.enabled = !speech.tts.enabled),
            Err(SettingsError::MaintenanceActive)
        ));
        assert!(matches!(
            owner.replace(0, SpeechSettings::default()),
            Err(SettingsError::MaintenanceActive)
        ));
        let transfer = owner.export().unwrap();
        assert!(matches!(
            owner.import(0, transfer),
            Err(SettingsError::MaintenanceActive)
        ));
        assert_eq!(
            owner.snapshot_all().unwrap().revision,
            guard.snapshot().revision
        );
        drop(guard);
        owner
            .edit::<SpeechSettings>(|speech| speech.tts.enabled = !speech.tts.enabled)
            .unwrap();
        assert!(matches!(
            owner.freeze_persisted(),
            Err(SettingsError::MaintenanceUnpersisted)
        ));
        lifecycle.persist().await.unwrap();
        assert_eq!(owner.freeze_persisted().unwrap().snapshot().revision, 1);
    }

    #[tokio::test]
    async fn maintenance_freeze_is_race_free_against_parallel_settings_edits() {
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        lifecycle.persist().await.unwrap();
        let owner = lifecycle.owner();
        let guard = owner.freeze_persisted().unwrap();
        let attempt = std::thread::spawn({
            let owner = Arc::clone(&owner);
            move || owner.edit::<SpeechSettings>(|speech| speech.tts.enabled = !speech.tts.enabled)
        });
        assert!(matches!(
            attempt.join().unwrap(),
            Err(SettingsError::MaintenanceActive)
        ));
        assert_eq!(
            guard.snapshot().revision,
            owner.snapshot_all().unwrap().revision
        );
        drop(guard);
        assert!(
            owner
                .edit::<SpeechSettings>(|speech| speech.tts.enabled = !speech.tts.enabled)
                .is_ok()
        );
    }
}
