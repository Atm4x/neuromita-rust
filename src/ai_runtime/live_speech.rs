//! Application-owned, read-only reconfiguration of already installed speech workers.
//! A candidate must initialize before it can replace an active runtime.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use thiserror::Error;
use tokio::{
    sync::{Mutex, RwLock, watch},
    time::MissedTickBehavior,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    audio::{
        controller::SpeechSessionController,
        session::{SpeechFuture, SpeechSessionError, SpeechTranscriber},
    },
    settings::{SettingsOwner, SpeechSettings, TtsBackend},
    voice::{VoiceError, VoiceFuture, VoiceOptions, VoiceProvider},
};

use super::{
    AiRuntimeError, AiService,
    application::{AiApplicationError, RunningAiApplication},
    installed_speech::{InstalledSpeechEnvironment, InstalledSpeechError},
    runtime_status::{RuntimePhase, RuntimeSnapshot},
    speech::Transcript,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SelectionKey {
    tts: Option<(String, String, String)>,
    asr: Option<(String, String, String)>,
}

impl SelectionKey {
    fn from_settings(settings: &SpeechSettings) -> Self {
        let tts = (settings.tts.enabled
            && settings.tts.backend == TtsBackend::Local
            && settings.tts.local_auto_load)
            .then(|| {
                let id = &settings.tts.local_model_id;
                (
                    id.clone(),
                    settings.tts.local_language.clone(),
                    settings
                        .tts_model_options
                        .get(id)
                        .map_or_else(|| "{}".to_owned(), serde_json::Value::to_string),
                )
            });
        let asr = (!settings.asr.recognizer_id.is_empty()).then(|| {
            let id = &settings.asr.recognizer_id;
            (
                id.clone(),
                settings
                    .asr
                    .model_languages
                    .get(id)
                    .cloned()
                    .unwrap_or_default(),
                settings
                    .asr_model_options
                    .get(id)
                    .map_or_else(|| "{}".to_owned(), serde_json::Value::to_string),
            )
        });
        Self { tts, asr }
    }

    fn services(&self) -> Vec<AiService> {
        let mut services = Vec::new();
        if self.tts.is_some() {
            services.push(AiService::Tts);
        }
        if self.asr.is_some() {
            services.push(AiService::Asr);
        }
        services
    }

    fn plugin_id(&self, service: AiService) -> Option<&str> {
        match service {
            AiService::Tts => self.tts.as_ref().map(|t| t.0.as_str()),
            AiService::Asr => self.asr.as_ref().map(|a| a.0.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum SpeechReconfigurationError {
    #[error(transparent)]
    Installed(#[from] InstalledSpeechError),
    #[error(transparent)]
    Runtime(#[from] AiApplicationError),
}

const RECOVERY_POLL_INTERVAL: Duration = Duration::from_secs(3);
const MAX_RECOVERY_ATTEMPTS_PER_GENERATION: u8 = 2;

#[derive(Default)]
struct RecoveryBudget {
    observed: HashMap<AiService, (Uuid, u8)>,
}

impl RecoveryBudget {
    fn claim(&mut self, service: AiService, generation: Uuid) -> bool {
        let entry = self.observed.entry(service).or_insert((generation, 0));
        if entry.0 != generation {
            *entry = (generation, 0);
        }
        if entry.1 >= MAX_RECOVERY_ATTEMPTS_PER_GENERATION {
            return false;
        }
        entry.1 += 1;
        true
    }

    fn reset(&mut self) {
        self.observed.clear();
    }
}

pub struct LiveSpeechRuntime {
    installed: InstalledSpeechEnvironment,
    current: RwLock<Option<RunningAiApplication>>,
    transition: Mutex<()>,
    applied_selection: Mutex<SelectionKey>,
    status: watch::Sender<RuntimeSnapshot>,
    shutting_down: AtomicBool,
}

impl LiveSpeechRuntime {
    pub fn new(
        installed: InstalledSpeechEnvironment,
        initial: Option<RunningAiApplication>,
        settings: &SpeechSettings,
        startup_error: Option<&str>,
    ) -> Self {
        let applied_selection = if initial.is_some() {
            SelectionKey::from_settings(settings)
        } else {
            SelectionKey::default()
        };
        let mut snapshot = initial.as_ref().map_or_else(
            RuntimeSnapshot::default,
            RunningAiApplication::status_snapshot,
        );
        if let Some(error) = startup_error {
            mark_failure(&mut snapshot, &SelectionKey::from_settings(settings), error);
        }
        Self {
            installed,
            current: RwLock::new(initial),
            transition: Mutex::new(()),
            applied_selection: Mutex::new(applied_selection),
            status: watch::channel(snapshot).0,
            shutting_down: AtomicBool::new(false),
        }
    }

    pub fn observe(&self) -> watch::Receiver<RuntimeSnapshot> {
        self.status.subscribe()
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.status.borrow().clone()
    }

    pub async fn refresh(&self) -> RuntimeSnapshot {
        if let Ok(_guard) = self.transition.try_lock() {
            let active = self.current.read().await;
            if let Some(running) = active.as_ref() {
                let updated = running.refresh_status().await;
                let existing = self.status.borrow().clone();
                let mut combined = updated;
                for (service, value) in &mut combined.services {
                    if let Some(previous) = existing.services.get(service)
                        && value.generation == previous.generation
                        && value.active_model == previous.active_model
                        && value.last_error.is_none()
                    {
                        value.last_error.clone_from(&previous.last_error);
                    }
                }
                self.status.send_replace(combined);
            }
        }
        self.snapshot()
    }

    /// Serializes candidates. The original runtime remains active until the new one
    /// has passed installation inspection and model initialization. A failed or stale
    /// candidate is retired without changing the active runtime or microphone.
    pub(crate) async fn reconcile(
        &self,
        desired: &SpeechSettings,
        settings: &SettingsOwner,
        capture: &Mutex<SpeechSessionController>,
        stopping: &CancellationToken,
    ) -> Result<(), SpeechReconfigurationError> {
        let _transition = self.transition.lock().await;
        if self.shutting_down.load(Ordering::Acquire) || stopping.is_cancelled() {
            return Ok(());
        }
        let key = SelectionKey::from_settings(desired);
        self.status.send_modify(|snapshot| {
            for service in key.services() {
                let status = snapshot.services.entry(service).or_default();
                status.pending_model = key.plugin_id(service).map(str::to_owned);
                status.last_error = None;
                if status.active_model.is_none() {
                    status.phase = RuntimePhase::Starting;
                }
            }
        });
        let mut effective = desired.clone();
        if !effective.tts.local_auto_load {
            effective.tts.enabled = false;
        }
        let prepared = match self.installed.prepare_from_saved(&effective).await {
            Ok(value) => value,
            Err(error) => {
                self.failed(&key, &error.to_string());
                return Err(error.into());
            }
        };
        let candidate = match prepared {
            Some(plan) => match plan.activate().await {
                Ok(running) => Some(running),
                Err(error) => {
                    self.failed(&key, &error.to_string());
                    return Err(error.into());
                }
            },
            None => None,
        };
        let still_selected = || {
            settings
                .snapshot_all()
                .is_ok_and(|snapshot| SelectionKey::from_settings(&snapshot.values.speech) == key)
        };
        if stopping.is_cancelled() || !still_selected() {
            if let Some(candidate) = candidate {
                let _ = candidate.shutdown().await;
            }
            self.restore_current().await;
            return Ok(());
        }
        let mut capture = capture.lock().await;
        if stopping.is_cancelled() || !still_selected() {
            drop(capture);
            if let Some(candidate) = candidate {
                let _ = candidate.shutdown().await;
            }
            self.restore_current().await;
            return Ok(());
        }
        let mut applied = self.applied_selection.lock().await;
        if applied.asr != key.asr {
            capture.stop().await;
        }
        let mut active = self.current.write().await;
        let old = std::mem::replace(&mut *active, candidate);
        let snapshot = active.as_ref().map_or_else(
            RuntimeSnapshot::default,
            RunningAiApplication::status_snapshot,
        );
        self.status.send_replace(snapshot);
        *applied = key;
        drop(active);
        drop(applied);
        drop(capture);
        if let Some(previous) = old {
            previous.shutdown().await?;
        }
        Ok(())
    }

    /// Re-resolve persisted installation and the current model selection before
    /// recovering one failed process. Only the corresponding split worker, or
    /// the complete shared worker, is allowed to transition to a new generation.
    async fn recover_failed_worker(
        &self,
        service: AiService,
        generation: Uuid,
        settings: &SettingsOwner,
        stopping: &CancellationToken,
    ) -> Result<(), SpeechReconfigurationError> {
        let _transition = self.transition.lock().await;
        if stopping.is_cancelled() || self.shutting_down.load(Ordering::Acquire) {
            return Ok(());
        }
        let snapshot = settings.snapshot_all().map_err(|error| {
            InstalledSpeechError::Environment(format!("cannot read speech settings: {error}"))
        })?;
        let speech = snapshot.values.speech.clone();
        let key = SelectionKey::from_settings(&speech);
        if *self.applied_selection.lock().await != key || key.plugin_id(service).is_none() {
            return Ok(());
        }
        let active = self.current.read().await;
        let Some(running) = active.as_ref() else {
            return Ok(());
        };
        let selected_services: HashSet<_> = key.services().into_iter().collect();
        let mut effective = speech;
        if !effective.tts.local_auto_load {
            effective.tts.enabled = false;
        }
        let prepared = self.installed.prepare_from_saved(&effective).await?;
        let prepared = prepared.ok_or_else(|| {
            InstalledSpeechError::Environment("selected speech worker is not installed".into())
        })?;
        let planned_services: HashSet<_> = prepared
            .workers
            .iter()
            .flat_map(|worker| worker.models.iter().map(|model| model.service))
            .collect();
        if planned_services != selected_services {
            return Err(InstalledSpeechError::Environment(
                "recovery plan omits a selected speech service".into(),
            )
            .into());
        }
        let still_current = || {
            if stopping.is_cancelled() || self.shutting_down.load(Ordering::Acquire) {
                return Err(AiRuntimeError::ActivationSuperseded);
            }
            let current = settings.snapshot_all().map_err(|error| {
                AiRuntimeError::ActivationGuard(format!("cannot recheck speech selection: {error}"))
            })?;
            if SelectionKey::from_settings(&current.values.speech) != key {
                return Err(AiRuntimeError::ActivationSuperseded);
            }
            Ok(())
        };
        still_current().map_err(AiApplicationError::from)?;
        running
            .recover_failed_worker(service, generation, &prepared, &still_current)
            .await?;
        self.status.send_replace(running.status_snapshot());
        Ok(())
    }

    fn failed(&self, key: &SelectionKey, message: &str) {
        self.status
            .send_modify(|snapshot| mark_failure(snapshot, key, message));
    }

    async fn restore_current(&self) {
        let current = self.current.read().await;
        self.status.send_replace(current.as_ref().map_or_else(
            RuntimeSnapshot::default,
            RunningAiApplication::status_snapshot,
        ));
    }

    pub async fn shutdown(&self) -> Result<(), AiApplicationError> {
        self.shutting_down.store(true, Ordering::Release);
        let _transition = self.transition.lock().await;
        let previous = self.current.write().await.take();
        self.status.send_replace(RuntimeSnapshot::default());
        if let Some(previous) = previous {
            previous.shutdown().await?;
        }
        Ok(())
    }
}

fn mark_failure(snapshot: &mut RuntimeSnapshot, key: &SelectionKey, message: &str) {
    for service in key.services() {
        let status = snapshot.services.entry(service).or_default();
        status.pending_model = None;
        status.last_error = Some(message.to_owned());
        if status.active_model.is_none() {
            status.phase = RuntimePhase::Failed;
        }
    }
}

pub struct LiveSpeechVoice(pub Arc<LiveSpeechRuntime>);
impl VoiceProvider for LiveSpeechVoice {
    fn synthesize(&self, text: Arc<str>, options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async move {
            let current = self.0.current.read().await;
            let running = current
                .as_ref()
                .filter(|runtime| runtime.has_service(AiService::Tts))
                .ok_or(VoiceError::Unavailable)?;
            running.voice_provider().synthesize(text, options).await
        })
    }
}

pub struct LiveSpeechTranscriber(pub Arc<LiveSpeechRuntime>);
impl SpeechTranscriber for LiveSpeechTranscriber {
    fn transcribe<'a>(&'a self, wav: &'a [u8]) -> SpeechFuture<'a, Transcript> {
        Box::pin(async move {
            let current = self.0.current.read().await;
            let running = current
                .as_ref()
                .ok_or_else(|| SpeechSessionError::Asr("ASR runtime is inactive".into()))?;
            let recognizer = running
                .speech_recognizer()
                .ok_or_else(|| SpeechSessionError::Asr("ASR model is not selected".into()))?;
            recognizer
                .transcribe_wav(wav)
                .await
                .map_err(|error| SpeechSessionError::Asr(error.to_string()))
        })
    }
}

pub(crate) async fn watch_speech_settings(
    runtime: Arc<LiveSpeechRuntime>,
    owner: Arc<SettingsOwner>,
    capture: Arc<Mutex<SpeechSessionController>>,
    mut updates: watch::Receiver<Arc<crate::settings::SettingsSnapshot>>,
    initial: SpeechSettings,
    stop: CancellationToken,
) {
    let mut previous = SelectionKey::from_settings(&initial);
    let mut budget = RecoveryBudget::default();
    let mut poll = tokio::time::interval(RECOVERY_POLL_INTERVAL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let speech = updates.borrow_and_update().values.speech.clone();
        let next = SelectionKey::from_settings(&speech);
        if previous != next {
            previous = next;
            budget.reset();
            if let Err(error) = runtime.reconcile(&speech, &owner, &capture, &stop).await {
                tracing::warn!(error = %error, "speech worker reconfiguration rejected");
            }
        }
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            result = updates.changed() => {
                if result.is_err() { break; }
            }
            _ = poll.tick() => {
                let snapshot = runtime.refresh().await;
                let mut handled = HashSet::new();
                for service in [AiService::Tts, AiService::Asr] {
                    let state = snapshot.service(service);
                    if state.phase != RuntimePhase::Failed || !previous.services().contains(&service) {
                        continue;
                    }
                    let Some(generation) = state.generation else { continue; };
                    if !handled.insert(generation) || !budget.claim(service, generation) {
                        continue;
                    }
                    if let Err(error) = runtime.recover_failed_worker(service, generation, &owner, &stop).await {
                        tracing::warn!(service = ?service, generation = %generation, error = %error, "speech worker recovery attempt rejected");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_budget_is_bounded_per_failed_generation_and_resets_on_reselection() {
        let mut budget = RecoveryBudget::default();
        let first = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        assert!(budget.claim(AiService::Tts, first));
        assert!(budget.claim(AiService::Tts, first));
        assert!(!budget.claim(AiService::Tts, first));
        assert!(budget.claim(AiService::Asr, first));
        assert!(budget.claim(AiService::Tts, replacement));
        assert!(budget.claim(AiService::Tts, replacement));
        assert!(!budget.claim(AiService::Tts, replacement));
        budget.reset();
        assert!(budget.claim(AiService::Tts, replacement));
    }

    #[test]
    fn selection_key_ignores_capture_and_non_selected_model_options() {
        let mut speech = SpeechSettings::default();
        speech.asr.recognizer_id = "whisper".into();
        let initial = SelectionKey::from_settings(&speech);
        speech.asr.device_id = "another mic".into();
        speech.asr.vad_threshold_milli = 800;
        speech
            .asr_model_options
            .insert("gigaam".into(), serde_json::json!({"x": 7}));
        assert_eq!(initial, SelectionKey::from_settings(&speech));
        speech
            .asr
            .model_languages
            .insert("whisper".into(), "en".into());
        assert_ne!(initial, SelectionKey::from_settings(&speech));
    }

    #[test]
    fn local_tts_not_autoloaded_is_excluded_from_activation() {
        let mut settings = SpeechSettings::default();
        settings.tts.enabled = true;
        settings.tts.backend = TtsBackend::Local;
        settings.tts.local_model_id = "mock_tts".into();
        assert!(SelectionKey::from_settings(&settings).tts.is_none());
        settings.tts.local_auto_load = true;
        assert_eq!(
            SelectionKey::from_settings(&settings).tts.unwrap().0,
            "mock_tts"
        );
    }

    #[tokio::test]
    async fn incomplete_install_fails_without_creating_a_runtime() {
        use crate::settings::{
            AppSettings, MemorySettingsRepository, SettingsConfig, SettingsLifecycle,
            SettingsSource,
        };

        let mut saved = AppSettings::initial_profile();
        saved.speech.asr.recognizer_id = "missing_asr".into();
        let owner = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(saved.clone()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap()
        .owner();
        let environment = InstalledSpeechEnvironment::for_desktop(
            std::env::temp_dir().join(format!("neuromita-absent-{}", uuid::Uuid::new_v4())),
        );
        let runtime = LiveSpeechRuntime::new(environment, None, &saved.speech, None);
        let capture = Mutex::new(SpeechSessionController::default());
        let failure = runtime
            .reconcile(&saved.speech, &owner, &capture, &CancellationToken::new())
            .await;
        assert!(failure.is_err());
        let state = runtime.snapshot().service(AiService::Asr);
        assert_eq!(state.phase, RuntimePhase::Failed);
        assert!(state.generation.is_none());
        assert!(state.active_model.is_none());
        assert!(state.pending_model.is_none());
        assert!(state.last_error.is_some());
        assert_eq!(
            capture.lock().await.snapshot().phase,
            crate::audio::controller::SpeechSessionPhase::Stopped
        );
    }

    #[tokio::test]
    async fn removing_a_selection_requires_no_python_and_stops_cleanly() {
        use crate::settings::{
            AppSettings, MemorySettingsRepository, SettingsConfig, SettingsLifecycle,
            SettingsSource,
        };
        let saved = AppSettings::initial_profile();
        let owner = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(saved.clone()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap()
        .owner();
        let environment = InstalledSpeechEnvironment::for_desktop(
            std::env::temp_dir().join(format!("neuromita-absent-{}", uuid::Uuid::new_v4())),
        );
        let runtime = LiveSpeechRuntime::new(environment, None, &saved.speech, None);
        let capture = Mutex::new(SpeechSessionController::default());
        runtime
            .reconcile(&saved.speech, &owner, &capture, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            runtime.snapshot().service(AiService::Asr).phase,
            RuntimePhase::Stopped
        );
        assert_eq!(
            runtime.snapshot().service(AiService::Tts).phase,
            RuntimePhase::Stopped
        );
    }

    #[tokio::test]
    async fn settings_change_before_watcher_poll_is_not_lost() {
        use crate::settings::{
            AppSettings, MemorySettingsRepository, SettingsConfig, SettingsLifecycle,
            SettingsSource,
        };
        let saved = AppSettings::initial_profile();
        let owner = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(saved.clone()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap()
        .owner();
        let updates = owner.subscribe();
        owner
            .edit::<SpeechSettings>(|speech| speech.asr.recognizer_id = "missing_asr".into())
            .unwrap();
        let environment = InstalledSpeechEnvironment::for_desktop(
            std::env::temp_dir().join(format!("neuromita-absent-{}", uuid::Uuid::new_v4())),
        );
        let runtime = Arc::new(LiveSpeechRuntime::new(
            environment,
            None,
            &saved.speech,
            None,
        ));
        let mut observed = runtime.observe();
        let stop = CancellationToken::new();
        let task = tokio::spawn(watch_speech_settings(
            Arc::clone(&runtime),
            owner,
            Arc::new(Mutex::new(SpeechSessionController::default())),
            updates,
            saved.speech,
            stop.clone(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if observed.borrow_and_update().service(AiService::Asr).phase
                    == RuntimePhase::Failed
                {
                    return;
                }
                observed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        stop.cancel();
        task.await.unwrap();
        assert!(
            runtime
                .snapshot()
                .service(AiService::Asr)
                .active_model
                .is_none()
        );
    }

    #[test]
    fn failures_do_not_evict_previous_ready_model() {
        let mut snapshot = RuntimeSnapshot::default();
        let status = snapshot.services.entry(AiService::Asr).or_default();
        status.phase = RuntimePhase::ModelReady;
        status.active_model = Some(super::super::runtime_status::LoadedModel {
            plugin_id: "old".into(),
            asset_revision: None,
        });
        let mut settings = SpeechSettings::default();
        settings.asr.recognizer_id = "new".into();
        mark_failure(
            &mut snapshot,
            &SelectionKey::from_settings(&settings),
            "missing backend",
        );
        let updated = snapshot.service(AiService::Asr);
        assert_eq!(updated.phase, RuntimePhase::ModelReady);
        assert_eq!(updated.active_model.unwrap().plugin_id, "old");
        assert_eq!(updated.last_error.as_deref(), Some("missing backend"));
    }
}
