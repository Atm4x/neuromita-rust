//! Versioned, bounded native speech IPC data contract.
//! A signed manifest declares capabilities; a ready process independently attests them.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ai_runtime::speech::Transcript,
    voice::{AudioArtifact, AudioFormat},
};
use std::sync::Arc;

pub const CONTRACT_VERSION: u32 = 1;
const MAX_WAV_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSpeechKind {
    Tts,
    Asr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSpeechCapability {
    pub kind: NativeSpeechKind,
    pub sample_rate_hz: u32,
    pub languages: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NativeSpeechError {
    #[error("native model does not advertise the selected speech service or language")]
    Capability,
    #[error("native speech request or response violates the bounded PCM WAV contract")]
    Protocol,
}

impl NativeSpeechCapability {
    pub fn validate(&self) -> Result<(), NativeSpeechError> {
        if !(8_000..=96_000).contains(&self.sample_rate_hz)
            || self.languages.is_empty()
            || self.languages.len() > 32
            || self.languages.iter().any(|language| {
                language.is_empty()
                    || language.len() > 32
                    || !language
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            })
        {
            return Err(NativeSpeechError::Protocol);
        }
        let mut unique = std::collections::HashSet::new();
        if self
            .languages
            .iter()
            .any(|language| !unique.insert(language.as_str()))
        {
            return Err(NativeSpeechError::Protocol);
        }
        Ok(())
    }

    pub fn accepts(&self, language: &str) -> bool {
        self.languages
            .iter()
            .any(|supported| supported == language || supported == "auto")
    }
}

pub fn capability<'a>(
    capabilities: &'a [NativeSpeechCapability],
    kind: NativeSpeechKind,
    language: &str,
) -> Result<&'a NativeSpeechCapability, NativeSpeechError> {
    capabilities
        .iter()
        .find(|candidate| candidate.kind == kind && candidate.accepts(language))
        .ok_or(NativeSpeechError::Capability)
}

fn valid_wav(wav: &[u8], sample_rate_hz: u32) -> bool {
    if wav.len() < 44
        || wav.len() > MAX_WAV_BYTES
        || &wav[0..4] != b"RIFF"
        || &wav[8..12] != b"WAVE"
        || &wav[12..16] != b"fmt "
        || u32::from_le_bytes(wav[16..20].try_into().unwrap()) != 16
        || u16::from_le_bytes(wav[20..22].try_into().unwrap()) != 1
        || !(1..=2).contains(&u16::from_le_bytes(wav[22..24].try_into().unwrap()))
        || u32::from_le_bytes(wav[24..28].try_into().unwrap()) != sample_rate_hz
        || u16::from_le_bytes(wav[34..36].try_into().unwrap()) != 16
        || &wav[36..40] != b"data"
    {
        return false;
    }
    let declared_size = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
    let data_size = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
    declared_size.checked_add(8) == Some(wav.len())
        && data_size.checked_add(44) == Some(wav.len())
        && data_size > 0
        && data_size % (u16::from_le_bytes(wav[22..24].try_into().unwrap()) as usize * 2) == 0
}

pub fn encode_tts(
    capabilities: &[NativeSpeechCapability],
    text: &str,
    voice: &str,
    language: &str,
    volume_percent: u8,
) -> Result<Value, NativeSpeechError> {
    capability(capabilities, NativeSpeechKind::Tts, language)?;
    if text.is_empty()
        || text.len() > MAX_TEXT_BYTES
        || text.contains('\0')
        || voice.len() > 512
        || voice.contains('\0')
        || volume_percent > 100
    {
        return Err(NativeSpeechError::Protocol);
    }
    Ok(
        json!({"speech_version": CONTRACT_VERSION, "kind": "tts", "input": {
            "text": text, "voice": voice, "language": language, "volume_percent": volume_percent,
        }}),
    )
}

