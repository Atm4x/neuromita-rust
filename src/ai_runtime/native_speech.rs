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

use super::native_service::AiHubNativeService;
use crate::{
    audio::session::{SpeechFuture, SpeechSessionError, SpeechTranscriber},
    settings::{SettingsOwner, SpeechEngineSelection, TtsBackend},
    voice::{VoiceError, VoiceFuture, VoiceOptions, VoiceProvider},
};

pub(crate) struct RoutedSpeechVoice {
    pub settings: Arc<SettingsOwner>,
    pub python: Arc<dyn VoiceProvider>,
    pub native: Option<Arc<AiHubNativeService>>,
}

impl VoiceProvider for RoutedSpeechVoice {
    fn synthesize(&self, text: Arc<str>, options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async move {
            let saved = self
                .settings
                .snapshot_all()
                .map_err(|error| VoiceError::Generation(error.to_string()))?;
            let speech = &saved.values.speech;
            if let SpeechEngineSelection::Native {
                model_id,
                manifest_sha256,
            } = &speech.tts_engine
            {
                if !speech.tts.enabled || speech.tts.backend != TtsBackend::Local {
                    return Err(VoiceError::Unavailable);
                }
                let owner = self.native.as_ref().ok_or(VoiceError::Unavailable)?;
                let result = owner
                    .synthesize_speech(
                        model_id,
                        manifest_sha256,
                        &text,
                        &options.voice,
                        &speech.tts.local_language,
                        options.volume.get(),
                    )
                    .await
                    .map_err(VoiceError::Generation)?;
                let current = self
                    .settings
                    .snapshot_all()
                    .map_err(|error| VoiceError::Generation(error.to_string()))?;
                if current.values.speech.tts_engine != speech.tts_engine
                    || current.values.speech.tts.local_language != speech.tts.local_language
                    || !current.values.speech.tts.enabled
                    || current.values.speech.tts.backend != TtsBackend::Local
                {
                    return Err(VoiceError::Unavailable);
                }
                Ok(result)
            } else {
                self.python.synthesize(text, options).await
            }
        })
    }
}

pub(crate) struct RoutedSpeechTranscriber {
    pub settings: Arc<SettingsOwner>,
    pub python: Option<Arc<dyn SpeechTranscriber>>,
    pub native: Option<Arc<AiHubNativeService>>,
}

impl SpeechTranscriber for RoutedSpeechTranscriber {
    fn transcribe<'a>(&'a self, wav: &'a [u8]) -> SpeechFuture<'a, Transcript> {
        Box::pin(async move {
            let saved = self
                .settings
                .snapshot_all()
                .map_err(|error| SpeechSessionError::Asr(error.to_string()))?;
            let speech = &saved.values.speech;
            if let SpeechEngineSelection::Native {
                model_id,
                manifest_sha256,
            } = &speech.asr_engine
            {
                let owner = self
                    .native
                    .as_ref()
                    .ok_or_else(|| SpeechSessionError::Asr("native ASR is not active".into()))?;
                let language = speech
                    .asr
                    .model_languages
                    .get(model_id)
                    .map_or("auto", String::as_str);
                let result = owner
                    .transcribe_speech(model_id, manifest_sha256, wav, language)
                    .await
                    .map_err(SpeechSessionError::Asr)?;
                let current = self
                    .settings
                    .snapshot_all()
                    .map_err(|error| SpeechSessionError::Asr(error.to_string()))?;
                if current.values.speech.asr_engine != speech.asr_engine
                    || current.values.speech.asr.model_languages.get(model_id)
                        != speech.asr.model_languages.get(model_id)
                {
                    return Err(SpeechSessionError::Asr(
                        "ASR selection changed during inference".into(),
                    ));
                }
                Ok(result)
            } else {
                self.python
                    .as_ref()
                    .ok_or_else(|| SpeechSessionError::Asr("Python ASR is not active".into()))?
                    .transcribe(wav)
                    .await
            }
        })
    }
}
