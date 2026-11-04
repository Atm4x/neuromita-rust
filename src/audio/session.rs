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
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ai_runtime::speech::{PythonSpeechRecognizer, SpeechRecognitionError, Transcript},
    character::CharacterId,
    interaction::UserInput,
    request::{
        ChatApiState, ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState,
    },
    voice::AudioArtifact,
};

pub use super::input_gate::GateCommand as SpeechControl;
use super::{
    device::{AudioCapture, AudioPlayback, DeviceError},
    input_gate::{GateConfig, GateError, InputGate, InputMode},
    segment::{SegmentError, SpeechMode, SpeechSegment, SpeechSegmenter, VoiceGate},
};

/// After capture ends, in-flight dispatch may finish within this window; a stuck
/// ASR or chat provider must not hold shutdown open forever.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

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
    #[error("the application already owns an active speech input session")]
    InputBusy,
    #[error(transparent)]
    InputGate(#[from] GateError),
}

/// One application-owned admission point, acquired before the host opens capture.
#[derive(Clone, Default)]
pub struct SpeechInputOwner(Arc<AtomicBool>);

impl SpeechInputOwner {
    pub fn new() -> Self {
        Self::default()
    }

    fn claim(&self) -> Result<SpeechInputLease, SpeechSessionError> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SpeechSessionError::InputBusy)?;
        Ok(SpeechInputLease(Arc::clone(&self.0)))
    }
}

struct SpeechInputLease(Arc<AtomicBool>);

impl Drop for SpeechInputLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechRoute {
    pub character: CharacterId,
    pub source: RequestSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtteranceId(Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamTicket {
    pub stream: Uuid,
    pub generation: Uuid,
    pub route: SpeechRoute,
}

struct Generation {
    id: Uuid,
    stop: CancellationToken,
}

struct SpeechStream {
    id: Uuid,
    route: SpeechRoute,
    stop: CancellationToken,
    generation: Mutex<Generation>,
}

impl SpeechStream {
    fn new(route: SpeechRoute, stop: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::new_v4(),
            route,
            generation: Mutex::new(Generation {
                id: Uuid::new_v4(),
                stop: stop.child_token(),
            }),
            stop,
        })
    }

    fn invalidate(&self) {
        let mut generation = self.generation.lock().expect("speech generation poisoned");
        generation.stop.cancel();
        *generation = Generation {
            id: Uuid::new_v4(),
            stop: self.stop.child_token(),
        };
    }

    fn ticket(self: &Arc<Self>) -> SpeechTicket {
        let generation = self.generation.lock().expect("speech generation poisoned");
        SpeechTicket {
            stream: StreamTicket {
                stream: self.id,
                generation: generation.id,
                route: self.route,
            },
            utterance: UtteranceId(Uuid::new_v4()),
            owner: Arc::clone(self),
            stop: generation.stop.child_token(),
            admitted: AtomicBool::new(false),
        }
    }
}

pub struct SpeechTicket {
    stream: StreamTicket,
    utterance: UtteranceId,
    owner: Arc<SpeechStream>,
    stop: CancellationToken,
    admitted: AtomicBool,
}

impl SpeechTicket {
    pub fn stream(&self) -> StreamTicket {
        self.stream
    }

    pub fn utterance(&self) -> UtteranceId {
        self.utterance
    }

    pub fn is_current(&self) -> bool {
        let generation = self
            .owner
            .generation
            .lock()
            .expect("speech generation poisoned");
        generation.id == self.stream.generation && !self.stop.is_cancelled()
    }

    /// Serialize synchronous admission with reset; never run asynchronous work here.
    pub fn admit<T>(&self, admission: impl FnOnce() -> T) -> Option<T> {
        let generation = self
            .owner
            .generation
            .lock()
            .expect("speech generation poisoned");
        if generation.id != self.stream.generation
            || self.stop.is_cancelled()
            || self.admitted.swap(true, Ordering::AcqRel)
        {
            return None;
        }
        Some(admission())
    }
}

struct StreamLifetime(Arc<SpeechStream>);

impl Drop for StreamLifetime {
    fn drop(&mut self) {
        let generation = self
            .0
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        generation.stop.cancel();
    }
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
    route: SpeechRoute,
}

impl ChatReplyTarget {
    pub fn capture(route: SpeechRoute) -> Result<Self, SpeechSessionError> {
        let bindings = crate::runtime::current()
            .map_err(|_| SpeechSessionError::Conversation("application unavailable".into()))?;
        Ok(Self::bound(Arc::clone(&bindings.chat), route))
    }

