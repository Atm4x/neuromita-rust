use std::{fmt, sync::Arc};

use thiserror::Error;
use uuid::Uuid;

use crate::{
    character::{AcceptedResponse, CharacterId},
    generation::{GenerationError, GenerationPreset},
    identity::RequestId,
    interaction::{ReplyTarget, TurnIntent, UserInput},
    timeline::{ContextPosition, EventId, conversation::ConversationAudience},
    voice::VoiceError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestSource {
    Presenter,
    Client(Uuid),
}

pub trait GenerationCommand: Send + Sync + fmt::Debug {
    fn character_id(&self) -> CharacterId;
    fn preset(&self) -> &GenerationPreset;
    fn input(&self) -> &UserInput;
    fn source(&self) -> RequestSource;
    fn intent(&self) -> TurnIntent;
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
    at: ContextPosition,
    audience: ConversationAudience,
    input_audience: ConversationAudience,
    target: ReplyTarget,
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
            at: ContextPosition::Latest,
            audience: ConversationAudience::private_to(character_id),
            input_audience: ConversationAudience::player_to(character_id),
            target: ReplyTarget::Input,
        }
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

    /// Reply to an existing event observed by this character, without adding player speech.
    /// The input text becomes generation guidance rather than a conversation message.
    pub fn to_event(mut self, event: EventId) -> Self {
        self.target = ReplyTarget::Event(event);
        self
    }
}

impl GenerationCommand for ReplyRequest {
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
        TurnIntent::Reply(self.target)
    }
    fn at(&self) -> ContextPosition {
        self.at
    }
    fn audience(&self) -> &ConversationAudience {
        &self.audience
    }
    fn input_audience(&self) -> Option<&ConversationAudience> {
        match self.target {
            ReplyTarget::Input => Some(&self.input_audience),
            ReplyTarget::Event(_) => None,
        }
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
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.message = self.message.at(at);
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.message = self.message.with_audience(audience);
        self
    }
}
impl GenerationCommand for ReactRequest {
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
    fn at(&self) -> ContextPosition {
        GenerationCommand::at(&self.message)
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
    pub fn at(mut self, at: ContextPosition) -> Self {
        self.message = self.message.at(at);
        self
    }
    pub fn with_audience(mut self, audience: ConversationAudience) -> Self {
        self.message = self.message.with_audience(audience);
        self
    }
}
impl GenerationCommand for InitiateRequest {
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
    fn at(&self) -> ContextPosition {
        GenerationCommand::at(&self.message)
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
    #[error("reply target is not an observed conversation message")]
    InvalidReplyTarget,
}

#[derive(Debug, Error)]
pub enum ObserveError {
    #[error("request observation closed before completion")]
    ClosedBeforeCompletion,
}
