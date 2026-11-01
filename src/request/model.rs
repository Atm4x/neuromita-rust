use std::{fmt, sync::Arc};

use thiserror::Error;
use uuid::Uuid;

use crate::{
    character::{AcceptedResponse, CharacterId},
    generation::{GenerationError, GenerationPreset},
    identity::RequestId,
    interaction::{TurnIntent, UserInput},
    timeline::{ContextPosition, EventId, conversation::ConversationAudience},
    voice::{VoiceError, VoicePlan},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestSource {
    Presenter,
    Client(Uuid),
}

pub trait Message: Send + Sync + fmt::Debug {
    fn character_id(&self) -> CharacterId;
    fn preset(&self) -> &GenerationPreset;
    fn input(&self) -> &UserInput;
    fn source(&self) -> RequestSource;
    fn intent(&self) -> TurnIntent;
    fn voice(&self) -> &VoicePlan;
    fn at(&self) -> ContextPosition;
    fn audience(&self) -> &ConversationAudience;
    fn input_audience(&self) -> Option<&ConversationAudience> {
        None
    }
}

#[derive(Debug)]
pub struct ReplyRequest {
    character_id: CharacterId,
    preset: GenerationPreset,
    input: UserInput,
    source: RequestSource,
    voice: VoicePlan,
    at: ContextPosition,
    audience: ConversationAudience,
    input_audience: ConversationAudience,
}

impl ReplyRequest {
    pub fn new(
        character_id: CharacterId,
        preset: GenerationPreset,
        input: UserInput,
        source: RequestSource,
    ) -> Self {
        Self {
            character_id,
            preset,
            input,
            source,
            voice: VoicePlan::Disabled,
            at: ContextPosition::Latest,
            audience: ConversationAudience::private_to(character_id),
            input_audience: ConversationAudience::player_to(character_id),
        }
    }
    pub fn with_voice(mut self, voice: VoicePlan) -> Self {
        self.voice = voice;
        self
    }
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.at = at;
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.audience = audience;
        self
    }
    pub fn with_input_audience(mut self, audience: ConversationAudience) -> Self {
        self.input_audience = audience;
        self
    }
}

impl Message for ReplyRequest {
    fn character_id(&self) -> CharacterId {
        self.character_id
    }
    fn preset(&self) -> &GenerationPreset {
        &self.preset
    }
    fn input(&self) -> &UserInput {
        &self.input
    }
    fn source(&self) -> RequestSource {
        self.source
    }
    fn intent(&self) -> TurnIntent {
        TurnIntent::Reply
    }
    fn voice(&self) -> &VoicePlan {
        &self.voice
    }
    fn at(&self) -> ContextPosition {
        self.at
    }
    fn audience(&self) -> &ConversationAudience {
        &self.audience
    }
    fn input_audience(&self) -> Option<&ConversationAudience> {
        Some(&self.input_audience)
    }
}

#[derive(Debug)]
pub struct ReactRequest {
    message: ReplyRequest,
    event: EventId,
}
impl ReactRequest {
    pub fn new(
        character: CharacterId,
        preset: GenerationPreset,
        event: EventId,
        instruction: UserInput,
        source: RequestSource,
    ) -> Self {
        Self {
            message: ReplyRequest::new(character, preset, instruction, source),
            event,
        }
    }
    pub fn with_voice(mut self, voice: VoicePlan) -> Self {
        self.message = self.message.with_voice(voice);
        self
    }
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.message = self.message.at(at);
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.message = self.message.with_audience(audience);
        self
    }
}
impl Message for ReactRequest {
    fn character_id(&self) -> CharacterId {
        self.message.character_id()
    }
    fn preset(&self) -> &GenerationPreset {
        self.message.preset()
    }
    fn input(&self) -> &UserInput {
        self.message.input()
    }
    fn source(&self) -> RequestSource {
        self.message.source()
    }
    fn intent(&self) -> TurnIntent {
        TurnIntent::React(self.event)
    }
    fn voice(&self) -> &VoicePlan {
        self.message.voice()
    }
    fn at(&self) -> ContextPosition {
        Message::at(&self.message)
    }
    fn audience(&self) -> &ConversationAudience {
        self.message.audience()
    }
}

#[derive(Debug)]
pub struct InitiateRequest {
    message: ReplyRequest,
}
impl InitiateRequest {
    pub fn new(
        character: CharacterId,
        preset: GenerationPreset,
        instruction: UserInput,
        source: RequestSource,
    ) -> Self {
        Self {
            message: ReplyRequest::new(character, preset, instruction, source),
        }
    }
    pub fn with_voice(mut self, voice: VoicePlan) -> Self {
        self.message = self.message.with_voice(voice);
        self
    }
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.message = self.message.at(at);
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.message = self.message.with_audience(audience);
        self
    }
}
impl Message for InitiateRequest {
    fn character_id(&self) -> CharacterId {
        self.message.character_id()
    }
    fn preset(&self) -> &GenerationPreset {
        self.message.preset()
    }
    fn input(&self) -> &UserInput {
        self.message.input()
    }
    fn source(&self) -> RequestSource {
        self.message.source()
    }
    fn intent(&self) -> TurnIntent {
        TurnIntent::Initiate
    }
    fn voice(&self) -> &VoicePlan {
        self.message.voice()
    }
    fn at(&self) -> ContextPosition {
        Message::at(&self.message)
    }
    fn audience(&self) -> &ConversationAudience {
        self.message.audience()
    }
}

#[derive(Debug, Clone)]
pub struct RequestMetadata {
    pub id: RequestId,
    pub character_id: CharacterId,
    pub source: RequestSource,
    pub intent: TurnIntent,
}

#[derive(Debug, Clone)]
pub enum RequestOutcome {
    Success(AcceptedResponse),
    TextGenerationFailed(GenerationError),
    VoiceGenerationFailed {
        response: AcceptedResponse,
        error: VoiceError,
    },
    InternalError(InternalFailure),
}

#[derive(Debug, Clone)]
pub struct InternalFailure {
    pub message: Arc<str>,
    pub partial_text: Option<Arc<str>>,
}
impl fmt::Display for InternalFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl From<Arc<str>> for InternalFailure {
    fn from(message: Arc<str>) -> Self {
        Self {
            message,
            partial_text: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProcessingStage {
    GeneratingText,
    GeneratingVoice { text: Arc<str> },
}

#[derive(Debug, Clone)]
pub enum RequestState {
    Pending,
    Processing(ProcessingStage),
    Finished(RequestOutcome),
    Cancelled,
}

impl RequestState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished(_) | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
pub struct RequestSnapshot {
    pub metadata: RequestMetadata,
    pub revision: u64,
    pub state: RequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectPhase {
    Working,
    Accepting,
    Applied,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelResult {
    Cancelled,
    Requested,
    TooLate,
    AlreadyFinished,
}

#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("ChatApi is not initialized")]
    Uninitialized,
    #[error("application is stopping")]
    Stopped,
    #[error("request queue is full")]
    QueueFull,
    #[error("request queue is closed")]
    QueueClosed,
    #[error("character is unavailable")]
    CharacterUnavailable,
    #[error("timeline could not accept the request input")]
    TimelineUnavailable,
}

#[derive(Debug, Error)]
pub enum ObserveError {
    #[error("request observation closed before completion")]
    ClosedBeforeCompletion,
}