pub fn decode_tts(
    capabilities: &[NativeSpeechCapability],
    language: &str,
    value: Value,
) -> Result<AudioArtifact, NativeSpeechError> {
    let selected = capability(capabilities, NativeSpeechKind::Tts, language)?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Response {
        speech_version: u32,
        kind: NativeSpeechKind,
        audio_format: String,
        sample_rate_hz: u32,
        audio_base64: String,
    }
    let response: Response =
        serde_json::from_value(value).map_err(|_| NativeSpeechError::Protocol)?;
    if response.speech_version != CONTRACT_VERSION
        || response.kind != NativeSpeechKind::Tts
        || response.audio_format != "wav"
        || response.sample_rate_hz != selected.sample_rate_hz
        || response.audio_base64.len() > (MAX_WAV_BYTES * 4 / 3 + 8)
    {
        return Err(NativeSpeechError::Protocol);
    }
    let bytes = STANDARD
        .decode(response.audio_base64)
        .map_err(|_| NativeSpeechError::Protocol)?;
    if !valid_wav(&bytes, response.sample_rate_hz) {
        return Err(NativeSpeechError::Protocol);
    }
    Ok(AudioArtifact {
        format: AudioFormat::Wave,
        bytes: Arc::from(bytes),
    })
}

pub fn encode_asr(
    capabilities: &[NativeSpeechCapability],
    wav: &[u8],
    language: &str,
) -> Result<Value, NativeSpeechError> {
    let selected = capability(capabilities, NativeSpeechKind::Asr, language)?;
    if !valid_wav(wav, selected.sample_rate_hz) {
        return Err(NativeSpeechError::Protocol);
    }
    Ok(
        json!({"speech_version": CONTRACT_VERSION, "kind": "asr", "input": {
            "audio_format": "wav", "audio_base64": STANDARD.encode(wav), "sample_rate_hz": selected.sample_rate_hz,
            "language": language,
        }}),
    )
}

pub fn decode_asr(value: Value) -> Result<Transcript, NativeSpeechError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Response {
        speech_version: u32,
        kind: NativeSpeechKind,
        text: String,
        language: Option<String>,
    }
    let response: Response =
        serde_json::from_value(value).map_err(|_| NativeSpeechError::Protocol)?;
    if response.speech_version != CONTRACT_VERSION
        || response.kind != NativeSpeechKind::Asr
        || response.text.len() > 128 * 1024
        || response.text.contains('\0')
        || response.language.as_ref().is_some_and(|language| {
            language.len() > 32
                || !language
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
    {
        return Err(NativeSpeechError::Protocol);
    }
    Ok(Transcript {
        text: response.text,
        language: response.language,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(kind: NativeSpeechKind) -> Vec<NativeSpeechCapability> {
        vec![NativeSpeechCapability {
            kind,
            sample_rate_hz: 16_000,
            languages: vec!["ru".into()],
        }]
    }

    fn wav() -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&38u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&32_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn contracts_admit_only_declared_service_language_and_bounded_audio() {
        let tts = cap(NativeSpeechKind::Tts);
        assert!(encode_tts(&tts, "hello", "voice", "ru", 80).is_ok());
        assert!(encode_tts(&tts, "hello", "voice", "en", 80).is_err());
        assert!(encode_asr(&tts, &wav(), "ru").is_err());
        let mut corrupted = wav();
        corrupted[24..28].copy_from_slice(&44_100u32.to_le_bytes());
        assert!(encode_asr(&cap(NativeSpeechKind::Asr), &corrupted, "ru").is_err());
        assert!(encode_asr(&cap(NativeSpeechKind::Asr), &wav(), "ru").is_ok());
        let value = json!({"speech_version": 1, "kind": "tts", "audio_format": "wav", "sample_rate_hz": 16_000,
            "audio_base64": STANDARD.encode(wav())});
        assert_eq!(
            decode_tts(&tts, "ru", value.clone()).unwrap().bytes.len(),
            46
        );
        assert!(decode_tts(&tts, "en", value).is_err());
        assert!(
            decode_asr(json!({"speech_version":1, "kind":"asr", "text":"тест", "language":"ru"}))
                .is_ok()
        );
        assert!(
            decode_asr(json!({"speech_version":1, "kind":"tts", "text":"test", "language":null}))
                .is_err()
        );
    }
}

use std::collections::HashMap;

use super::{
    native_service::AiHubNativeService,
    service_workers::{ServiceInferencePermit, ServiceModel, ServiceWorkerRegistry},
};
use crate::{
    audio::session::{SpeechFuture, SpeechSessionError, SpeechTranscriber},
    settings::{
        SettingsOwner, SpeechModelReference, SpeechSettings, TtsBackend, WorkerImplementation,
    },
    voice::{VoiceError, VoiceFuture, VoiceOptions, VoiceProvider},
};

/// The per-service transport contract, independent of its process implementation.
/// Only the selected model identity is passed to a worker; the worker never chooses
/// an alternative implementation after an error.
trait TtsWorker: Send + Sync {
    fn synthesize<'a>(
        &'a self,
        model: &'a SpeechModelReference,
        speech: &'a SpeechSettings,
        permit: &'a ServiceInferencePermit<'_>,
        text: Arc<str>,
        options: VoiceOptions,
    ) -> VoiceFuture<'a>;
}

trait AsrWorker: Send + Sync {
    fn transcribe<'a>(
        &'a self,
        model: &'a SpeechModelReference,
        speech: &'a SpeechSettings,
        permit: &'a ServiceInferencePermit<'_>,
        wav: &'a [u8],
    ) -> SpeechFuture<'a, Transcript>;
}

struct PythonTtsWorker(Arc<dyn VoiceProvider>);
struct RustTtsWorker(Arc<AiHubNativeService>);
struct PythonAsrWorker(Arc<dyn SpeechTranscriber>);
struct RustAsrWorker(Arc<AiHubNativeService>);

impl TtsWorker for PythonTtsWorker {
    fn synthesize<'a>(
        &'a self,
        model: &'a SpeechModelReference,
        speech: &'a SpeechSettings,
        _permit: &'a ServiceInferencePermit<'_>,
        text: Arc<str>,
        options: VoiceOptions,
    ) -> VoiceFuture<'a> {
        Box::pin(async move {
            if speech.tts.backend == TtsBackend::Local
                && model.model_id != speech.tts.local_model_id
            {
                return Err(VoiceError::Unavailable);
            }
            self.0.synthesize(text, options).await
        })
    }
}

