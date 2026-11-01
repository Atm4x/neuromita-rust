use std::{collections::HashMap, sync::Arc};

use tokio::sync::watch;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    identity::{CharacterId, RequestId},
    interaction::TurnIntent,
};

pub use crate::identity::EventId;
pub mod domain;
pub mod ingress;
mod legacy;
pub mod selection;
pub mod storage;
pub mod view;

use legacy::{legacy_event, make_draft};
use selection::{SessionWorkspace, TimelineAdmission, TimelineSelection, TimelineTurn};
use view::{MitaView, TimelineMessageQueries};

#[cfg(feature = "sqlite-system")]
use selection::TimelineSelectionRepository;

use domain::{
    AppendGuard, BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId,
    TimelineRepository, TimelineStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextAnchor {
    pub branch_id: Uuid,
    pub through: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPosition {
    Latest,
    At(ContextAnchor),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContextCaptureError {
    #[error("generation context belongs to a prior character knowledge epoch")]
    StaleKnowledge,
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventContent {
    PlayerMessage {
        text: Arc<str>,
    },
    Observation {
        text: Arc<str>,
    },
    Reaction {
        text: Arc<str>,
        target: EventId,
    },
    Initiation {
        text: Arc<str>,
    },
    CharacterReply {
        text: Arc<str>,
        reply_to: EventId,
        context: ContextAnchor,
    },
}

#[derive(Debug, Clone)]
pub struct TimelineEvent {
    pub id: EventId,
    pub sequence: u64,
    pub observer: CharacterId,
    pub request_id: Option<RequestId>,
    pub content: EventContent,
}

#[derive(Debug, Clone)]
pub struct HistoryView {
    pub at: ContextAnchor,
    pub events: Vec<TimelineEvent>,
}

/// Two read models bound to the same immutable timeline prefix.
/// The legacy view is retained for clients that have not migrated yet.
pub(crate) struct TimelineContextSnapshot {
    pub(crate) history: HistoryView,
    pub(crate) view: MitaView,
}

pub enum TimelineConfig {
    Transient,
    #[cfg(feature = "sqlite-system")]
    Sqlite {
        database: std::path::PathBuf,
        session: SessionId,
        root: BranchId,
    },
}

pub(crate) struct SessionTimeline {
    branch: BranchId,
    store: Box<dyn TimelineRepository + Send>,
    workspace: SessionWorkspace,
    selection_updates: watch::Sender<TimelineSelection>,
    context_updates: HashMap<CharacterId, watch::Sender<u64>>,
    #[cfg(feature = "sqlite-system")]
    selection_repository: Option<storage::SqliteTimelineRepository>,
}

impl SessionTimeline {
    pub(crate) fn new() -> Self {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store
            .create_session(SessionId::new(), branch)
            .expect("fresh session must create its branch");
        let mut workspace = SessionWorkspace::default();
        workspace
            .select(&store, branch, None)
            .expect("fresh branch exists");
        let context_updates =
            Self::context_channels(&store, branch).expect("fresh branch context epochs exist");
        Self {
            branch,
            store: Box::new(store),
            selection_updates: watch::channel(workspace.selected().expect("selection set")).0,
            context_updates,
            workspace,
            #[cfg(feature = "sqlite-system")]
            selection_repository: None,
        }
    }

    pub(crate) fn from_config(config: TimelineConfig) -> Result<Self, domain::TimelineError> {
        match config {
            TimelineConfig::Transient => Ok(Self::new()),
            #[cfg(feature = "sqlite-system")]
            TimelineConfig::Sqlite {
                database,
                session,
                root,
            } => {
                use storage::SqliteTimelineRepository;
                let mut store = SqliteTimelineRepository::open(&database)?;
                store.create_session(session, root)?;
                let mut selection_repository = SqliteTimelineRepository::open(database)?;
                let selection = match selection_repository.selection()? {
                    Some(selection) => selection,
                    None => selection_repository.select_branch(root, None)?,
                };
                if store.branch(selection.branch_id)?.session_id != session {
                    return Err(domain::TimelineError::Conflict);
                }
                let context_updates = Self::context_channels(&store, selection.branch_id)?;
                Ok(Self {
                    branch: selection.branch_id,
                    store: Box::new(store),
                    workspace: SessionWorkspace::from_selection(selection),
                    selection_updates: watch::channel(selection).0,
                    context_updates,
                    selection_repository: Some(selection_repository),
                })
            }
        }
    }

    pub(crate) fn capture_turn(
        &self,
        character: CharacterId,
        trigger: EventId,
        context: ContextAnchor,
    ) -> Result<TimelineTurn, domain::TimelineError> {
        if context.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "generation context belongs to another timeline branch",
            ));
        }
        self.workspace.capture_at(
            self.store.as_ref(),
            character,
            Some(trigger),
            context.through,
        )
    }

    pub(crate) fn assert_selection(
        &self,
        selection: TimelineSelection,
    ) -> Result<(), domain::TimelineError> {
        if self.selection() != selection {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        #[cfg(feature = "sqlite-system")]
        if let Some(repository) = &self.selection_repository
            && repository.selection()? != Some(selection)
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    pub(crate) fn admission(
        &self,
        character: CharacterId,
        selection: TimelineSelection,
    ) -> Result<TimelineAdmission, domain::TimelineError> {
        self.assert_selection(selection)?;
        Ok(TimelineAdmission {
            selection,
            context_epoch: self.store.context_epoch(self.branch, character)?,
        })
    }

    pub(crate) fn assert_admission(
        &self,
        character: CharacterId,
        admission: TimelineAdmission,
    ) -> Result<(), domain::TimelineError> {
        self.assert_selection(admission.selection)?;
        if self.store.context_epoch(self.branch, character)? != admission.context_epoch {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    pub(crate) fn verify_admitted_input(
        &self,
        character: CharacterId,
        request: RequestId,
        input: &crate::interaction::UserInput,
    ) -> Result<(), domain::TimelineError> {
        let fact = self.store.event(self.branch, input.event_id())?;
        if fact.draft.kind != EventKind::PlayerMessage
            || fact.draft.actor.as_deref() != Some("player")
            || fact.draft.payload.get("request_id").and_then(Value::as_str)
                != Some(request.0.to_string().as_str())
            || fact.draft.payload.get("content").and_then(Value::as_str)
                != Some(input.text().as_ref())
            || !self
                .store
                .knows(self.branch, character, input.event_id(), None)?
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    pub(crate) fn assert_turn(&self, turn: TimelineTurn) -> Result<(), domain::TimelineError> {
        self.workspace.assert_current(self.store.as_ref(), turn)?;
        #[cfg(feature = "sqlite-system")]
        if let Some(repository) = &self.selection_repository
            && repository.selection()? != Some(turn.selection)
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        Ok(())
    }

    fn context_channels(
        store: &dyn TimelineRepository,
        branch: BranchId,
    ) -> Result<HashMap<CharacterId, watch::Sender<u64>>, domain::TimelineError> {
        CharacterId::ALL
            .into_iter()
            .map(|character| {
                let epoch = store.context_epoch(branch, character)?;
                Ok((character, watch::channel(epoch).0))
            })
            .collect()
    }

    pub(crate) fn observe_context_epoch(&self, character: CharacterId) -> watch::Receiver<u64> {
        self.context_updates[&character].subscribe()
    }

    /// Reset is a semantic fact, not a request or a mutation of past events.
    /// The local character watcher is notified only after the guarded append commits.
    pub(crate) fn reset_character_context(
        &mut self,
        expected: TimelineSelection,
        character: CharacterId,
        id: EventId,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_selection(expected)?;
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.id = id;
        reset.observers.insert(character);
        self.store.append_guarded(
            self.branch,
            self.anchor().through,
            &format!("context-reset:{}", id.0),
            json!({"character": character.to_string()}),
            vec![reset],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        let epoch = self.store.context_epoch(self.branch, character)?;
        let sender = &self.context_updates[&character];
        if *sender.borrow() != epoch {
            sender.send_replace(epoch);
        }
        Ok(id)
    }

    pub(crate) fn observe_selection(&self) -> watch::Receiver<TimelineSelection> {
        self.selection_updates.subscribe()
    }

    pub(crate) fn selection(&self) -> TimelineSelection {
        self.workspace
            .selected()
            .expect("active timeline selection exists")
    }

    pub(crate) fn select_branch(
        &mut self,
        branch: BranchId,
        expected: TimelineSelection,
    ) -> Result<TimelineSelection, domain::TimelineError> {
        if self.store.branch(branch)?.session_id != self.store.branch(self.branch)?.session_id {
            return Err(domain::TimelineError::Conflict);
        }
        if self.workspace.selected() != Some(expected) {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let epochs = CharacterId::ALL
            .into_iter()
            .map(|character| {
                self.store
                    .context_epoch(branch, character)
                    .map(|epoch| (character, epoch))
            })
            .collect::<Result<Vec<_>, _>>()?;
        #[cfg(feature = "sqlite-system")]
        let new_selection = if let Some(repository) = &mut self.selection_repository {
            repository.select_branch(branch, Some(expected))?
        } else {
            self.workspace
                .select(self.store.as_ref(), branch, Some(expected))?
        };
        #[cfg(not(feature = "sqlite-system"))]
        let new_selection = self
            .workspace
            .select(self.store.as_ref(), branch, Some(expected))?;
        self.workspace.install_selection(new_selection);
        self.branch = branch;
        for (character, epoch) in epochs {
            self.context_updates[&character].send_replace(epoch);
        }
        self.selection_updates.send_replace(new_selection);
        Ok(new_selection)
    }

    #[cfg(test)]
    pub(crate) fn append_test_fact(
        &mut self,
        key: &str,
        draft: EventDraft,
    ) -> Result<(), domain::TimelineError> {
        let head = self.anchor().through;
        self.store
            .append(self.branch, head, key, json!({}), vec![draft])?;
        Ok(())
    }

    pub(crate) fn anchor(&self) -> ContextAnchor {
        ContextAnchor {
            branch_id: self.branch.0,
            through: self
                .store
                .branch(self.branch)
                .expect("owned branch exists")
                .head_sequence,
        }
    }

    pub(crate) fn capture_context(
        &self,
        character: CharacterId,
        position: ContextPosition,
    ) -> Result<TimelineContextSnapshot, ContextCaptureError> {
        let anchor = match position {
            ContextPosition::Latest => self.anchor(),
            ContextPosition::At(at) => at,
        };
        if anchor.branch_id != self.branch.0 {
            return Err(ContextCaptureError::Invalid(
                "generation context belongs to another timeline branch".into(),
            ));
        }
        if matches!(position, ContextPosition::At(_)) {
            let captured_epoch = self
                .store
                .context_epoch_at(self.branch, character, Some(anchor.through))
                .map_err(|error| ContextCaptureError::Invalid(error.to_string()))?;
            let current_epoch = self
                .store
                .context_epoch(self.branch, character)
                .map_err(|error| ContextCaptureError::Invalid(error.to_string()))?;
            if captured_epoch != current_epoch {
                return Err(ContextCaptureError::StaleKnowledge);
            }
        }
        self.inspect_context_at(character, anchor)
            .map_err(ContextCaptureError::Invalid)
    }

    /// Read a saved branch without selecting it or changing pending generations.
    pub(crate) fn inspect_context_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<TimelineContextSnapshot, String> {
        let branch = self
            .inspection_branch(at)
            .map_err(|error| error.to_string())?;
        let history = self.history_on_branch(character, branch, at)?;
        let view = TimelineMessageQueries::capture(
            self.store.as_ref(),
            branch,
            character,
            Some(at.through),
        )
        .map_err(|error| error.to_string())?;
        Ok(TimelineContextSnapshot { history, view })
    }

    fn inspection_branch(&self, at: ContextAnchor) -> Result<BranchId, domain::TimelineError> {
        let branch = BranchId(at.branch_id);
        let active_session = self.store.branch(self.branch)?.session_id;
        let info = self.store.branch(branch)?;
        if info.session_id != active_session {
            return Err(domain::TimelineError::Invalid(
                "context belongs to another session",
            ));
        }
        if at.through > info.head_sequence {
            return Err(domain::TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        Ok(branch)
    }

    /// Count accepted replies in canonical history, independent of what the
    /// character currently remembers or what the UI projection hides.
    /// The value is branch-prefix scoped and survives context resets and reopen.
    pub(crate) fn accepted_replies_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<u64, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        let facts = self.store.world(branch)?;
        domain::accepted_reply_count(
            facts.iter().take_while(|fact| fact.sequence <= at.through),
            character,
        )
    }

    pub(crate) fn reaction_cause_at(
        &self,
        character: CharacterId,
        intent: TurnIntent,
        at: ContextAnchor,
    ) -> Result<Option<domain::TimelineFact>, domain::TimelineError> {
        let TurnIntent::React(target) = intent else {
            return Ok(None);
        };
        if at.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "reaction context branch mismatch",
            ));
        }
        let fact = self.store.event(self.branch, target)?;
        if fact.sequence > at.through
            || !self
                .store
                .knows(self.branch, character, target, Some(at.through))?
        {
            return Err(domain::TimelineError::Invalid(
                "reaction cause was not known at captured context",
            ));
        }
        Ok(Some(fact))
    }

    #[cfg(test)]
    pub(crate) fn history(&self, character: CharacterId) -> HistoryView {
        self.history_at(character, self.anchor())
            .expect("current branch anchor is valid")
    }

    #[cfg(test)]
    pub(crate) fn history_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<HistoryView, String> {
        if at.branch_id != self.branch.0 {
            return Err("invalid timeline position".into());
        }
        self.history_on_branch(character, self.branch, at)
    }

    fn history_on_branch(
        &self,
        character: CharacterId,
        branch: BranchId,
        at: ContextAnchor,
    ) -> Result<HistoryView, String> {
        let known = self
            .store
            .view(branch, character, Some(at.through))
            .map_err(|error| error.to_string())?;
        Ok(HistoryView {
            at,
            events: known
                .iter()
                .filter_map(|item| legacy_event(&item.fact, character))
                .collect(),
        })
    }

    pub(crate) fn observe(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
    ) -> Result<EventId, String> {
        let mut draft = make_draft(id, EventKind::CharacterObservation, character, text, None);
        draft.actor = Some(character.to_string());
        self.commit(format!("observe:{}", id.0), draft, None, None)
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    pub(crate) fn at_event(&self, id: EventId) -> Option<ContextAnchor> {
        self.store
            .event(self.branch, id)
            .ok()
            .map(|event| ContextAnchor {
                branch_id: self.branch.0,
                through: event.sequence,
            })
    }

    pub(crate) fn ingest_semantic_events(
        &mut self,
        expected: TimelineSelection,
        batch: ingress::SemanticBatch,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        self.assert_selection(expected)?;
        let head = self.anchor().through;
        ingress::append_semantic_batch(
            self.store.as_mut(),
            self.branch,
            head,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
            batch,
        )
    }

    pub(crate) fn append_player_message(
        &mut self,
        character: CharacterId,
        request: RequestId,
        id: EventId,
        text: Arc<str>,
    ) -> Result<(), String> {
        self.write_player_message(character, id, text, Some(request))
    }

    pub(crate) fn publish_player_message(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
    ) -> Result<(), String> {
        self.write_player_message(character, id, text, None)
    }

    fn write_player_message(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
        request: Option<RequestId>,
    ) -> Result<(), String> {
        let mut draft = make_draft(id, EventKind::PlayerMessage, character, text, request);
        draft.actor = Some("player".to_string());
        let operation_key = request.map_or_else(
            || format!("message:{}", id.0),
            |request| format!("input:{}", request.0),
        );
        self.commit(operation_key, draft, None, None)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn append_proactive_trigger(
        &mut self,
        character: CharacterId,
        request: RequestId,
        id: EventId,
        text: Arc<str>,
        intent: TurnIntent,
        context: ContextAnchor,
    ) -> Result<(), domain::TimelineError> {
        if context.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "generation context belongs to another timeline branch",
            ));
        }
        let pinned =
            self.workspace
                .capture_at(self.store.as_ref(), character, None, context.through)?;
        self.assert_turn(pinned)?;
        let mut draft = make_draft(
            id,
            EventKind::ProactiveOpportunity,
            character,
            text,
            Some(request),
        );
        match intent {
            TurnIntent::Reply => {
                return Err(domain::TimelineError::Invalid(
                    "reply requires an admitted player message",
                ));
            }
            TurnIntent::Initiate => {
                draft
                    .payload
                    .insert("legacy_intent".into(), json!("initiate"));
            }
            TurnIntent::React(target) => {
                if !self.store.knows(self.branch, character, target, None)? {
                    return Err(domain::TimelineError::Invalid(
                        "reaction target is unknown to the character",
                    ));
                }
                draft.payload.insert("legacy_intent".into(), json!("react"));
                draft
                    .payload
                    .insert("target".into(), json!(target.0.to_string()));
                draft.links.push(CausalLink {
                    relation: CausalRelation::CausedBy,
                    event_id: target,
                });
            }
        }
        self.commit(format!("input:{}", request.0), draft, Some(pinned), None)
    }

    pub(crate) fn append_reply(
        &mut self,
        character: CharacterId,
        request: RequestId,
        trigger: EventId,
        turn: TimelineTurn,
        expected_revision: u64,
        text: Arc<str>,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_turn(turn)?;
        let context = ContextAnchor {
            branch_id: turn.selection.branch_id.0,
            through: turn.context_head,
        };
        if context.branch_id != self.branch.0 || context.through > self.anchor().through {
            return Err(domain::TimelineError::Invalid(
                "context does not belong to the active timeline",
            ));
        }
        let trigger_fact = self.store.event(self.branch, trigger)?;
        if !self.store.knows(self.branch, character, trigger, None)?
            || trigger_fact.draft.kind == EventKind::CharacterMessage
        {
            return Err(domain::TimelineError::Invalid(
                "reply target is not known to this character",
            ));
        }
        // Operation-key uniqueness is enforced atomically by the repository receipt.
        // Reading the complete world here would be both racy and O(history).
        let id = EventId::new();
        let mut draft = make_draft(
            id,
            EventKind::CharacterMessage,
            character,
            text,
            Some(request),
        );
        draft.actor = Some(character.to_string());
        draft.payload.insert("context".into(), json!(context));
        draft
            .payload
            .insert("reply_to".into(), json!(trigger.0.to_string()));
        if trigger_fact.draft.kind == EventKind::PlayerMessage {
            draft.links.push(CausalLink {
                relation: CausalRelation::ReplyTo,
                event_id: trigger,
            });
        } else if let Some(target) = trigger_fact
            .draft
            .payload
            .get("target")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
        {
            draft.links.push(CausalLink {
                relation: CausalRelation::ReactTo,
                event_id: EventId(target),
            });
            draft.links.push(CausalLink {
                relation: CausalRelation::CausedBy,
                event_id: trigger,
            });
        } else {
            draft.links.push(CausalLink {
                relation: CausalRelation::CausedBy,
                event_id: trigger,
            });
        }
        self.commit(
            format!("reply:{}", request.0),
            draft,
            Some(turn),
            Some((character, expected_revision)),
        )?;
        Ok(id)
    }

    pub(crate) fn events(&self) -> Vec<TimelineEvent> {
        self.store
            .world(self.branch)
            .expect("owned branch exists")
            .iter()
            .filter_map(|fact| {
                fact.draft
                    .observers
                    .iter()
                    .next()
                    .and_then(|&character| legacy_event(fact, character))
            })
            .collect()
    }

    fn commit(
        &mut self,
        operation_key: String,
        draft: EventDraft,
        turn: Option<TimelineTurn>,
        accepted_reply_revision: Option<(CharacterId, u64)>,
    ) -> Result<(), domain::TimelineError> {
        let current = self.selection();
        if turn.is_some_and(|turn| turn.selection != current) {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let guard = AppendGuard {
            selection_generation: current.generation,
            context: turn.map(|turn| (turn.character, turn.context_epoch, turn.trigger)),
            accepted_reply_revision,
        };
        let head = self.anchor().through;
        self.store.append_guarded(
            self.branch,
            head,
            &operation_key,
            json!({}),
            vec![draft],
            guard,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod inspection_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[test]
    fn legacy_accept_does_not_answer_a_forgotten_trigger() {
        let mut timeline = SessionTimeline::new();
        let request = RequestId::new();
        let trigger = EventId::new();
        timeline
            .append_player_message(CharacterId::Mita, request, trigger, Arc::from("hello"))
            .unwrap();
        let turn = timeline
            .capture_turn(CharacterId::Mita, trigger, timeline.anchor())
            .unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Mita);
        timeline
            .store
            .append(timeline.branch, 1, "reset", json!({}), vec![reset])
            .unwrap();
        assert!(
            timeline
                .append_reply(
                    CharacterId::Mita,
                    request,
                    trigger,
                    turn,
                    0,
                    Arc::from("answer")
                )
                .is_err()
        );
        assert_eq!(timeline.anchor().through, 2);
    }

    fn reset_and_relearn(
        store: &mut dyn TimelineRepository,
        branch: BranchId,
        original: EventId,
        key: &str,
    ) {
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Mita);
        let mut disclosure = EventDraft::new(EventKind::EventObserved);
        disclosure.observers.insert(CharacterId::Mita);
        disclosure.links.push(CausalLink {
            relation: CausalRelation::Observes,
            event_id: original,
        });
        let head = store.branch(branch).unwrap().head_sequence;
        store
            .append(branch, head, key, json!({}), vec![reset, disclosure])
            .unwrap();
        assert!(
            store
                .knows(branch, CharacterId::Mita, original, None)
                .unwrap()
        );
    }

    fn assert_stale_pin(timeline: &SessionTimeline, original: EventId, at: ContextAnchor) {
        let pin = timeline
            .capture_turn(CharacterId::Mita, original, at)
            .unwrap();
        assert_eq!(pin.context_head, at.through);
        assert_eq!(pin.context_epoch, 0);
        assert!(
            timeline
                .store
                .context_epoch(timeline.branch, CharacterId::Mita)
                .unwrap()
                > 0
        );
        assert_eq!(
            timeline.assert_turn(pin),
            Err(domain::TimelineError::ConcurrentWrite)
        );
    }

    #[test]
    fn captured_epoch_cannot_be_replaced_by_later_reset_and_relearning() {
        let mut timeline = SessionTimeline::new();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Mita, original, Arc::from("original"))
            .unwrap();
        let captured = timeline
            .capture_context(CharacterId::Mita, ContextPosition::Latest)
            .unwrap();
        let at = captured.history.at;
        assert_eq!(captured.view.messages()[0].content, "original");

        let branch = timeline.branch;
        reset_and_relearn(
            timeline.store.as_mut(),
            branch,
            original,
            "reset-and-relearn",
        );
        assert_stale_pin(&timeline, original, at);
        assert_eq!(captured.view.messages()[0].content, "original");

        let before = timeline.anchor().through;
        assert_eq!(
            timeline.append_proactive_trigger(
                CharacterId::Mita,
                RequestId::new(),
                EventId::new(),
                Arc::from("react to original"),
                TurnIntent::React(original),
                at,
            ),
            Err(domain::TimelineError::ConcurrentWrite)
        );
        assert_eq!(timeline.anchor().through, before);
    }

    #[test]
    fn newer_unrelated_facts_do_not_invalidate_a_captured_epoch() {
        let mut timeline = SessionTimeline::new();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Mita, original, Arc::from("original"))
            .unwrap();
        let at = timeline
            .capture_context(CharacterId::Mita, ContextPosition::Latest)
            .unwrap()
            .history
            .at;
        timeline
            .publish_player_message(CharacterId::Mita, EventId::new(), Arc::from("later"))
            .unwrap();
        let pin = timeline
            .capture_turn(CharacterId::Mita, original, at)
            .unwrap();
        assert_eq!(pin.context_head, at.through);
        assert_eq!(pin.context_epoch, 0);
        timeline.assert_turn(pin).unwrap();
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn external_sqlite_reset_and_relearning_cannot_repin_old_prompt() {
        let path =
            std::env::temp_dir().join(format!("neuromita-snapshot-epoch-{}.db", Uuid::new_v4()));
        let session = SessionId::new();
        let branch = BranchId::new();
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root: branch,
        })
        .unwrap();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Mita, original, Arc::from("original"))
            .unwrap();
        let at = timeline
            .capture_context(CharacterId::Mita, ContextPosition::Latest)
            .unwrap()
            .history
            .at;
        let mut external = storage::SqliteTimelineRepository::open(&path).unwrap();
        reset_and_relearn(&mut external, branch, original, "remote-reset-and-relearn");
        assert_stale_pin(&timeline, original, at);

        let before = timeline.anchor().through;
        assert_eq!(
            timeline.append_proactive_trigger(
                CharacterId::Mita,
                RequestId::new(),
                EventId::new(),
                Arc::from("late reaction"),
                TurnIntent::React(original),
                at,
            ),
            Err(domain::TimelineError::ConcurrentWrite)
        );
        assert_eq!(timeline.anchor().through, before);
        drop(external);
        drop(timeline);
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn standalone_message_remains_durable_without_request_receipt() {
        let path = std::env::temp_dir().join(format!(
            "neuromita-standalone-message-{}.db",
            uuid::Uuid::new_v4()
        ));
        let session = SessionId::new();
        let root = BranchId::new();
        let id = EventId::new();
        for reopen in [false, true] {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            if !reopen {
                timeline
                    .publish_player_message(CharacterId::Mita, id, Arc::from("independent input"))
                    .unwrap();
            }
            let events = timeline.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].id, id);
            assert_eq!(events[0].request_id, None);
            let captured = timeline
                .capture_context(CharacterId::Mita, ContextPosition::Latest)
                .unwrap();
            assert_eq!(captured.view.messages()[0].content, "independent input");
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn legacy_api_is_backed_by_the_only_canonical_event_store() {
        let mut timeline = SessionTimeline::new();
        let request = RequestId::new();
        let event = EventId::new();
        timeline
            .append_player_message(CharacterId::Mita, request, event, Arc::from("hello"))
            .unwrap();
        assert_eq!(timeline.store.world(timeline.branch).unwrap().len(), 1);
        assert_eq!(timeline.events().len(), 1);
        assert_eq!(timeline.history(CharacterId::Mita).events.len(), 1);
        assert!(timeline.history(CharacterId::KindMita).events.is_empty());
    }
}
