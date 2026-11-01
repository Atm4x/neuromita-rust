use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{
    CharacterContext, CharacterId, CharacterState, CharacterView, ResponseEffect,
    model::{Character, GenerationFocus, ReactionCause},
};
use crate::{
    identity::RequestId,
    interaction::{TurnIntent, UserInput},
    timeline::{
        ContextAnchor, ContextCaptureError, ContextPosition, EventId, GeneratedReply,
        SessionTimeline,
        conversation::ConversationAudience,
        selection::{TimelineAdmission, TimelineTurn},
    },
    voice::AudioArtifact,
};

#[derive(Debug, Clone, Error)]
pub enum CharacterError {
    #[error("character owner is unavailable")]
    Unavailable,
    #[error("character state changed since context preparation")]
    StaleState,
    #[error("character operation failed: {0}")]
    Internal(String),
}

#[derive(Debug, Clone)]
pub struct AcceptedResponse {
    pub text: Arc<str>,
    pub event_id: EventId,
    pub context: ContextAnchor,
    pub audio: Option<AudioArtifact>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AcceptanceContext {
    pub anchor: ContextAnchor,
    pub intent: TurnIntent,
    pub state_revision: u64,
    pub turn: TimelineTurn,
}

enum Command {
    Context {
        request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
        admission: TimelineAdmission,
        reply: oneshot::Sender<Result<CharacterContext, CharacterError>>,
    },
    Accept {
        request: RequestId,
        context: AcceptanceContext,
        text: Arc<str>,
        audio: Option<AudioArtifact>,
        effects: Vec<ResponseEffect>,
        audience: ConversationAudience,
        reply: oneshot::Sender<Result<AcceptedResponse, CharacterError>>,
    },
    State(oneshot::Sender<Result<CharacterState, CharacterError>>),
    View {
        at: ContextAnchor,
        reply: oneshot::Sender<Result<CharacterView, CharacterError>>,
    },
    Observe {
        input: UserInput,
        reply: oneshot::Sender<Result<EventId, CharacterError>>,
    },
}

#[derive(Clone)]
pub struct CharacterHandle {
    commands: mpsc::Sender<Command>,
}

impl CharacterHandle {
    pub async fn state(&self) -> Result<CharacterState, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::State(reply))
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    pub async fn get_context(&self, at: ContextAnchor) -> Result<CharacterView, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::View { at, reply })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }
    pub async fn observe(&self, input: UserInput) -> Result<EventId, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Observe { input, reply })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }
    pub(crate) async fn prepare(
        &self,
        request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
        admission: TimelineAdmission,
    ) -> Result<CharacterContext, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Context {
                request,
                guidance,
                intent,
                at,
                admission,
                reply,
            })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }

    pub(crate) async fn accept(
        &self,
        request: RequestId,
        context: AcceptanceContext,
        text: Arc<str>,
        audio: Option<AudioArtifact>,
        effects: Vec<ResponseEffect>,
        audience: ConversationAudience,
    ) -> Result<AcceptedResponse, CharacterError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command::Accept {
                request,
                context,
                text,
                audio,
                effects,
                audience,
                reply,
            })
            .await
            .map_err(|_| CharacterError::Unavailable)?;
        receive.await.map_err(|_| CharacterError::Unavailable)?
    }
}

#[derive(Clone)]
pub(crate) struct CharacterRegistry {
    handles: HashMap<CharacterId, CharacterHandle>,
}

impl CharacterRegistry {
    pub(crate) fn spawn(
        timeline: Arc<Mutex<SessionTimeline>>,
        stop: CancellationToken,
        capacity: usize,
    ) -> (Self, Vec<JoinHandle<()>>) {
        let mut handles = HashMap::new();
        let mut tasks = Vec::new();
        for id in CharacterId::ALL {
            let (send, receive) = mpsc::channel(capacity);
            let mut character = Character::create(id);
            character.state = {
                let timeline = timeline.lock().expect("timeline lock poisoned");
                state_at(&timeline, id, timeline.anchor()).expect("owned timeline is readable")
            };
            let handler = CharacterHandler {
                character,
                timeline: Arc::clone(&timeline),
            };
            handles.insert(id, CharacterHandle { commands: send });
            tasks.push(tokio::spawn(handler.run(receive, stop.clone())));
        }
        (Self { handles }, tasks)
    }

    pub(crate) fn get(&self, id: CharacterId) -> Option<CharacterHandle> {
        self.handles.get(&id).cloned()
    }
}

