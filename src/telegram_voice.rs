//! Telegram user-session voice conversation coordinator.
//! The MTProto client and its authorization are supplied by the application.
//! This module does not use the Telegram Bot API or play audio.
use crate::voice::{
    AudioArtifact, AudioFormat, VoiceError, VoiceFuture, VoiceOptions, VoiceProvider,
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, timeout},
};

pub type TelegramFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TelegramError>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramError {
    Disconnected,
    Unauthorized,
    Unavailable,
    Network,
    Timeout,
    InvalidMedia,
    MediaTooLarge,
}
impl std::fmt::Display for TelegramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Mp3,
    VoiceOgg,
    Other,
}

#[derive(Debug, Clone)]
pub struct Incoming {
    pub id: i64,
    pub peer: String,
    pub outgoing: bool,
    pub kind: Option<MediaKind>,
}

/// Must be implemented by an authorized *user* MTProto session; a Bot API
/// token is not a valid implementation of this transport.
pub trait TelegramTransport: Send + Sync {
    fn ready(&self) -> TelegramFuture<'_, ()>;
    fn latest<'a>(&'a self, peer: &'a str) -> TelegramFuture<'a, i64>;
    fn send<'a>(&'a self, peer: &'a str, text: &'a str) -> TelegramFuture<'a, i64>;
    fn recent<'a>(&'a self, peer: &'a str, after: i64) -> TelegramFuture<'a, Vec<Incoming>>;
    fn download<'a>(&'a self, peer: &'a str, id: i64, limit: usize) -> TelegramFuture<'a, Vec<u8>>;
}

pub trait TelegramDecoder: Send + Sync {
    fn decode(&self, kind: MediaKind, bytes: Vec<u8>) -> TelegramFuture<'_, AudioArtifact>;
}

#[derive(Clone)]
pub struct TelegramConfig {
    pub peer: String,
    pub wait: Duration,
    pub interval: Duration,
    pub poll: Duration,
    pub max_media_bytes: usize,
}
impl TelegramConfig {
    pub fn validate(&self) -> Result<(), TelegramError> {
        if !self.peer.starts_with('@')
            || self.peer.len() > 256
            || self.peer.len() < 2
            || self.wait.is_zero()
            || self.wait > Duration::from_secs(600)
            || self.interval > Duration::from_secs(600)
            || self.poll.is_zero()
            || self.max_media_bytes == 0
            || self.max_media_bytes > 32 * 1024 * 1024
        {
            return Err(TelegramError::Unavailable);
        }
        Ok(())
    }
}

struct Conversation {
    last_send: Option<Instant>,
    speaker: Option<String>,
}
pub struct TelegramVoiceService {
    transport: Arc<dyn TelegramTransport>,
    decoder: Arc<dyn TelegramDecoder>,
    config: TelegramConfig,
    /// Serializes all work sharing a Telegram bot conversation.
    conversation: Mutex<Conversation>,
}

impl TelegramVoiceService {
    pub fn new(
        transport: Arc<dyn TelegramTransport>,
        decoder: Arc<dyn TelegramDecoder>,
        config: TelegramConfig,
    ) -> Result<Self, TelegramError> {
        config.validate()?;
        Ok(Self {
            transport,
            decoder,
            config,
            conversation: Mutex::new(Conversation {
                last_send: None,
                speaker: None,
            }),
        })
    }

    async fn send(&self, state: &mut Conversation, text: &str) -> Result<i64, TelegramError> {
        if let Some(last) = state.last_send {
            let due = last + self.config.interval;
            if let Some(delay) = due.checked_duration_since(Instant::now()) {
                sleep(delay).await;
            }
        }
        // Even a failed send might have reached Telegram.
        state.last_send = Some(Instant::now());
        self.transport.send(&self.config.peer, text).await
    }

