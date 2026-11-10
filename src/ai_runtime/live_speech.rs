//! Application-owned, read-only reconfiguration of already installed speech workers.
//! A candidate must initialize before it can replace an active runtime.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
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
    runtime_status::{RecoveryAttempts, RuntimePhase, RuntimeSnapshot},
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

/// Controls automatic speech retry admission without changing Python ownership.
/// A zero backoff still limits retries to one per poll interval.
#[derive(Debug, Clone, Copy)]
pub struct RecoveryPolicy {
    poll_interval: Duration,
    retry_backoff: Duration,
    max_attempts: u8,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            poll_interval: RECOVERY_POLL_INTERVAL,
            retry_backoff: Duration::ZERO,
            max_attempts: MAX_RECOVERY_ATTEMPTS_PER_GENERATION,
        }
    }
}

impl RecoveryPolicy {
    pub fn new(
        poll_interval: Duration,
        retry_backoff: Duration,
        max_attempts: u8,
    ) -> Result<Self, &'static str> {
        if poll_interval.is_zero() || max_attempts == 0 || retry_backoff > Duration::from_secs(300)
        {
            return Err(
                "recovery poll must be nonzero, max attempts positive and backoff at most 300 seconds",
            );
        }
        Ok(Self {
            poll_interval,
            retry_backoff,
            max_attempts,
        })
    }
}

struct RecoveryBudget {
    observed: HashMap<Uuid, u8>,
    last_attempt: HashMap<Uuid, tokio::time::Instant>,
    policy: RecoveryPolicy,
}

impl Default for RecoveryBudget {
    fn default() -> Self {
        Self::with_policy(RecoveryPolicy::default())
    }
}

impl RecoveryBudget {
    fn with_policy(policy: RecoveryPolicy) -> Self {
        Self {
            observed: HashMap::new(),
            last_attempt: HashMap::new(),
            policy,
        }
    }

    fn claim(&mut self, generation: Uuid) -> Option<RecoveryAttempts> {
        let now = tokio::time::Instant::now();
        let attempted = self.observed.entry(generation).or_default();
        if *attempted >= self.policy.max_attempts
            || self
                .last_attempt
                .get(&generation)
                .is_some_and(|last| now.duration_since(*last) < self.policy.retry_backoff)
        {
            return None;
        }
        *attempted += 1;
        self.last_attempt.insert(generation, now);
        Some(RecoveryAttempts {
            generation,
            attempted: *attempted,
            maximum: self.policy.max_attempts,
            exhausted: *attempted == self.policy.max_attempts,
        })
    }

    fn retain_failed_generations(&mut self, snapshot: &RuntimeSnapshot) {
        let failed: HashSet<_> = snapshot
            .services
            .values()
            .filter(|status| status.phase == RuntimePhase::Failed)
            .filter_map(|status| status.generation)
            .collect();
        self.observed
            .retain(|generation, _| failed.contains(generation));
        self.last_attempt
            .retain(|generation, _| failed.contains(generation));
    }

    fn reset(&mut self) {
        self.observed.clear();
        self.last_attempt.clear();
    }
}

