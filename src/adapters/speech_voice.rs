//! Application speech routing for a request-scoped voice plan.
//! Selection is resolved before generation, so later settings changes cannot redirect a queued request.

use std::sync::Arc;

use crate::voice::{VoiceBackend, VoiceError, VoiceFuture, VoiceOptions, VoiceProvider};

use super::{fish_audio::FishAudioVoice, telegram_voice::TelegramVoice};

pub(crate) struct SpeechVoiceRouter {
    local: Arc<dyn VoiceProvider>,
    fish: FishAudioVoice,
    telegram: TelegramVoice,
}

impl SpeechVoiceRouter {
    pub(crate) fn new(
        local: Arc<dyn VoiceProvider>,
        fish: FishAudioVoice,
        telegram: TelegramVoice,
    ) -> Self {
        Self {
            local,
            fish,
            telegram,
        }
    }
}

impl VoiceProvider for SpeechVoiceRouter {
    fn synthesize(&self, text: Arc<str>, options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async move {
            match &options.backend {
                VoiceBackend::Disabled => Err(VoiceError::Unavailable),
                VoiceBackend::Local => self.local.synthesize(text, options).await,
                VoiceBackend::FishAudio {
                    provider,
                    model,
                    speed_percent,
                } => {
                    self.fish
                        .synthesize(
                            &text,
                            &options.voice,
                            provider,
                            model,
                            *speed_percent,
                            options.volume,
                        )
                        .await
                }
                VoiceBackend::Telegram {
                    bot,
                    wait_seconds,
                    interval_seconds,
                } => {
                    self.telegram
                        .synthesize(
                            &text,
                            &options.voice,
                            bot,
                            *wait_seconds,
                            *interval_seconds,
                            options.volume,
                        )
                        .await
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        settings::Volume,
        voice::{AudioArtifact, AudioFormat},
    };

    struct LocalStub;
    impl VoiceProvider for LocalStub {
        fn synthesize(&self, _text: Arc<str>, _options: VoiceOptions) -> VoiceFuture<'_> {
            Box::pin(async {
                Ok(AudioArtifact {
                    format: AudioFormat::Wave,
                    bytes: Arc::from(b"RIFF".as_slice()),
                })
            })
        }
    }

    #[tokio::test]
    async fn dispatch_uses_request_plan_instead_of_mutable_settings() {
        let path = std::env::temp_dir().join("nonexistent-fish-key");
        let router = SpeechVoiceRouter::new(
            Arc::new(LocalStub),
            FishAudioVoice::new(path).unwrap(),
            TelegramVoice::new(std::env::temp_dir()),
        );
        let mut options = VoiceOptions {
            voice: "default".into(),
            volume: Volume::new(50).unwrap(),
            backend: VoiceBackend::Disabled,
        };
        assert!(matches!(
            router.synthesize("test".into(), options.clone()).await,
            Err(VoiceError::Unavailable)
        ));
        options.backend = VoiceBackend::Local;
        assert_eq!(
            router
                .synthesize("test".into(), options.clone())
                .await
                .unwrap()
                .bytes
                .as_ref(),
            b"RIFF"
        );
        options.backend = VoiceBackend::FishAudio {
            provider: "not-fish".into(),
            model: "s2.1-pro-free".into(),
            speed_percent: 100,
        };
        assert!(router.synthesize("test".into(), options).await.is_err());
    }
}