struct CharacterHandler {
    character: Character,
    timeline: Arc<Mutex<SessionTimeline>>,
}

impl CharacterHandler {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>, stop: CancellationToken) {
        loop {
            let command = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                command = commands.recv() => match command { Some(command) => command, None => break },
            };
            match command {
                Command::Context {
                    request,
                    guidance,
                    intent,
                    at,
                    admission,
                    reply,
                } => {
                    let _ =
                        reply.send(self.context_admitted(request, guidance, intent, at, admission));
                }
                Command::Accept {
                    request,
                    context,
                    text,
                    audio,
                    effects,
                    audience,
                    reply,
                } => {
                    let _ =
                        reply.send(self.accept(request, context, text, audio, effects, audience));
                }
                Command::State(reply) => {
                    let state = self
                        .timeline
                        .lock()
                        .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))
                        .and_then(|timeline| {
                            state_at(&timeline, self.character.id, timeline.anchor())
                        });
                    if let Ok(ref state) = state {
                        self.character.state = state.clone();
                    }
                    let _ = reply.send(state);
                }
                Command::View { at, reply } => {
                    let _ = reply.send(self.view(at));
                }
                Command::Observe { input, reply } => {
                    let result = self
                        .timeline
                        .lock()
                        .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))
                        .and_then(|mut timeline| {
                            timeline
                                .observe(
                                    self.character.id,
                                    input.event_id(),
                                    Arc::clone(input.text()),
                                )
                                .map_err(CharacterError::Internal)
                        });
                    let _ = reply.send(result);
                }
            }
        }
    }

    #[cfg(test)]
    fn context(
        &mut self,
        request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
    ) -> Result<CharacterContext, CharacterError> {
        let admission = {
            let timeline = self.timeline.lock().expect("timeline mutex poisoned");
            timeline
                .admission(self.character.id, timeline.selection())
                .unwrap()
        };
        self.context_admitted(request, guidance, intent, at, admission)
    }

    fn context_admitted(
        &mut self,
        _request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
        admission: TimelineAdmission,
    ) -> Result<CharacterContext, CharacterError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        timeline
            .assert_admission(self.character.id, admission)
            .map_err(|_| CharacterError::StaleState)?;
        let captured =
            timeline
                .capture_context(self.character.id, at)
                .map_err(|error| match error {
                    ContextCaptureError::StaleKnowledge => CharacterError::StaleState,
                    ContextCaptureError::Invalid(reason) => CharacterError::Internal(reason),
                })?;
        let anchor = captured.history.at;
        let state = state_at(&timeline, self.character.id, anchor)?;
        let reaction = timeline
            .reaction_cause_at(self.character.id, intent, anchor)
            .map_err(|error| CharacterError::Internal(error.to_string()))?
            .map(|fact| ReactionCause {
                event_id: fact.draft.id,
                kind: fact.draft.kind,
                description: fact
                    .draft
                    .payload
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .map(Arc::from),
            });
        self.character.state = state_at(&timeline, self.character.id, timeline.anchor())?;
        let turn = timeline
            .capture_turn(
                self.character.id,
                match intent {
                    TurnIntent::Reply(id) | TurnIntent::React(id) => Some(id),
                    TurnIntent::Initiate => None,
                },
                anchor,
            )
            .map_err(|error| CharacterError::Internal(error.to_string()))?;
        timeline
            .assert_turn(turn)
            .map_err(|_| CharacterError::StaleState)?;
        Ok(CharacterContext {
            character_id: self.character.id,
            description: Arc::clone(&self.character.description),
            prompt: Arc::clone(&self.character.prompt),
            state,
            history: captured.history,
            view: captured.view,
            current_input: guidance,
            focus: GenerationFocus { intent, reaction },
            anchor,
            turn,
        })
    }

    fn accept(
        &mut self,
        request: RequestId,
        context: AcceptanceContext,
        text: Arc<str>,
        audio: Option<AudioArtifact>,
        effects: Vec<ResponseEffect>,
        audience: ConversationAudience,
    ) -> Result<AcceptedResponse, CharacterError> {
        if !matches!(effects.as_slice(), [ResponseEffect::RecordAcceptedReply]) {
            return Err(CharacterError::Internal(
                "invalid reply effect batch".into(),
            ));
        }
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        self.character.state = state_at(&timeline, self.character.id, timeline.anchor())?;
        if context.state_revision != self.character.state.revision {
            return Err(CharacterError::StaleState);
        }
        let accepted_replies = self
            .character
            .state
            .accepted_replies
            .checked_add(1)
            .ok_or_else(|| CharacterError::Internal("reply counter overflow".into()))?;
        let revision = self
            .character
            .state
            .revision
            .checked_add(1)
            .ok_or_else(|| CharacterError::Internal("state revision overflow".into()))?;
        timeline
            .assert_turn(context.turn)
            .map_err(|_| CharacterError::StaleState)?;
        let event_id = timeline
            .append_reply(GeneratedReply {
                request,
                intent: context.intent,
                turn: context.turn,
                expected_revision: context.state_revision,
                text: Arc::clone(&text),
                audience,
            })
            .map_err(|error| match error {
                crate::timeline::domain::TimelineError::ConcurrentWrite => {
                    CharacterError::StaleState
                }
                other => CharacterError::Internal(other.to_string()),
            })?;
        self.character.state = CharacterState {
            accepted_replies,
            revision,
        };
        Ok(AcceptedResponse {
            text,
            event_id,
            context: context.anchor,
            audio,
        })
    }
    fn view(&self, at: ContextAnchor) -> Result<CharacterView, CharacterError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| CharacterError::Internal("timeline lock poisoned".into()))?;
        let captured = timeline
            .inspect_context_at(self.character.id, at)
            .map_err(CharacterError::Internal)?;
        Ok(CharacterView {
            character_id: self.character.id,
            description: Arc::clone(&self.character.description),
            prompt: Arc::clone(&self.character.prompt),
            state: state_at(&timeline, self.character.id, at)?,
            history: captured.history,
            view: captured.view,
        })
    }
}

