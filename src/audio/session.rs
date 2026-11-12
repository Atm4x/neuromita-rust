//! Speech session: captured PCM16 -> VAD/PTT -> ASR -> player message -> reply -> playback.
//! One application-owned worker performs transcript dispatch; the capture path is bounded.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use thiserror::Error;
use tokio::{sync::mpsc, time::timeout};
use tokio_util::sync::CancellationToken;

use crate::{
    ai_runtime::speech::{PythonSpeechRecognizer, SpeechRecognitionError, Transcript},
    character::CharacterId,
    interaction::UserInput,
    request::{
        ChatApiState, ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState,
    },
    voice::AudioArtifact,
};

use super::{
    device::{AudioCapture, AudioPlayback, DeviceError},
    segment::{SegmentError, SpeechSegment, SpeechSegmenter, VoiceGate},
    ticket::{SpeechStream, SpeechTicket},
};

#[derive(Debug, Error)]
pub enum SpeechSessionError {
    #[error(transparent)]
    Audio(#[from] DeviceError),
    #[error(transparent)]
    Segment(#[from] SegmentError),
    #[error("ASR failed: {0}")]
    Asr(String),
    #[error("conversation failed: {0}")]
    Conversation(String),
    #[error("speech dispatch channel was closed")]
    DispatchClosed,
    #[error("speech processing queue is full")]
    QueueFull,
    #[error("speech task failed: {0}")]
    Task(String),
}

pub type SpeechFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SpeechSessionError>> + Send + 'a>>;

pub trait SpeechTranscriber: Send + Sync {
    fn transcribe<'a>(&'a self, wav: &'a [u8]) -> SpeechFuture<'a, Transcript>;
}

impl SpeechTranscriber for PythonSpeechRecognizer {
    fn transcribe<'a>(&'a self, wav: &'a [u8]) -> SpeechFuture<'a, Transcript> {
        Box::pin(async move {
            self.transcribe_wav(wav)
                .await
                .map_err(|error: SpeechRecognitionError| SpeechSessionError::Asr(error.to_string()))
        })
    }
}

pub trait SpeechConversation: Send + Sync {
    fn reply<'a>(
        &'a self,
        ticket: &'a SpeechTicket,
        transcript: &'a Transcript,
    ) -> SpeechFuture<'a, Option<AudioArtifact>>;
}

struct CancelUnfinished<'a> {
    request: &'a RequestHandle,
    finished: bool,
}

impl Drop for CancelUnfinished<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.request.cancel();
        }
    }
}

pub struct ChatReplyTarget {
    api: Arc<ChatApiState>,
    character: CharacterId,
    source: RequestSource,
}

impl ChatReplyTarget {
    pub fn capture(
        character: CharacterId,
        source: RequestSource,
    ) -> Result<Self, SpeechSessionError> {
        let bindings = crate::runtime::current()
            .map_err(|_| SpeechSessionError::Conversation("application unavailable".into()))?;
        Ok(Self::bound(Arc::clone(&bindings.chat), character, source))
    }

    pub(crate) fn bound(
        api: Arc<ChatApiState>,
        character: CharacterId,
        source: RequestSource,
    ) -> Self {
        Self {
            api,
            character,
            source,
        }
    }
}