    pub async fn speak(&self, text: &str, speaker: &str) -> Result<AudioArtifact, TelegramError> {
        if text.trim().is_empty() {
            return Err(TelegramError::Unavailable);
        }
        let mut state = self.conversation.lock().await;
        self.transport.ready().await?;
        let mut watermark = self.transport.latest(&self.config.peer).await?;
        let speaker = speaker.trim();
        if !speaker.is_empty() {
            let command = format!("/speaker {speaker}");
            if state.speaker.as_deref() != Some(command.as_str()) {
                watermark = watermark.max(self.send(&mut state, &command).await?);
                state.speaker = Some(command);
            }
        }
        let sent = self.send(&mut state, text).await?;
        watermark = watermark.max(sent);
        let deadline = Instant::now() + self.config.wait;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .ok_or(TelegramError::Timeout)?;
            let mut messages = timeout(left, self.transport.recent(&self.config.peer, watermark))
                .await
                .map_err(|_| TelegramError::Timeout)??;
            messages.sort_by_key(|m| m.id);
            for msg in messages {
                if msg.id <= watermark || msg.outgoing || msg.peer != self.config.peer {
                    continue;
                }
                let Some(kind @ (MediaKind::Mp3 | MediaKind::VoiceOgg)) = msg.kind else {
                    continue;
                };
                let left = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or(TelegramError::Timeout)?;
                let bytes = timeout(
                    left,
                    self.transport
                        .download(&self.config.peer, msg.id, self.config.max_media_bytes),
                )
                .await
                .map_err(|_| TelegramError::Timeout)??;
                if bytes.is_empty() {
                    return Err(TelegramError::InvalidMedia);
                }
                if bytes.len() > self.config.max_media_bytes {
                    return Err(TelegramError::MediaTooLarge);
                }
                let left = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or(TelegramError::Timeout)?;
                let audio = timeout(left, self.decoder.decode(kind, bytes))
                    .await
                    .map_err(|_| TelegramError::Timeout)??;
                if audio.format != AudioFormat::Wave || audio.bytes.is_empty() {
                    return Err(TelegramError::InvalidMedia);
                }
                return Ok(audio);
            }
            let left = deadline
                .checked_duration_since(Instant::now())
                .ok_or(TelegramError::Timeout)?;
            sleep(left.min(self.config.poll)).await;
        }
    }
}
impl VoiceProvider for TelegramVoiceService {
    fn synthesize(&self, text: Arc<str>, options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async move {
            self.speak(&text, &options.voice)
                .await
                .map_err(|e| VoiceError::Generation(e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicUsize, Ordering},
    };
    #[derive(Default)]
    struct Fake {
        sent: Mutex<Vec<String>>,
        replies: Mutex<VecDeque<Vec<Incoming>>>,
        downloads: AtomicUsize,
    }
    impl TelegramTransport for Fake {
        fn ready(&self) -> TelegramFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn latest<'a>(&'a self, _: &'a str) -> TelegramFuture<'a, i64> {
            Box::pin(async { Ok(10) })
        }
        fn send<'a>(&'a self, _: &'a str, msg: &'a str) -> TelegramFuture<'a, i64> {
            Box::pin(async move {
                self.sent.lock().await.push(msg.into());
                Ok(11)
            })
        }
        fn recent<'a>(&'a self, _: &'a str, _: i64) -> TelegramFuture<'a, Vec<Incoming>> {
            Box::pin(async move { Ok(self.replies.lock().await.pop_front().unwrap_or_default()) })
        }
        fn download<'a>(&'a self, _: &'a str, _: i64, _: usize) -> TelegramFuture<'a, Vec<u8>> {
            Box::pin(async move {
                self.downloads.fetch_add(1, Ordering::SeqCst);
                Ok(vec![1, 2])
            })
        }
    }
    struct Decoder;
    impl TelegramDecoder for Decoder {
        fn decode(&self, _: MediaKind, _: Vec<u8>) -> TelegramFuture<'_, AudioArtifact> {
            Box::pin(async {
                Ok(AudioArtifact {
                    format: AudioFormat::Wave,
                    bytes: Arc::from([1_u8, 2]),
                })
            })
        }
    }
    fn setup(fake: Arc<Fake>) -> TelegramVoiceService {
        TelegramVoiceService::new(
            fake,
            Arc::new(Decoder),
            TelegramConfig {
                peer: "@silero_voice_bot".into(),
                wait: Duration::from_millis(30),
                interval: Duration::ZERO,
                poll: Duration::from_millis(1),
                max_media_bytes: 1024,
            },
        )
        .unwrap()
    }
    fn audio(id: i64, peer: &str) -> Incoming {
        Incoming {
            id,
            peer: peer.into(),
            outgoing: false,
            kind: Some(MediaKind::Mp3),
        }
    }
    #[tokio::test]
    async fn stale_foreign_media_ignored_and_wav_returned() {
        let fake = Arc::new(Fake::default());
        fake.replies.lock().await.push_back(vec![
            audio(9, "@silero_voice_bot"),
            audio(12, "@other"),
            audio(13, "@silero_voice_bot"),
        ]);
        let artifact = setup(fake.clone()).speak("hello", "mita").await.unwrap();
        assert_eq!(artifact.format, AudioFormat::Wave);
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn no_reply_is_a_failure() {
        let fake = Arc::new(Fake::default());
        assert_eq!(
            setup(fake).speak("hello", "mita").await.unwrap_err(),
            TelegramError::Timeout
        );
    }
    #[tokio::test]
    async fn speaker_command_is_sent_only_when_changed() {
        let fake = Arc::new(Fake::default());
        fake.replies
            .lock()
            .await
            .push_back(vec![audio(12, "@silero_voice_bot")]);
        fake.replies
            .lock()
            .await
            .push_back(vec![audio(13, "@silero_voice_bot")]);
        let service = setup(fake.clone());
        service.speak("first", "mita").await.unwrap();
        service.speak("second", "mita").await.unwrap();
        assert_eq!(
            *fake.sent.lock().await,
            vec!["/speaker mita", "first", "second"]
        );
    }
}