impl TtsWorker for RustTtsWorker {
    fn synthesize<'a>(
        &'a self,
        model: &'a SpeechModelReference,
        speech: &'a SpeechSettings,
        permit: &'a ServiceInferencePermit<'_>,
        text: Arc<str>,
        options: VoiceOptions,
    ) -> VoiceFuture<'a> {
        Box::pin(async move {
            if speech.tts.backend != TtsBackend::Local || !speech.tts.enabled {
                return Err(VoiceError::Unavailable);
            }
            let revision = model.revision.as_deref().ok_or(VoiceError::Unavailable)?;
            self.0
                .synthesize_speech(
                    permit,
                    &model.model_id,
                    revision,
                    &text,
                    &options.voice,
                    &speech.tts.local_language,
                    options.volume.get(),
                )
                .await
                .map_err(VoiceError::Generation)
        })
    }
}

impl AsrWorker for PythonAsrWorker {
    fn transcribe<'a>(
        &'a self,
        model: &'a SpeechModelReference,
        speech: &'a SpeechSettings,
        _permit: &'a ServiceInferencePermit<'_>,
        wav: &'a [u8],
    ) -> SpeechFuture<'a, Transcript> {
        Box::pin(async move {
            if model.model_id != speech.asr.recognizer_id {
                return Err(SpeechSessionError::Asr(
                    "selected ASR model differs from the loaded Python plugin".into(),
                ));
            }
            self.0.transcribe(wav).await
        })
    }
}

impl AsrWorker for RustAsrWorker {
    fn transcribe<'a>(
        &'a self,
        model: &'a SpeechModelReference,
        speech: &'a SpeechSettings,
        permit: &'a ServiceInferencePermit<'_>,
        wav: &'a [u8],
    ) -> SpeechFuture<'a, Transcript> {
        Box::pin(async move {
            let revision = model.revision.as_deref().ok_or_else(|| {
                SpeechSessionError::Asr("native model revision is missing".into())
            })?;
            let language = speech
                .asr
                .model_languages
                .get(&model.model_id)
                .map_or("auto", String::as_str);
            self.0
                .transcribe_speech(permit, &model.model_id, revision, wav, language)
                .await
                .map_err(SpeechSessionError::Asr)
        })
    }
}