fn state_at(
    timeline: &SessionTimeline,
    character: CharacterId,
    at: ContextAnchor,
) -> Result<CharacterState, CharacterError> {
    let replies = timeline
        .accepted_replies_at(character, at)
        .map_err(|error| CharacterError::Internal(error.to_string()))?;
    Ok(CharacterState {
        accepted_replies: replies,
        revision: replies,
    })
}

#[cfg(test)]
mod turn_tests {
    use super::*;
    use crate::timeline::domain::{BranchId, EventDraft, EventKind};

    fn handler() -> CharacterHandler {
        CharacterHandler {
            character: Character::create(CharacterId::Mita),
            timeline: Arc::new(Mutex::new(SessionTimeline::new())),
        }
    }

    fn prepare(handler: &mut CharacterHandler) -> (RequestId, CharacterContext) {
        let request = RequestId::new();
        let event = EventId::new();
        handler
            .timeline
            .lock()
            .unwrap()
            .publish_player_message(CharacterId::Mita, event, Arc::from("hello"))
            .unwrap();
        let context = handler
            .context(
                request,
                Arc::from(""),
                TurnIntent::Reply(event),
                ContextPosition::Latest,
            )
            .unwrap();
        (request, context)
    }

    fn acceptance(context: &CharacterContext) -> AcceptanceContext {
        AcceptanceContext {
            anchor: context.anchor,
            intent: context.focus.intent,
            state_revision: context.state.revision,
            turn: context.turn,
        }
    }

    fn accept(
        handler: &mut CharacterHandler,
        request: RequestId,
        context: AcceptanceContext,
    ) -> Result<AcceptedResponse, CharacterError> {
        handler.accept(
            request,
            context,
            Arc::from("answer"),
            None,
            vec![ResponseEffect::RecordAcceptedReply],
            ConversationAudience::private_to(CharacterId::Mita),
        )
    }