    pub(crate) fn bound(api: Arc<ChatApiState>, route: SpeechRoute) -> Self {
        Self { api, route }
    }
}

impl SpeechConversation for ChatReplyTarget {
    fn reply<'a>(
        &'a self,
        ticket: &'a SpeechTicket,
        transcript: &'a Transcript,
    ) -> SpeechFuture<'a, Option<AudioArtifact>> {
        Box::pin(async move {
            if ticket.stream.route != self.route {
                return Err(SpeechSessionError::Conversation(
                    "speech route mismatch".into(),
                ));
            }
            let input = UserInput::new(transcript.text.clone())
                .map_err(|error| SpeechSessionError::Conversation(error.to_string()))?;
            let Some(handle) = ticket.admit(|| {
                let event = self.api.publish_message(self.route.character, input)?;
                self.api.send(ReplyRequest::new(
                    self.route.character,
                    event,
                    self.route.source,
                ))
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

#[derive(Default)]
struct PlaybackWindow {
    active: bool,
    until: Option<Instant>,
}

struct PlaybackState(Arc<Mutex<PlaybackWindow>>);

impl Drop for PlaybackState {
    fn drop(&mut self) {
        let mut window = self.0.lock().expect("playback window poisoned");
        window.active = false;
        window.until = Some(Instant::now() + Duration::from_millis(400));
    }
}

struct QueuedSpeech {
    ticket: SpeechTicket,
    segment: SpeechSegment,
}

fn queue_segment(
    send: &mpsc::Sender<QueuedSpeech>,
    stream: &Arc<SpeechStream>,
    segment: SpeechSegment,
) -> Result<(), SpeechSessionError> {
    send.try_send(QueuedSpeech {
        ticket: stream.ticket(),
        segment,
    })
    .map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => SpeechSessionError::QueueFull,
        mpsc::error::TrySendError::Closed(_) => SpeechSessionError::DispatchClosed,
    })
}

pub struct SpeechInputHandler {
    route: SpeechRoute,
    transcriber: Arc<dyn SpeechTranscriber>,
    conversation: Arc<dyn SpeechConversation>,
    playback: Arc<dyn AudioPlayback>,
    input_lease: Option<SpeechInputLease>,
    gate: Option<GateConfig>,
}

impl SpeechInputHandler {
    pub fn new(
        route: SpeechRoute,
        transcriber: Arc<dyn SpeechTranscriber>,
        conversation: Arc<dyn SpeechConversation>,
        playback: Arc<dyn AudioPlayback>,
    ) -> Self {
        Self {
            route,
            transcriber,
            conversation,
            playback,
            input_lease: None,
            gate: None,
        }
    }

    pub fn with_owner(mut self, owner: &SpeechInputOwner) -> Result<Self, SpeechSessionError> {
        if self.input_lease.is_some() {
            return Err(SpeechSessionError::InputBusy);
        }
        self.input_lease = Some(owner.claim()?);
        Ok(self)
    }

    pub fn with_gate(mut self, config: GateConfig) -> Result<Self, SpeechSessionError> {
        InputGate::new(config)?;
        self.gate = Some(config);
        Ok(self)
    }

    pub async fn run<G: VoiceGate + 'static>(
        mut self,
        capture: impl AudioCapture,
        mut segmenter: SpeechSegmenter<G>,
        mut controls: mpsc::Receiver<SpeechControl>,
        stop: CancellationToken,
    ) -> Result<(), SpeechSessionError> {
        let _input_lease = self.input_lease.take();
        let mut capture = capture;
        let mut gate = InputGate::new(self.gate.unwrap_or(GateConfig {
            mode: if segmenter.mode() == SpeechMode::PushToTalk {
                InputMode::PushToTalk
            } else {
                InputMode::Automatic
            },
            enabled: true,
            pause_while_speaking: true,
            manual_lease: Duration::from_secs(3),
            speaking_tail: Duration::from_millis(400),
        }))?;
        let route = self.route;
        let stream = SpeechStream::new(self.route, stop.clone());
        let _lifetime = StreamLifetime(Arc::clone(&stream));
        let (send, mut receive) = mpsc::channel::<QueuedSpeech>(2);
        let worker_stop = stop.clone();
        let playing = Arc::new(Mutex::new(PlaybackWindow::default()));
        let worker_playing = Arc::clone(&playing);
        let worker = tokio::spawn(async move {
            while let Some(QueuedSpeech { ticket, segment }) = receive.recv().await {
                if worker_stop.is_cancelled() {
                    return Ok(());
                }
                if !ticket.is_current() {
                    continue;
                }
                let turn_stop = &ticket.stop;
                let wav = segment.as_wav()?;
                let transcript = tokio::select! {
                    biased;
                    _ = turn_stop.cancelled() => continue,
                    _ = worker_stop.cancelled() => return Ok(()),
                    result = self.transcriber.transcribe(&wav) => result?,
                };
                if !ticket.is_current() || transcript.text.trim().is_empty() {
                    continue;
                }
                let audio = tokio::select! {
                    biased;
                    _ = turn_stop.cancelled() => continue,
                    _ = worker_stop.cancelled() => return Ok(()),
                    result = self.conversation.reply(&ticket, &transcript) => result?,
                };
                if !ticket.is_current() {
                    continue;
                }
                if let Some(audio) = audio {
                    let playing_token = turn_stop.child_token();
                    worker_playing
                        .lock()
                        .expect("playback window poisoned")
                        .active = true;
                    let _playing = PlaybackState(Arc::clone(&worker_playing));
                    tokio::select! {
                        biased;
                        _ = turn_stop.cancelled() => continue,
                        result = self.playback.play(audio, playing_token) => result?,
                    }
                }
            }
            Ok::<(), SpeechSessionError>(())
        });
        let mut worker = tokio_util::task::AbortOnDropHandle::new(worker);
        let mut worker_finished = None;
        let mut controls_open = true;
        let mut previous = gate.snapshot(Instant::now());
        segmenter.set_mode(if previous.mode == InputMode::PushToTalk {
            SpeechMode::PushToTalk
        } else {
            SpeechMode::Automatic
        });
        let capture_result: Result<(), SpeechSessionError> = async { loop {
            let state = gate.snapshot(Instant::now());
                if state.mode == InputMode::PushToTalk && !segmenter.has_ptt_policy() {
                    return Err(SegmentError::InvalidConfiguration.into());
                }
            if state.epoch != previous.epoch {
                stream.invalidate();
                if !(state.manual_active && !previous.manual_active && state.mode == previous.mode) {
                    segmenter.reset();
                }
            }
            segmenter.set_mode(if state.mode == InputMode::PushToTalk { SpeechMode::PushToTalk } else { SpeechMode::Automatic });
            if state.manual_active != previous.manual_active && state.permitted {
                let segment = match state.mode {
                    InputMode::PushToTalk => segmenter.set_pressed(state.manual_active),
                    InputMode::Radio if !state.manual_active && state.epoch == previous.epoch => segmenter.finish_input(),
                    _ => None,
                };
                if let Some(segment) = segment {
                    queue_segment(&send, &stream, segment)?;
                }
            }
            previous = state;
            let deadline = gate.next_deadline();
            tokio::select! {
                biased;
                _ = stop.cancelled() => break Ok(()),
                result = &mut worker => {
                    worker_finished = Some(result);
                    break Ok(());
                }
                control = controls.recv(), if controls_open => {
                    match control {
                        Some(SpeechControl::Manual(event))
                            if matches!(route.source, RequestSource::Client(id) if id != event.session) => {}
                        Some(SpeechControl::SourceDisconnected(source))
                            if route.source == RequestSource::Client(source) => {
                            stop.cancel();
                            break Ok(());
                        }
                        Some(command) => gate.apply(command, Instant::now())?,
                        None => {
                            controls_open = false;
                            if state.mode != InputMode::Automatic {
                                gate.apply(SpeechControl::SetEnabled(false), Instant::now())?;
                            }
                        }
                    }
                }
                _ = async {
                    if let Some(deadline) = deadline { sleep_until(deadline).await }
                    else { std::future::pending::<()>().await }
                } => {}
                frame = capture.next_frame() => {
                    match frame {
                        Ok(Some(frame)) => {
                            let local_playback = {
                                let window = playing.lock().expect("playback window poisoned");
                                window.active || window.until.is_some_and(|until| until > Instant::now())
                            };
                            if !state.permitted || local_playback {
                                segmenter.discard_playback_frame()?;
                                continue;
                            }
                            if state.mode == InputMode::Radio && !state.allowed {
                                segmenter.buffer_idle_frame(frame)?;
                                continue;
                            }
                            match segmenter.process(frame) {
                                Ok(Some(segment)) => {
                                    if let Err(error) = queue_segment(&send, &stream, segment) {
                                        break Err(error);
                                    }
                                }
                                Ok(None) => {}
                                Err(error) => break Err(error.into()),
                            }
                        }
                        Ok(None) => {
                            if let Some(segment) = segmenter.flush()
                                && let Err(error) = queue_segment(&send, &stream, segment) { break Err(error); }
                            break Ok(());
                        }
                        Err(error) => break Err(error.into()),
                    }
                }
            }
        }}.await;
        drop(capture);
        drop(send);
        if capture_result.is_err() || stop.is_cancelled() {
            stop.cancel();
            worker.abort();
        }
        let drained = match worker_finished {
            Some(result) => result,
            None => {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => {
                        worker.abort();
                        worker.await
                    }
                    result = &mut worker => result,
                    _ = tokio::time::sleep(DRAIN_TIMEOUT) => {
                        worker.abort();
                        let _ = worker.await;
                        return Err(SpeechSessionError::Task(
                            "speech dispatch did not finish before the drain deadline".into(),
                        ));
                    }
                }
            }
        };
        let dispatch = match drained {
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
        score::ScoreError,
        segment::{SegmentConfig, SpeechMode},
        vad::{FRAME_SAMPLES, MonoPcm16Frame, VoiceDecision},
    };
    use std::{sync::Mutex, time::Duration};

    struct Gate;
    impl VoiceGate for Gate {
        fn inspect(&mut self, f: &MonoPcm16Frame) -> Result<VoiceDecision, ScoreError> {
            Ok(VoiceDecision {
                probability: f32::from(f[0] != 0),
                speech: f[0] != 0,
            })
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

    fn test_session(delivered: Arc<Mutex<Vec<String>>>) -> SpeechInputHandler {
        SpeechInputHandler::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Presenter,
            },
            Arc::new(Asr),
            Arc::new(Chat(delivered)),
            Arc::new(SilentPlayback),
        )
    }

    fn short_segmenter() -> SpeechSegmenter<Gate> {
        SpeechSegmenter::new(
            Gate,
            SegmentConfig {
                pre_roll_samples: FRAME_SAMPLES,
                start_voice_samples: FRAME_SAMPLES,
                end_silence_samples: FRAME_SAMPLES,
                min_segment_samples: FRAME_SAMPLES,
                max_segment_samples: 8 * FRAME_SAMPLES,
                ..SegmentConfig::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn input_owner_rejects_another_handler_before_capture_opens() {
        let owner = SpeechInputOwner::new();
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let first = test_session(Arc::clone(&delivered))
            .with_owner(&owner)
            .unwrap();
        assert!(matches!(
            test_session(Arc::clone(&delivered)).with_owner(&owner.clone()),
            Err(SpeechSessionError::InputBusy)
        ));
        drop(first);
        assert!(test_session(delivered).with_owner(&owner).is_ok());
    }

    #[tokio::test]
    async fn abort_releases_the_input_owner_after_the_worker_is_dropped() {
        let owner = SpeechInputOwner::new();
        let session = test_session(Arc::new(Mutex::new(Vec::new())))
            .with_owner(&owner)
            .unwrap();
        let (frames, receive_frames) = mpsc::channel(2);
        let (_controls, receive_controls) = mpsc::channel(2);
        let task = tokio::spawn(session.run(
            ControlledCapture(receive_frames),
            short_segmenter(),
            receive_controls,
            CancellationToken::new(),
        ));
        capture_barrier(&frames).await;
        assert!(matches!(owner.claim(), Err(SpeechSessionError::InputBusy)));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(owner.claim().is_ok());
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
            pre_roll_samples: 3 * crate::audio::vad::FRAME_SAMPLES,
            start_voice_samples: 2 * crate::audio::vad::FRAME_SAMPLES,
            end_silence_samples: 3 * crate::audio::vad::FRAME_SAMPLES,
            min_segment_samples: 2 * crate::audio::vad::FRAME_SAMPLES,
            max_segment_samples: 25 * crate::audio::vad::FRAME_SAMPLES,
        };
        let session = SpeechInputHandler::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Presenter,
            },
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

    struct FailingGate;
    impl VoiceGate for FailingGate {
        fn inspect(&mut self, _: &MonoPcm16Frame) -> Result<VoiceDecision, ScoreError> {
            Err(ScoreError::Provider("detector failed".into()))
        }
        fn reset(&mut self) {}
    }

    #[tokio::test]
    async fn provider_failure_ends_the_session_through_the_normal_shutdown_path() {
        let session = SpeechInputHandler::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Presenter,
            },
            Arc::new(Asr),
            Arc::new(Chat(Arc::new(Mutex::new(Vec::new())))),
            Arc::new(SilentPlayback),
        );
        let config = SegmentConfig {
            mode: SpeechMode::Automatic,
            ..SegmentConfig::default()
        };
        let (_send, controls) = mpsc::channel(2);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            session.run(
                Capture(vec![[100; FRAME_SAMPLES]; 3]),
                SpeechSegmenter::new(FailingGate, config).unwrap(),
                controls,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("a provider failure must not hang shutdown");
        assert!(matches!(result, Err(SpeechSessionError::Segment(_))));
    }

    struct HangingTranscriber;
    impl SpeechTranscriber for HangingTranscriber {
        fn transcribe<'a>(&'a self, _: &'a [u8]) -> SpeechFuture<'a, Transcript> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_asr_cannot_hold_shutdown_open() {
        let session = SpeechInputHandler::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Presenter,
            },
            Arc::new(HangingTranscriber),
            Arc::new(Chat(Arc::new(Mutex::new(Vec::new())))),
            Arc::new(SilentPlayback),
        );
        let config = SegmentConfig {
            mode: SpeechMode::Automatic,
            pre_roll_samples: 0,
            start_voice_samples: FRAME_SAMPLES,
            end_silence_samples: FRAME_SAMPLES,
            min_segment_samples: FRAME_SAMPLES,
            max_segment_samples: 8 * FRAME_SAMPLES,
        };
        let (_send, controls) = mpsc::channel(2);
        let result = session
            .run(
                Capture(vec![[100; FRAME_SAMPLES]; 2]),
                SpeechSegmenter::new(Gate, config).unwrap(),
                controls,
                CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(result, Err(SpeechSessionError::Task(message)) if message.contains("drain deadline")),
            "stalled dispatch must hit the drain deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stop_after_eof_cancels_drain_without_waiting_for_deadline() {
        let session = SpeechInputHandler::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Presenter,
            },
            Arc::new(HangingTranscriber),
            Arc::new(Chat(Arc::new(Mutex::new(Vec::new())))),
            Arc::new(SilentPlayback),
        );
        let segmenter = SpeechSegmenter::new(
            Gate,
            SegmentConfig {
                min_segment_samples: FRAME_SAMPLES,
                start_voice_samples: FRAME_SAMPLES,
                ..SegmentConfig::default()
            },
        )
        .unwrap();
        let (_send, controls) = mpsc::channel(2);
        let stop = CancellationToken::new();
        let task = tokio::spawn(session.run(
            Capture(vec![[1; FRAME_SAMPLES]]),
            segmenter,
            controls,
            stop.clone(),
        ));
        tokio::task::yield_now().await;
        stop.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
    }

    enum CaptureCommand {
        Frame(Box<MonoPcm16Frame>),
        Barrier(tokio::sync::oneshot::Sender<()>),
        Disconnected,
    }

    struct ControlledCapture(mpsc::Receiver<CaptureCommand>);

    impl AudioCapture for ControlledCapture {
        fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
            Box::pin(async {
                loop {
                    match self.0.recv().await {
                        Some(CaptureCommand::Frame(frame)) => return Ok(Some(*frame)),
                        Some(CaptureCommand::Barrier(ack)) => {
                            let _ = ack.send(());
                        }
                        Some(CaptureCommand::Disconnected) => {
                            return Err(DeviceError::Stopped("test device unplugged".into()));
                        }
                        None => return Ok(None),
                    }
                }
            })
        }
    }

    struct StalledAsr {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        started: tokio::sync::Notify,
        completion: Mutex<Option<tokio::sync::oneshot::Receiver<Transcript>>>,
    }

    impl SpeechTranscriber for StalledAsr {
        fn transcribe<'a>(&'a self, _: &'a [u8]) -> SpeechFuture<'a, Transcript> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let completion = self.completion.lock().unwrap().take().unwrap();
                self.started.notify_one();
                Ok(completion.await.expect("test completion closed"))
            })
        }
    }

    async fn capture_barrier(send: &mpsc::Sender<CaptureCommand>) {
        let (ack, received) = tokio::sync::oneshot::channel();
        send.send(CaptureCommand::Barrier(ack)).await.unwrap();
        received.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn ptt_expiry_discards_partial_audio_and_release_tail_keeps_next_turn() {
        use crate::audio::{input_gate::ManualCapture, segment::PttPolicy};
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let session = test_session(Arc::clone(&delivered));
        let mut segmenter = short_segmenter()
            .with_ptt_policy(PttPolicy {
                pre_roll_samples: 0,
                tail_samples: FRAME_SAMPLES,
                min_voiced_samples: FRAME_SAMPLES,
                require_release_after_max: true,
            })
            .unwrap();
        segmenter.set_mode(SpeechMode::PushToTalk);
        let (frames, receive_frames) = mpsc::channel(4);
        let (controls, receive_controls) = mpsc::channel(4);
        let task = tokio::spawn(session.run(
            ControlledCapture(receive_frames),
            segmenter,
            receive_controls,
            CancellationToken::new(),
        ));
        let mut press = ManualCapture {
            session: Uuid::new_v4(),
            generation: 1,
            active: true,
            cancelled: false,
            renew: false,
        };
        controls.send(SpeechControl::Manual(press)).await.unwrap();
        frames
            .send(CaptureCommand::Frame(Box::new([1; FRAME_SAMPLES])))
            .await
            .unwrap();
        capture_barrier(&frames).await;
        tokio::time::advance(Duration::from_secs(3)).await;
        capture_barrier(&frames).await;
        press.active = false;
        controls.send(SpeechControl::Manual(press)).await.unwrap();
        capture_barrier(&frames).await;
        assert!(delivered.lock().unwrap().is_empty());
        press.generation = 2;
        press.active = true;
        controls.send(SpeechControl::Manual(press)).await.unwrap();
        frames
            .send(CaptureCommand::Frame(Box::new([2; FRAME_SAMPLES])))
            .await
            .unwrap();
        capture_barrier(&frames).await;
        press.active = false;
        controls.send(SpeechControl::Manual(press)).await.unwrap();
        capture_barrier(&frames).await;
        assert!(delivered.lock().unwrap().is_empty());
        frames
            .send(CaptureCommand::Frame(Box::new([0; FRAME_SAMPLES])))
            .await
            .unwrap();
        drop(frames);
        task.await.unwrap().unwrap();
        assert_eq!(&*delivered.lock().unwrap(), &["hello from microphone"]);
    }

    #[tokio::test]
    async fn disconnect_cancels_pending_asr_and_explicit_restart_uses_a_fresh_stream() {
        let owner = SpeechInputOwner::new();
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let (complete, completion) = tokio::sync::oneshot::channel();
        let asr = Arc::new(StalledAsr {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            started: tokio::sync::Notify::new(),
            completion: Mutex::new(Some(completion)),
        });
        let session = SpeechInputHandler::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Presenter,
            },
            Arc::clone(&asr) as Arc<dyn SpeechTranscriber>,
            Arc::new(Chat(Arc::clone(&delivered))),
            Arc::new(SilentPlayback),
        )
        .with_owner(&owner)
        .unwrap();
        let (frames, receive_frames) = mpsc::channel(4);
        let (_controls, receive_controls) = mpsc::channel(2);
        let task = tokio::spawn(session.run(
            ControlledCapture(receive_frames),
            short_segmenter(),
            receive_controls,
            CancellationToken::new(),
        ));
        for sample in [1, 0] {
            frames
                .send(CaptureCommand::Frame(Box::new([sample; FRAME_SAMPLES])))
                .await
                .unwrap();
        }
        asr.started.notified().await;
        frames.send(CaptureCommand::Disconnected).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            result,
            Err(SpeechSessionError::Audio(DeviceError::Stopped(_)))
        ));
        assert!(
            complete
                .send(Transcript {
                    text: "late unplugged audio".into(),
                    language: None
                })
                .is_err()
        );
        assert!(delivered.lock().unwrap().is_empty());
        let session = test_session(Arc::clone(&delivered))
            .with_owner(&owner)
            .unwrap();
        let (_controls, receive_controls) = mpsc::channel(1);
        session
            .run(
                Capture(vec![[1; FRAME_SAMPLES], [0; FRAME_SAMPLES]]),
                short_segmenter(),
                receive_controls,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(&*delivered.lock().unwrap(), &["hello from microphone"]);
    }

    async fn invalidate_with_queued_audio(control: SpeechControl) {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (complete, completion) = tokio::sync::oneshot::channel();
        let asr = Arc::new(StalledAsr {
            calls: Arc::clone(&calls),
            started: tokio::sync::Notify::new(),
            completion: Mutex::new(Some(completion)),
        });
        let session = SpeechInputHandler::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Client(Uuid::new_v4()),
            },
            Arc::clone(&asr) as Arc<dyn SpeechTranscriber>,
            Arc::new(Chat(Arc::clone(&delivered))),
            Arc::new(SilentPlayback),
        );
        let (frames, capture) = mpsc::channel(16);
        let (controls, receive_controls) = mpsc::channel(2);
        let segmenter = SpeechSegmenter::new(
            Gate,
            SegmentConfig {
                mode: SpeechMode::Automatic,
                pre_roll_samples: crate::audio::vad::FRAME_SAMPLES,
                start_voice_samples: crate::audio::vad::FRAME_SAMPLES,
                end_silence_samples: crate::audio::vad::FRAME_SAMPLES,
                min_segment_samples: crate::audio::vad::FRAME_SAMPLES,
                max_segment_samples: 8 * crate::audio::vad::FRAME_SAMPLES,
            },
        )
        .unwrap();
        let task = tokio::spawn(session.run(
            ControlledCapture(capture),
            segmenter,
            receive_controls,
            CancellationToken::new(),
        ));
        for sample in [1, 0] {
            frames
                .send(CaptureCommand::Frame(Box::new([sample; FRAME_SAMPLES])))
                .await
                .unwrap();
        }
        asr.started.notified().await;
        // Active ASR is blocked; the next two complete segments fill the dispatch queue.
        for sample in [2, 0, 3, 0] {
            frames
                .send(CaptureCommand::Frame(Box::new([sample; FRAME_SAMPLES])))
                .await
                .unwrap();
        }
        capture_barrier(&frames).await;
        controls.send(control).await.unwrap();
        // Control has priority over capture; this acknowledgement observes its application.
        capture_barrier(&frames).await;
        let _ = complete.send(Transcript {
            text: "late old transcript".into(),
            language: Some("en".into()),
        });
        drop(frames);
        task.await.unwrap().unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "invalidated queued audio reached ASR"
        );
        assert!(
            delivered.lock().unwrap().is_empty(),
            "late transcript reached chat"
        );
    }

    #[tokio::test]
    async fn reset_discards_two_queued_segments_and_late_asr() {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            invalidate_with_queued_audio(SpeechControl::Reset),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn mute_discards_two_queued_segments_and_late_asr() {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            invalidate_with_queued_audio(SpeechControl::SetEnabled(false)),
        )
        .await
        .unwrap();
    }

    #[test]
    fn ticket_admission_is_once_only_and_retains_client_route() {
        let route = SpeechRoute {
            character: CharacterId::Crazy,
            source: RequestSource::Client(Uuid::new_v4()),
        };
        let stream = SpeechStream::new(route, CancellationToken::new());
        let old = stream.ticket();
        let first = stream.ticket();
        assert_eq!(first.stream().route, route);
        assert_ne!(old.utterance(), first.utterance());
        assert_eq!(first.admit(|| 42), Some(42));
        assert_eq!(first.admit(|| 43), None);
        stream.invalidate();
        assert!(!old.is_current());
        assert_eq!(old.admit(|| panic!("stale admission")), None);
        let current = stream.ticket();
        assert_eq!(old.stream().stream, current.stream().stream);
        assert_ne!(old.stream().generation, current.stream().generation);
        assert_eq!(current.stream().route, route);
        assert_eq!(current.admit(|| 44), Some(44));
    }

    #[test]
    fn stream_drop_cancels_tickets_after_admission_panics_without_double_panic() {
        let stream = SpeechStream::new(
            SpeechRoute {
                character: CharacterId::Crazy,
                source: RequestSource::Presenter,
            },
            CancellationToken::new(),
        );
        let ticket = stream.ticket();
        let guard = StreamLifetime(stream);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = guard;
            ticket.admit(|| panic!("controlled admission panic"));
        }));
        assert!(unwind.is_err());
        assert!(ticket.stop.is_cancelled());
    }
}