pub(crate) struct RoutedSpeechVoice {
    pub settings: Arc<SettingsOwner>,
    registry: Arc<ServiceWorkerRegistry>,
    implementations: HashMap<WorkerImplementation, Arc<dyn TtsWorker>>,
}

impl RoutedSpeechVoice {
    pub fn new(
        settings: Arc<SettingsOwner>,
        python: Arc<dyn VoiceProvider>,
        native: Option<Arc<AiHubNativeService>>,
        registry: Arc<ServiceWorkerRegistry>,
    ) -> Self {
        let mut implementations: HashMap<WorkerImplementation, Arc<dyn TtsWorker>> = HashMap::new();
        implementations.insert(
            WorkerImplementation::Python,
            Arc::new(PythonTtsWorker(python)),
        );
        if let Some(owner) = native {
            implementations.insert(WorkerImplementation::Rust, Arc::new(RustTtsWorker(owner)));
        }
        Self {
            settings,
            registry,
            implementations,
        }
    }

    fn ensure_selection_unchanged(&self, speech: &SpeechSettings) -> Result<(), VoiceError> {
        let current = self
            .settings
            .snapshot_all()
            .map_err(|error| VoiceError::Generation(error.to_string()))?;
        if current.values.speech.model_for(super::AiService::Tts)
            != speech.model_for(super::AiService::Tts)
            || current.values.speech.tts != speech.tts
            || current.values.speech.tts_model_options != speech.tts_model_options
        {
            return Err(VoiceError::Unavailable);
        }
        Ok(())
    }
}

impl VoiceProvider for RoutedSpeechVoice {
    fn synthesize(&self, text: Arc<str>, options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async move {
            let saved = self
                .settings
                .snapshot_all()
                .map_err(|error| VoiceError::Generation(error.to_string()))?;
            let speech = &saved.values.speech;
            let selected = speech
                .model_for(super::AiService::Tts)
                .unwrap_or_else(|| SpeechModelReference::python(speech.tts.local_model_id.clone()));
            let worker = self
                .implementations
                .get(&selected.implementation)
                .ok_or(VoiceError::Unavailable)?;

            let audio = if speech.tts.backend == TtsBackend::Local {
                let binding = self
                    .registry
                    .connect(
                        ServiceModel {
                            service: super::AiService::Tts,
                            model: selected.clone().into(),
                        },
                        Arc::clone(worker),
                    )
                    .map_err(|_| VoiceError::Unavailable)?;
                let call = binding.begin().await.map_err(|_| VoiceError::Unavailable)?;
                let audio = call
                    .dispatch(call.backend().synthesize(
                        &selected,
                        speech,
                        call.permit(),
                        text,
                        options,
                    ))
                    .await
                    .map_err(|_| VoiceError::Unavailable)??;
                self.ensure_selection_unchanged(speech)?;
                audio
            } else {
                // Telegram/API are existing non-local voice transports, not
                // active model-backed service workers. Preserve their behavior.
                let permit = self.registry.inference_permit(super::AiService::Tts).await;
                let audio = worker
                    .synthesize(&selected, speech, &permit, text, options)
                    .await?;
                self.ensure_selection_unchanged(speech)?;
                audio
            };
            Ok(audio)
        })
    }
}

pub(crate) struct RoutedSpeechTranscriber {
    pub settings: Arc<SettingsOwner>,
    registry: Arc<ServiceWorkerRegistry>,
    implementations: HashMap<WorkerImplementation, Arc<dyn AsrWorker>>,
}

impl RoutedSpeechTranscriber {
    pub fn new(
        settings: Arc<SettingsOwner>,
        python: Option<Arc<dyn SpeechTranscriber>>,
        native: Option<Arc<AiHubNativeService>>,
        registry: Arc<ServiceWorkerRegistry>,
    ) -> Self {
        let mut implementations: HashMap<WorkerImplementation, Arc<dyn AsrWorker>> = HashMap::new();
        if let Some(owner) = python {
            implementations.insert(
                WorkerImplementation::Python,
                Arc::new(PythonAsrWorker(owner)),
            );
        }
        if let Some(owner) = native {
            implementations.insert(WorkerImplementation::Rust, Arc::new(RustAsrWorker(owner)));
        }
        Self {
            settings,
            registry,
            implementations,
        }
    }
}