    #[test]
    fn reselect_same_branch_invalidates_pending_acceptance() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let current = timeline.selection();
            timeline
                .select_branch(BranchId(current.branch_id.0), current)
                .unwrap();
        }
        assert!(matches!(
            accept(&mut handler, request, token),
            Err(CharacterError::StaleState)
        ));
        assert_eq!(handler.character.state.accepted_replies, 0);
        assert_eq!(handler.timeline.lock().unwrap().events().len(), 1);
    }

    #[test]
    fn reset_invalidates_pending_turn_but_later_unrelated_fact_does_not() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            timeline
                .append_test_fact("unrelated", EventDraft::new(EventKind::UnityEvent))
                .unwrap();
        }
        assert!(accept(&mut handler, request, token).is_ok());
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let mut reset = EventDraft::new(EventKind::CharacterContextReset);
            reset.observers.insert(CharacterId::Mita);
            timeline.append_test_fact("reset", reset).unwrap();
        }
        assert!(matches!(
            accept(&mut handler, request, token),
            Err(CharacterError::StaleState)
        ));
    }

    #[test]
    fn accepted_state_survives_reset_and_hidden_history() {
        let mut handler = handler();
        let (request, context) = prepare(&mut handler);
        let reply = accept(&mut handler, request, acceptance(&context)).unwrap();
        let before_reset = handler.timeline.lock().unwrap().anchor();
        assert_eq!(
            state_at(
                &handler.timeline.lock().unwrap(),
                CharacterId::Mita,
                before_reset
            )
            .unwrap()
            .accepted_replies,
            1
        );
        {
            let mut timeline = handler.timeline.lock().unwrap();
            let mut hidden = EventDraft::new(EventKind::MessageHidden);
            hidden.observers.insert(CharacterId::Mita);
            hidden.payload.insert(
                "event_ids".into(),
                serde_json::json!([reply.event_id.0.to_string()]),
            );
            timeline.append_test_fact("hide-accepted", hidden).unwrap();
            let mut reset = EventDraft::new(EventKind::CharacterContextReset);
            reset.observers.insert(CharacterId::Mita);
            timeline
                .append_test_fact("reset-after-accepted", reset)
                .unwrap();
            let now = timeline.anchor();
            assert_eq!(
                timeline
                    .accepted_replies_at(CharacterId::Mita, now)
                    .unwrap(),
                1
            );
            assert_eq!(
                timeline
                    .accepted_replies_at(CharacterId::KindMita, now)
                    .unwrap(),
                0
            );
        }
        let (next_request, next_context) = prepare(&mut handler);
        assert_eq!(next_context.state.accepted_replies, 1);
        assert_eq!(next_context.state.revision, 1);
        assert_eq!(next_context.view.messages().len(), 1);
        assert_eq!(next_context.view.messages()[0].content, "hello");
        accept(&mut handler, next_request, acceptance(&next_context)).unwrap();
        assert_eq!(handler.character.state.accepted_replies, 2);
        assert_eq!(
            handler.view(before_reset).unwrap().state.accepted_replies,
            1
        );
    }

    #[test]
    fn reactive_context_includes_later_known_dialogue_and_world_knowledge() {
        use crate::timeline::domain::MessageRole;
        let mut handler = handler();
        let cause = EventId::new();
        {
            let mut timeline = handler.timeline.lock().unwrap();
            timeline
                .observe(CharacterId::Mita, cause, Arc::from("door opened"))
                .unwrap();
            timeline
                .publish_player_message(
                    CharacterId::Mita,
                    EventId::new(),
                    Arc::from("later, unrelated user input"),
                )
                .unwrap();
            let mut note = EventDraft::new(EventKind::SessionNote);
            note.observers.insert(CharacterId::Mita);
            note.payload
                .insert("content".into(), serde_json::json!("weather became rainy"));
            timeline.append_test_fact("world-note", note).unwrap();
        }
        let request = RequestId::new();
        let context = handler
            .context(
                request,
                Arc::from("react to the door"),
                TurnIntent::React(cause),
                ContextPosition::Latest,
            )
            .unwrap();
        assert_eq!(context.anchor.through, 3);
        assert_eq!(context.view.count(), 3);
        let messages = context.view.messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "door opened");
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content, "later, unrelated user input");
        assert_eq!(messages[2].role, MessageRole::System);
        assert_eq!(messages[2].content, "weather became rainy");
        assert_eq!(context.current_input.as_ref(), "react to the door");

        let earlier = handler
            .timeline
            .lock()
            .unwrap()
            .capture_context(
                CharacterId::Mita,
                ContextPosition::At(ContextAnchor {
                    branch_id: context.anchor.branch_id,
                    through: 1,
                }),
            )
            .unwrap();
        assert_eq!(earlier.view.messages().len(), 1);
    }

    #[test]
    fn reacting_to_cause_not_known_at_snapshot_has_no_side_effect() {
        let mut handler = handler();
        let cause = EventId::new();
        let old = handler.timeline.lock().unwrap().anchor();
        handler
            .timeline
            .lock()
            .unwrap()
            .observe(CharacterId::Mita, cause, Arc::from("door opened"))
            .unwrap();
        let result = handler.context(
            RequestId::new(),
            Arc::from("react"),
            TurnIntent::React(cause),
            ContextPosition::At(old),
        );
        assert!(matches!(result, Err(CharacterError::Internal(_))));
        assert_eq!(handler.timeline.lock().unwrap().anchor().through, 1);
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn persisted_accepted_state_survives_reopen_and_reset() {
        use crate::timeline::{TimelineConfig, domain::SessionId};
        let path =
            std::env::temp_dir().join(format!("neuromita-state-{}.db", uuid::Uuid::new_v4()));
        let root = BranchId::new();
        let session = SessionId::new();
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let mut handler = CharacterHandler {
                character: Character::create(CharacterId::Mita),
                timeline: Arc::new(Mutex::new(timeline)),
            };
            let (request, context) = prepare(&mut handler);
            accept(&mut handler, request, acceptance(&context)).unwrap();
            let mut timeline = handler.timeline.lock().unwrap();
            let mut reset = EventDraft::new(EventKind::CharacterContextReset);
            reset.observers.insert(CharacterId::Mita);
            timeline.append_test_fact("persisted-reset", reset).unwrap();
        }
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let mut handler = CharacterHandler {
                character: Character::create(CharacterId::Mita),
                timeline: Arc::new(Mutex::new(timeline)),
            };
            let (request, context) = prepare(&mut handler);
            assert_eq!(context.state.accepted_replies, 1);
            assert_eq!(context.view.count(), 1);
            accept(&mut handler, request, acceptance(&context)).unwrap();
            assert_eq!(handler.character.state.accepted_replies, 2);
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn prepared_context_uses_single_prefix_and_does_not_repeat_new_input() {
        let mut handler = handler();
        let original = EventId::new();
        {
            let mut timeline = handler.timeline.lock().unwrap();
            timeline
                .observe(CharacterId::Mita, original, Arc::from("seen by Mita"))
                .unwrap();
        }
        let input_id = EventId::new();
        handler
            .timeline
            .lock()
            .unwrap()
            .publish_player_message(CharacterId::Mita, input_id, Arc::from("new request"))
            .unwrap();
        let snapshot = handler
            .context(
                RequestId::new(),
                Arc::from(""),
                TurnIntent::Reply(input_id),
                ContextPosition::Latest,
            )
            .unwrap();
        assert_eq!(snapshot.anchor.through, 2);
        assert_eq!(snapshot.view.through, snapshot.anchor.through);
        assert_eq!(snapshot.history.at, snapshot.anchor);
        assert_eq!(snapshot.view.count(), 2);
        assert!(snapshot.view.contains(&original.0.to_string()));
        assert!(snapshot.view.contains(&input_id.0.to_string()));
        assert!(snapshot.current_input.is_empty());
        assert_eq!(handler.timeline.lock().unwrap().anchor().through, 2);

        let old = handler.view(snapshot.anchor).unwrap();
        assert_eq!(old.view.count(), 2);
        assert_eq!(old.view.through, snapshot.anchor.through);
        assert_eq!(old.history.at, snapshot.anchor);
        let first = handler.timeline.lock().unwrap().anchor();
        let empty = handler
            .view(ContextAnchor {
                through: 0,
                ..first
            })
            .unwrap();
        assert_eq!(empty.view.count(), 0);
        assert_eq!(empty.state.accepted_replies, 0);
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn concurrent_sqlite_selection_change_invalidates_pending_acceptance() {
        use crate::timeline::selection::TimelineSelectionRepository;
        use crate::timeline::{
            TimelineConfig, domain::SessionId, storage::SqliteTimelineRepository,
        };
        let path =
            std::env::temp_dir().join(format!("neuromita-turn-pin-{}.db", uuid::Uuid::new_v4()));
        let root = BranchId::new();
        let session = SessionId::new();
        let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        })
        .unwrap();
        let mut handler = CharacterHandler {
            character: Character::create(CharacterId::Mita),
            timeline: Arc::new(Mutex::new(timeline)),
        };
        let (request, context) = prepare(&mut handler);
        let token = acceptance(&context);
        let mut other = SqliteTimelineRepository::open(&path).unwrap();
        let previous = other.selection().unwrap().unwrap();
        other.select_branch(root, Some(previous)).unwrap();
        assert!(matches!(
            accept(&mut handler, request, token),
            Err(CharacterError::StaleState)
        ));
        drop(other);
        drop(handler);
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }
}