pub struct LiveSpeechRuntime {
    installed: InstalledSpeechEnvironment,
    current: RwLock<Option<RunningAiApplication>>,
    transition: Mutex<()>,
    applied_selection: Mutex<SelectionKey>,
    status: watch::Sender<RuntimeSnapshot>,
    shutting_down: AtomicBool,
    recovery_policy: RecoveryPolicy,
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
            recovery_policy: RecoveryPolicy::default(),
        }
    }

    pub fn new_with_recovery_policy(
        installed: InstalledSpeechEnvironment,
        initial: Option<RunningAiApplication>,
        settings: &SpeechSettings,
        startup_error: Option<&str>,
        policy: RecoveryPolicy,
    ) -> Self {
        let mut runtime = Self::new(installed, initial, settings, startup_error);
        runtime.recovery_policy = policy;
        runtime
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
                for (service, value) in &mut combined.services {
                    if let Some(previous) = existing.services.get(service)
                        && value.phase == RuntimePhase::Failed
                        && previous.generation == value.generation
                    {
                        value.recovery.clone_from(&previous.recovery);
                    }
                }
                self.status.send_if_modified(|current| {
                    if *current == combined {
                        false
                    } else {
                        *current = combined;
                        true
                    }
                });
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

    fn report_recovery_attempt(&self, attempt: &RecoveryAttempts) {
        self.status.send_if_modified(|snapshot| {
            let mut changed = false;
            for state in snapshot.services.values_mut() {
                if state.phase == RuntimePhase::Failed
                    && state.generation == Some(attempt.generation)
                    && state.recovery.as_ref() != Some(attempt)
                {
                    state.recovery = Some(attempt.clone());
                    changed = true;
                }
            }
            changed
        });
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

enum RecoveryWait<T> {
    Completed(T),
    SelectionChanged,
    Stopped,
}

async fn wait_for_recovery<F: Future>(
    recovery: F,
    updates: &mut watch::Receiver<Arc<crate::settings::SettingsSnapshot>>,
    selected: &SelectionKey,
    stop: &CancellationToken,
) -> RecoveryWait<F::Output> {
    tokio::pin!(recovery);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return RecoveryWait::Stopped,
            changed = updates.changed() => {
                if changed.is_err() { return RecoveryWait::Stopped; }
                let new_key = SelectionKey::from_settings(&updates.borrow_and_update().values.speech);
                if &new_key != selected { return RecoveryWait::SelectionChanged; }
            },
            result = &mut recovery => return RecoveryWait::Completed(result),
        }
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
    let mut budget = RecoveryBudget::with_policy(runtime.recovery_policy);
    let mut poll = tokio::time::interval(runtime.recovery_policy.poll_interval);
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
                budget.retain_failed_generations(&snapshot);
                let mut handled = HashSet::new();
                for service in [AiService::Tts, AiService::Asr] {
                    let state = snapshot.service(service);
                    if state.phase != RuntimePhase::Failed || !previous.services().contains(&service) {
                        continue;
                    }
                    let Some(generation) = state.generation else { continue; };
                    if !handled.insert(generation) {
                        continue;
                    }
                    let Some(attempt) = budget.claim(generation) else { continue; };
                    let result = match wait_for_recovery(
                        runtime.recover_failed_worker(service, generation, &owner, &stop),
                        &mut updates,
                        &previous,
                        &stop,
                    ).await {
                        RecoveryWait::Completed(result) => result,
                        RecoveryWait::SelectionChanged => break,
                        RecoveryWait::Stopped => return,
                    };
                    if let Err(error) = result {
                        tracing::warn!(service = ?service, generation = %generation, attempted = attempt.attempted, exhausted = attempt.exhausted, error = %error, "speech worker recovery attempt rejected");
                    }
                    runtime.refresh().await;
                    runtime.report_recovery_attempt(&attempt);
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
        let first_attempt = budget.claim(first).unwrap();
        assert_eq!(first_attempt.attempted, 1);
        assert!(!first_attempt.exhausted);
        let final_attempt = budget.claim(first).unwrap();
        assert_eq!(final_attempt.attempted, 2);
        assert!(final_attempt.exhausted);
        assert!(budget.claim(first).is_none());
        assert_eq!(budget.claim(replacement).unwrap().attempted, 1);
        assert!(budget.claim(replacement).unwrap().exhausted);
        assert!(budget.claim(replacement).is_none());
        budget.reset();
        assert_eq!(budget.claim(replacement).unwrap().attempted, 1);
    }

    #[tokio::test]
    async fn recovery_policy_caps_attempts_and_enforces_real_backoff_per_generation() {
        assert!(RecoveryPolicy::new(Duration::ZERO, Duration::ZERO, 2).is_err());
        assert!(RecoveryPolicy::new(Duration::from_secs(1), Duration::ZERO, 0).is_err());
        assert!(RecoveryPolicy::new(Duration::from_secs(1), Duration::from_secs(301), 2).is_err());
        let policy =
            RecoveryPolicy::new(Duration::from_millis(5), Duration::from_millis(30), 2).unwrap();
        let mut budget = RecoveryBudget::with_policy(policy);
        let generation = Uuid::new_v4();
        let first = budget.claim(generation).unwrap();
        assert_eq!(
            (first.attempted, first.maximum, first.exhausted),
            (1, 2, false)
        );
        assert!(budget.claim(generation).is_none());
        tokio::time::sleep(Duration::from_millis(35)).await;
        let second = budget.claim(generation).unwrap();
        assert_eq!(
            (second.attempted, second.maximum, second.exhausted),
            (2, 2, true)
        );
        tokio::time::sleep(Duration::from_millis(35)).await;
        assert!(budget.claim(generation).is_none());
        budget.reset();
        assert_eq!(budget.claim(generation).unwrap().attempted, 1);
    }

    #[test]
    fn recovery_budget_retires_generations_after_success_or_stop() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut budget = RecoveryBudget::default();
        budget.claim(first).unwrap();
        budget.claim(second).unwrap();
        let mut snapshot = RuntimeSnapshot::default();
        let failed = snapshot.services.entry(AiService::Tts).or_default();
        failed.generation = Some(second);
        failed.phase = RuntimePhase::Failed;
        budget.retain_failed_generations(&snapshot);
        assert!(!budget.observed.contains_key(&first));
        assert_eq!(budget.observed.get(&second), Some(&1));
        snapshot.services.clear();
        budget.retain_failed_generations(&snapshot);
        assert!(budget.observed.is_empty());
    }

    #[test]
    fn exhaustion_is_observable_for_every_service_on_a_shared_generation() {
        let settings = SpeechSettings::default();
        let runtime = LiveSpeechRuntime::new(
            InstalledSpeechEnvironment::for_desktop(std::env::temp_dir()),
            None,
            &settings,
            None,
        );
        let generation = Uuid::new_v4();
        runtime.status.send_modify(|snapshot| {
            for service in [AiService::Tts, AiService::Asr] {
                let state = snapshot.services.entry(service).or_default();
                state.phase = RuntimePhase::Failed;
                state.generation = Some(generation);
                state.last_error = Some("original Python worker exit".into());
            }
        });
        let mut observed = runtime.observe();
        let attempt = RecoveryAttempts {
            generation,
            attempted: 2,
            maximum: MAX_RECOVERY_ATTEMPTS_PER_GENERATION,
            exhausted: true,
        };
        runtime.report_recovery_attempt(&attempt);
        assert!(observed.has_changed().unwrap());
        let snapshot = runtime.snapshot();
        for service in [AiService::Tts, AiService::Asr] {
            let status = snapshot.service(service);
            assert_eq!(
                status.last_error.as_deref(),
                Some("original Python worker exit")
            );
            assert_eq!(status.recovery, Some(attempt.clone()));
        }
        observed.borrow_and_update();
        runtime.report_recovery_attempt(&attempt);
        assert!(!observed.has_changed().unwrap());
    }

    #[tokio::test]
    async fn cancelled_recovery_drops_in_flight_candidate_without_waiting_for_initialize() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::sync::Notify;

        struct DropGuard(Arc<AtomicBool>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let settings = crate::settings::AppSettings::initial_profile();
        let owner = crate::settings::SettingsLifecycle::prepare(crate::settings::SettingsConfig {
            source: crate::settings::SettingsSource::initial(settings),
            repository: Arc::new(crate::settings::MemorySettingsRepository::new()),
        })
        .await
        .unwrap()
        .owner();
        let mut updates = owner.subscribe();
        let stop = CancellationToken::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(Notify::new());
        let blocked = async {
            let _guard = DropGuard(Arc::clone(&dropped));
            entered.notify_one();
            std::future::pending::<()>().await
        };
        let selected = SelectionKey::from_settings(&owner.snapshot_all().unwrap().values.speech);
        let pending = wait_for_recovery(blocked, &mut updates, &selected, &stop);
        tokio::pin!(pending);
        tokio::select! {
            _ = entered.notified() => {
                stop.cancel();
                assert!(matches!(pending.await, RecoveryWait::Stopped));
            },
            result = &mut pending => panic!("blocked candidate completed before cancellation: {}", match result { RecoveryWait::Stopped => "stopped", RecoveryWait::SelectionChanged => "changed", RecoveryWait::Completed(_) => "completed" }),
        }
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn changed_settings_drop_in_flight_recovery_candidate() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::sync::Notify;
        struct DropGuard(Arc<AtomicBool>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let saved = crate::settings::AppSettings::initial_profile();
        let owner = crate::settings::SettingsLifecycle::prepare(crate::settings::SettingsConfig {
            source: crate::settings::SettingsSource::initial(saved),
            repository: Arc::new(crate::settings::MemorySettingsRepository::new()),
        })
        .await
        .unwrap()
        .owner();
        let mut updates = owner.subscribe();
        updates.borrow_and_update();
        let stop = CancellationToken::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(Notify::new());
        let blocked = async {
            let _guard = DropGuard(Arc::clone(&dropped));
            entered.notify_one();
            std::future::pending::<()>().await
        };
        let selected = SelectionKey::from_settings(&owner.snapshot_all().unwrap().values.speech);
        let pending = wait_for_recovery(blocked, &mut updates, &selected, &stop);
        tokio::pin!(pending);
        tokio::select! {
            _ = entered.notified() => {
                owner.edit::<SpeechSettings>(|settings| settings.asr.recognizer_id = "mock_asr".into()).unwrap();
                assert!(matches!(pending.await, RecoveryWait::SelectionChanged));
            },
            _ = &mut pending => panic!("blocked candidate completed before settings changed"),
        }
        assert!(dropped.load(Ordering::Acquire));
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

    #[cfg(unix)]
    #[test]
    fn installed_speech_watcher_recovers_mock_worker_from_saved_settings() {
        std::thread::Builder::new()
            .name("installed-speech-recovery".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(installed_speech_watcher_recovery_fixture());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[cfg(unix)]
    async fn installed_speech_watcher_recovery_fixture() {
        use crate::ai_runtime::{
            backend::{BackendKind, LockedBackend},
            overlay::LockedOverlay,
        };
        use crate::settings::{
            AppSettings, MemorySettingsRepository, SettingsConfig, SettingsLifecycle,
            SettingsSource,
        };
        use std::os::unix::fs::PermissionsExt;

        let python = std::env::var_os("NEUROMITA_TEST_PYTHON")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("PATH")?
                    .as_os_str()
                    .to_str()
                    .and_then(|value| {
                        value
                            .split(':')
                            .map(|part| std::path::PathBuf::from(part).join("python3"))
                            .find(|path| path.is_file())
                    })
            })
            .expect("test requires a Python executable");
        let python = std::fs::canonicalize(python).unwrap();
        let output = tokio::process::Command::new(&python)
            .arg("--version")
            .output()
            .await
            .unwrap();
        let version_line = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        let version = String::from_utf8_lossy(version_line)
            .trim()
            .strip_prefix("Python ")
            .unwrap()
            .to_owned();
        let root = std::env::temp_dir().join(format!("nm-installed-recovery-{}", Uuid::new_v4()));
        let data = root.join("AIData");
        let python_path = data.join("bootstrap/python/fixture/bin/python3");
        let uv_path = data.join("bootstrap/uv/fixture/uv");
        let plugin_path = data.join("plugins/builtin/mock_tts");
        for path in [&python_path, &uv_path] {
            tokio::fs::create_dir_all(path.parent().unwrap())
                .await
                .unwrap();
        }
        tokio::fs::create_dir_all(&plugin_path).await.unwrap();
        let uv_script = "#!/bin/sh\n[ \"$1\" = \"python\" ] && [ \"$2\" = \"find\" ] || exit 3\nprintf '%s\\n' \"$UV_PYTHON_INSTALL_DIR/fixture/bin/python3\"\n";
        tokio::fs::write(&uv_path, uv_script).await.unwrap();
        tokio::fs::set_permissions(&uv_path, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        tokio::fs::write(
            &python_path,
            format!("#!/bin/sh\nexec '{}' \"$@\"\n", python.display()),
        )
        .await
        .unwrap();
        tokio::fs::set_permissions(&python_path, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        let sdk = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        tokio::fs::copy(
            sdk.join("plugins/builtin/mock_tts/plugin.json"),
            plugin_path.join("plugin.json"),
        )
        .await
        .unwrap();
        let code = tokio::fs::read_to_string(sdk.join("plugins/builtin/mock_tts/plugin.py"))
            .await.unwrap()
            .replace("    def infer(self, input_data):\n", "    def infer(self, input_data):\n        if input_data.get('text') == 'CRASH':\n            import os\n            os._exit(43)\n");
        tokio::fs::write(plugin_path.join("plugin.py"), code)
            .await
            .unwrap();
        let backend = LockedBackend {
            kind: BackendKind::RemoteApi,
            revision: "fixture".into(),
            python_version: version.clone(),
            packages: vec![],
        };
        let backend_path = data.join("backends/remote-api/fixture");
        tokio::fs::create_dir_all(&backend_path).await.unwrap();
        tokio::fs::write(
            backend_path.join(".neuromita-backend.json"),
            serde_json::to_vec(&backend).unwrap(),
        )
        .await
        .unwrap();
        let overlay = LockedOverlay {
            plugin_id: "mock_tts".into(),
            revision: "fixture".into(),
            python_version: version,
            packages: vec![],
            actions: vec![],
        };
        let overlay_path = data.join("overlays/mock_tts/fixture");
        tokio::fs::create_dir_all(&overlay_path).await.unwrap();
        tokio::fs::write(
            overlay_path.join(".neuromita-overlay.json"),
            serde_json::to_vec(&overlay).unwrap(),
        )
        .await
        .unwrap();
        let mut saved = AppSettings::initial_profile();
        saved.speech.tts.enabled = true;
        saved.speech.tts.backend = TtsBackend::Local;
        saved.speech.tts.local_auto_load = true;
        saved.speech.tts.local_model_id = "mock_tts".into();
        saved.speech.asr.recognizer_id.clear();
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(saved.clone()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        crate::ai_runtime::sdk::SdkPublisher::new(root.join("AIData"))
            .publish_embedded()
            .unwrap();
        let installed = InstalledSpeechEnvironment::for_desktop(root.clone());
        let plan = installed
            .prepare_from_saved(&saved.speech)
            .await
            .unwrap()
            .unwrap();
        let running = plan.activate().await.unwrap();
        let original = running
            .status_snapshot()
            .service(AiService::Tts)
            .generation
            .unwrap();
        let runtime = Arc::new(LiveSpeechRuntime::new(
            installed,
            Some(running),
            &saved.speech,
            None,
        ));
        let current = runtime.current.read().await;
        let active = current.as_ref().unwrap();
        assert!(
            active
                .supervisor()
                .call(
                    AiService::Tts,
                    "infer",
                    serde_json::json!({"kind":"tts", "input":{"text":"CRASH"}})
                )
                .await
                .is_err()
        );
        drop(current);
        let stop = CancellationToken::new();
        let mut updates = runtime.observe();
        let task = tokio::spawn(watch_speech_settings(
            Arc::clone(&runtime),
            Arc::clone(&owner),
            Arc::new(Mutex::new(SpeechSessionController::default())),
            owner.subscribe(),
            saved.speech,
            stop.clone(),
        ));
        let replacement = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let status = updates.borrow_and_update().service(AiService::Tts);
                if status.phase == RuntimePhase::ModelReady && status.generation != Some(original) {
                    assert!(status.active_model.is_some());
                    assert!(status.recovery.is_none());
                    return status.generation.unwrap();
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_ne!(original, replacement);
        assert_eq!(
            owner
                .snapshot_all()
                .unwrap()
                .values
                .speech
                .tts
                .local_model_id,
            "mock_tts"
        );
        stop.cancel();
        task.await.unwrap();
        Arc::try_unwrap(runtime)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
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