impl SpeechTranscriber for RoutedSpeechTranscriber {
    fn transcribe<'a>(&'a self, wav: &'a [u8]) -> SpeechFuture<'a, Transcript> {
        Box::pin(async move {
            let saved = self
                .settings
                .snapshot_all()
                .map_err(|error| SpeechSessionError::Asr(error.to_string()))?;
            let speech = &saved.values.speech;
            let selected = speech
                .model_for(super::AiService::Asr)
                .ok_or_else(|| SpeechSessionError::Asr("no ASR model selected".into()))?;
            let worker = self
                .implementations
                .get(&selected.implementation)
                .ok_or_else(|| {
                    SpeechSessionError::Asr("selected ASR model worker is not installed".into())
                })?;
            let binding = self
                .registry
                .connect(
                    ServiceModel {
                        service: super::AiService::Asr,
                        model: selected.clone().into(),
                    },
                    Arc::clone(worker),
                )
                .map_err(|error| SpeechSessionError::Asr(error.to_string()))?;
            let call = binding
                .begin()
                .await
                .map_err(|error| SpeechSessionError::Asr(error.to_string()))?;
            let transcript = call
                .dispatch(
                    call.backend()
                        .transcribe(&selected, speech, call.permit(), wav),
                )
                .await
                .map_err(|error| SpeechSessionError::Asr(error.to_string()))??;
            let current = self
                .settings
                .snapshot_all()
                .map_err(|error| SpeechSessionError::Asr(error.to_string()))?;
            if current.values.speech.model_for(super::AiService::Asr)
                != speech.model_for(super::AiService::Asr)
                || current.values.speech.asr != speech.asr
                || current.values.speech.asr_model_options != speech.asr_model_options
            {
                return Err(SpeechSessionError::Asr(
                    "ASR model selection changed during inference".into(),
                ));
            }
            Ok(transcript)
        })
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::{
        ai_runtime::AiService,
        presentation::SettingsPresenter,
        settings::{
            AppSettings, MemorySettingsRepository, SettingsConfig, SettingsLifecycle,
            SettingsSource,
        },
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountedTts(Arc<AtomicUsize>);
    impl TtsWorker for CountedTts {
        fn synthesize<'a>(
            &'a self,
            model: &'a SpeechModelReference,
            _speech: &'a SpeechSettings,
            _permit: &'a ServiceInferencePermit<'_>,
            text: Arc<str>,
            _options: VoiceOptions,
        ) -> VoiceFuture<'a> {
            Box::pin(async move {
                assert_eq!(model.implementation, WorkerImplementation::Rust);
                assert_eq!(&*text, "hello");
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(AudioArtifact {
                    format: AudioFormat::Wave,
                    bytes: Arc::from(&b"fixture"[..]),
                })
            })
        }
    }

    #[tokio::test]
    async fn native_selection_uses_typed_tts_port_and_rejects_same_named_python_substitution() {
        let mut saved = AppSettings::initial_profile();
        saved.speech.tts.enabled = true;
        saved.speech.tts.backend = TtsBackend::Local;
        saved.speech.tts.local_auto_load = true;
        let model = SpeechModelReference::rust("shared", "a".repeat(64));
        saved
            .speech
            .select_model(AiService::Tts, Some(model.clone()))
            .unwrap();
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(saved),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let registry = ServiceWorkerRegistry::new();
        let _claim = registry
            .claim(ServiceModel {
                service: AiService::Tts,
                model: model.into(),
            })
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut routed = RoutedSpeechVoice::new(
            Arc::clone(&owner),
            Arc::new(crate::voice::UnavailableVoiceProvider),
            None,
            registry,
        );
        routed.implementations.insert(
            WorkerImplementation::Rust,
            Arc::new(CountedTts(Arc::clone(&calls))),
        );
        let options = VoiceOptions {
            voice: "speaker".into(),
            volume: crate::settings::Volume::new(50).unwrap(),
        };
        assert_eq!(
            &*routed
                .synthesize(Arc::from("hello"), options.clone())
                .await
                .unwrap()
                .bytes,
            b"fixture"
        );
        SettingsPresenter::new(owner)
            .tts
            .model
            .set(Some(SpeechModelReference::python("shared")))
            .unwrap();
        assert!(
            routed
                .synthesize(Arc::from("hello"), options)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