impl SpeechConversation for ChatReplyTarget {
    fn reply<'a>(
        &'a self,
        ticket: &'a SpeechTicket,
        transcript: &'a Transcript,
    ) -> SpeechFuture<'a, Option<AudioArtifact>> {
        Box::pin(async move {
            let input = UserInput::new(transcript.text.clone())
                .map_err(|error| SpeechSessionError::Conversation(error.to_string()))?;
            let Some(handle) = ticket.admit(|| {
                let event = self.api.publish_message(self.character, input)?;
                self.api
                    .send(ReplyRequest::new(self.character, event, self.source))
            }) else {
                return Ok(None);
            };
            let handle =
                handle.map_err(|error| SpeechSessionError::Conversation(error.to_string()))?;
            let mut cancellation = CancelUnfinished {
                request: &handle,
                finished: false,
            };
            let snapshot = handle
                .wait()
                .await
                .map_err(|error| SpeechSessionError::Conversation(error.to_string()))?;
            cancellation.finished = true;
            match &snapshot.state {
                RequestState::Finished(RequestOutcome::Success(result)) => Ok(result.audio.clone()),
                RequestState::Finished(RequestOutcome::VoiceGenerationFailed { error, .. }) => {
                    Err(SpeechSessionError::Conversation(error.to_string()))
                }
                RequestState::Finished(RequestOutcome::TextGenerationFailed(error)) => {
                    Err(SpeechSessionError::Conversation(error.to_string()))
                }
                RequestState::Finished(RequestOutcome::InternalError(error)) => {
                    Err(SpeechSessionError::Conversation(error.to_string()))
                }
                RequestState::Cancelled => {
                    Err(SpeechSessionError::Conversation("reply cancelled".into()))
                }
                _ => Err(SpeechSessionError::Conversation(
                    "reply has no terminal outcome".into(),
                )),
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechControl {
    PushToTalk(bool),
    Reset,
    CancelPlayback,
}

struct PlaybackState(Arc<AtomicBool>);
impl Drop for PlaybackState {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct QueuedSegment {
    segment: SpeechSegment,
    ticket: SpeechTicket,
}

fn queue_segment(
    send: &mpsc::Sender<QueuedSegment>,
    segment: SpeechSegment,
    stream: &Arc<SpeechStream>,
) -> Result<(), SpeechSessionError> {
    send.try_send(QueuedSegment {
        segment,
        ticket: stream.ticket(),
    })
    .map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => SpeechSessionError::QueueFull,
        mpsc::error::TrySendError::Closed(_) => SpeechSessionError::DispatchClosed,
    })
}

#[derive(Clone)]
pub struct SpeechInputHandler {
    transcriber: Arc<dyn SpeechTranscriber>,
    conversation: Arc<dyn SpeechConversation>,
    playback: Arc<dyn AudioPlayback>,
}

impl SpeechInputHandler {
    pub fn new(
        transcriber: Arc<dyn SpeechTranscriber>,
        conversation: Arc<dyn SpeechConversation>,
        playback: Arc<dyn AudioPlayback>,
    ) -> Self {
        Self {
            transcriber,
            conversation,
            playback,
        }
    }

    pub async fn run<G: VoiceGate + 'static>(
        self,
        mut capture: impl AudioCapture,
        mut segmenter: SpeechSegmenter<G>,
        mut controls: mpsc::Receiver<SpeechControl>,
        stop: CancellationToken,
    ) -> Result<(), SpeechSessionError> {
        let stream = SpeechStream::new(stop.clone());
        let (send, mut receive) = mpsc::channel::<QueuedSegment>(2);
        let worker_stop = stop.clone();
        let capture_stream = Arc::clone(&stream);
        let playing = Arc::new(AtomicBool::new(false));
        let worker_playing = Arc::clone(&playing);
        let playback_token = Arc::new(Mutex::new(None::<CancellationToken>));
        let worker_playback_token = Arc::clone(&playback_token);
        let active_turn = Arc::new(Mutex::new(None::<CancellationToken>));
        let worker_active_turn = Arc::clone(&active_turn);
        let worker = tokio::spawn(async move {
            while let Some(QueuedSegment { segment, ticket }) = receive.recv().await {
                if worker_stop.is_cancelled() {
                    return Ok(());
                }
                if !ticket.is_current() {
                    continue;
                }
                let turn_stop = ticket.cancellation().child_token();
                if let Ok(mut active) = worker_active_turn.lock() {
                    *active = Some(turn_stop.clone());
                }
                let wav = segment.as_wav()?;
                let transcript = tokio::select! {
                    biased;
                    _ = turn_stop.cancelled() => continue,
                    _ = worker_stop.cancelled() => return Ok(()),
                    result = self.transcriber.transcribe(&wav) => result?,
                };
                if transcript.text.trim().is_empty() {
                    continue;
                }
                let audio = tokio::select! {
                    biased;
                    _ = turn_stop.cancelled() => continue,
                    _ = worker_stop.cancelled() => return Ok(()),
                    result = self.conversation.reply(&ticket, &transcript) => result?,
                };
                if let Some(audio) = audio {
                    let playing_token = turn_stop.child_token();
                    if let Ok(mut active) = worker_playback_token.lock() {
                        *active = Some(playing_token.clone());
                    }
                    worker_playing.store(true, Ordering::Release);
                    let _playing = PlaybackState(Arc::clone(&worker_playing));
                    let result = self.playback.play(audio, playing_token).await;
                    if let Ok(mut active) = worker_playback_token.lock() {
                        *active = None;
                    }
                    result?;
                }
                if let Ok(mut active) = worker_active_turn.lock() {
                    *active = None;
                }
            }
            Ok::<(), SpeechSessionError>(())
        });
        let mut worker = tokio_util::task::AbortOnDropHandle::new(worker);
        let mut worker_finished = None;
        let mut controls_open = true;
        let capture_result = loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break Ok(()),
                result = &mut worker => {
                    worker_finished = Some(result);
                    break Ok(());
                }
                control = controls.recv(), if controls_open => {
                    match control {
                        Some(SpeechControl::PushToTalk(pressed)) => {
                            if pressed {
                                capture_stream.invalidate();
                            }
                            if pressed && let Ok(active) = active_turn.lock()
                                && let Some(token) = active.as_ref() { token.cancel(); }
                            if pressed && let Ok(active) = playback_token.lock()
                                && let Some(token) = active.as_ref() {
                                    token.cancel();
                            }
                            if let Some(segment) = segmenter.set_pressed(pressed)
                                && let Err(error) = queue_segment(&send, segment, &capture_stream) { break Err(error); }
                        }
                        Some(SpeechControl::Reset | SpeechControl::CancelPlayback) => {
                            capture_stream.invalidate();
                            if let Ok(active) = active_turn.lock()
                                && let Some(token) = active.as_ref() { token.cancel(); }
                            if let Ok(active) = playback_token.lock()
                                && let Some(token) = active.as_ref() {
                                    token.cancel();
                            }
                            segmenter.reset();
                        },
                        None => controls_open = false,
                    }
                }
                frame = capture.next_frame() => {
                    match frame {
                        Ok(Some(frame)) => {
                            if playing.load(Ordering::Acquire) {
                                segmenter.suppress_playback_echo();
                                continue;
                            }
                            if let Some(segment) = segmenter.process(frame)
                                && let Err(error) = queue_segment(&send, segment, &capture_stream) { break Err(error); }
                        }
                        Ok(None) => {
                            if let Some(segment) = segmenter.flush()
                                && let Err(error) = queue_segment(&send, segment, &capture_stream) { break Err(error); }
                            break Ok(());
                        }
                        Err(error) => break Err(error.into()),
                    }
                }
            }
        };
        drop(send);
        if capture_result.is_err() || stop.is_cancelled() {
            stop.cancel();
            worker.abort();
        }
        let dispatch = match match worker_finished {
            Some(result) => result,
            None => match timeout(Duration::from_secs(30), &mut worker).await {
                Ok(result) => result,
                Err(_) => {
                    stop.cancel();
                    worker.abort();
                    return Err(SpeechSessionError::Task(
                        "speech dispatch drain exceeded 30 seconds".into(),
                    ));
                }
            },
        } {
            Ok(result) => result,
            Err(error) if stop.is_cancelled() && error.is_cancelled() => Ok(()),
            Err(error) => Err(SpeechSessionError::Task(error.to_string())),
        };
        capture_result?;
        dispatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{
        device::{DeviceFuture, SilentPlayback},
        segment::{SegmentConfig, SpeechMode},
        vad::{FRAME_SAMPLES, MonoPcm16Frame, VoiceDecision},
    };
    use std::sync::Mutex;

    struct Gate;
    impl VoiceGate for Gate {
        fn inspect(&mut self, f: &MonoPcm16Frame) -> VoiceDecision {
            VoiceDecision {
                probability: f32::from(f[0] != 0),
                speech: f[0] != 0,
            }
        }
        fn reset(&mut self) {}
    }
    struct Capture(Vec<MonoPcm16Frame>);
    impl AudioCapture for Capture {
        fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
            Box::pin(async {
                Ok(if self.0.is_empty() {
                    None
                } else {
                    Some(self.0.remove(0))
                })
            })
        }
    }
    struct Asr;
    impl SpeechTranscriber for Asr {
        fn transcribe<'a>(&'a self, bytes: &'a [u8]) -> SpeechFuture<'a, Transcript> {
            Box::pin(async move {
                assert_eq!(&bytes[0..4], b"RIFF");
                Ok(Transcript {
                    text: "hello from microphone".into(),
                    language: Some("en".into()),
                })
            })
        }
    }
    struct Chat(Arc<Mutex<Vec<String>>>);
    impl SpeechConversation for Chat {
        fn reply<'a>(
            &'a self,
            ticket: &'a SpeechTicket,
            transcript: &'a Transcript,
        ) -> SpeechFuture<'a, Option<AudioArtifact>> {
            Box::pin(async move {
                ticket.admit(|| self.0.lock().unwrap().push(transcript.text.clone()));
                Ok(None)
            })
        }
    }
    #[tokio::test]
    async fn automatic_session_sends_single_segment_through_asr_and_chat() {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let frames = [
            vec![[0; FRAME_SAMPLES]; 4],
            vec![[100; FRAME_SAMPLES]; 5],
            vec![[0; FRAME_SAMPLES]; 4],
        ]
        .concat();
        let config = SegmentConfig {
            mode: SpeechMode::Automatic,
            pre_roll_frames: 3,
            start_voice_frames: 2,
            end_silence_frames: 3,
            min_segment_frames: 2,
            max_segment_frames: 25,
        };
        let session = SpeechInputHandler::new(
            Arc::new(Asr),
            Arc::new(Chat(Arc::clone(&delivered))),
            Arc::new(SilentPlayback),
        );
        let (_send, controls) = mpsc::channel(2);
        session
            .run(
                Capture(frames),
                SpeechSegmenter::new(Gate, config).unwrap(),
                controls,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(&*delivered.lock().unwrap(), &["hello from microphone"]);
    }

    struct SignalledCapture {
        frames: mpsc::Receiver<MonoPcm16Frame>,
        consumed: mpsc::UnboundedSender<()>,
    }

    impl AudioCapture for SignalledCapture {
        fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
            Box::pin(async {
                let frame = self.frames.recv().await;
                if frame.is_some() {
                    let _ = self.consumed.send(());
                }
                Ok(frame)
            })
        }
    }

    struct BlockFirstAsr {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        first_started: Arc<tokio::sync::Notify>,
    }

    impl SpeechTranscriber for BlockFirstAsr {
        fn transcribe<'a>(&'a self, _wav: &'a [u8]) -> SpeechFuture<'a, Transcript> {
            Box::pin(async move {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    self.first_started.notify_one();
                    std::future::pending::<()>().await;
                }
                Ok(Transcript {
                    text: format!("utterance {call}"),
                    language: Some("en".into()),
                })
            })
        }
    }

    async fn invalidation_rejects_queued_segments_and_late_asr_before_dispatch(
        control: SpeechControl,
    ) {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_started = Arc::new(tokio::sync::Notify::new());
        let session = SpeechInputHandler::new(
            Arc::new(BlockFirstAsr {
                calls: Arc::clone(&calls),
                first_started: Arc::clone(&first_started),
            }),
            Arc::new(Chat(Arc::clone(&delivered))),
            Arc::new(SilentPlayback),
        );
        let config = SegmentConfig {
            mode: SpeechMode::Automatic,
            pre_roll_frames: 0,
            start_voice_frames: 2,
            end_silence_frames: 3,
            min_segment_frames: 2,
            max_segment_frames: 25,
        };
        let (frames_send, frames_receive) = mpsc::channel(64);
        let (consumed_send, mut consumed_receive) = mpsc::unbounded_channel();
        let (control_send, control_receive) = mpsc::channel(8);
        let task = tokio::spawn(session.run(
            SignalledCapture {
                frames: frames_receive,
                consumed: consumed_send,
            },
            SpeechSegmenter::new(Gate, config).unwrap(),
            control_receive,
            CancellationToken::new(),
        ));
        let utterance = [vec![[100; FRAME_SAMPLES]; 5], vec![[0; FRAME_SAMPLES]; 4]].concat();
        for frame in utterance.iter().copied().chain(utterance.iter().copied()) {
            frames_send.send(frame).await.unwrap();
        }
        for _ in 0..utterance.len() * 2 {
            tokio::time::timeout(Duration::from_secs(2), consumed_receive.recv())
                .await
                .unwrap()
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), first_started.notified())
            .await
            .unwrap();
        control_send.send(control).await.unwrap();
        for &frame in &utterance {
            frames_send.send(frame).await.unwrap();
        }
        drop(frames_send);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(&*delivered.lock().unwrap(), &["utterance 1"]);
    }

    #[tokio::test]
    async fn reset_rejects_queued_segments_and_late_asr_before_dispatch() {
        invalidation_rejects_queued_segments_and_late_asr_before_dispatch(SpeechControl::Reset)
            .await;
    }

    #[tokio::test]
    async fn ptt_press_rejects_queued_segments_and_late_asr_before_dispatch() {
        invalidation_rejects_queued_segments_and_late_asr_before_dispatch(
            SpeechControl::PushToTalk(true),
        )
        .await;
    }
}
