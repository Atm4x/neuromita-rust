use std::sync::Arc;

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
mod legacy;
pub mod selection;
pub mod storage;
pub mod view;

use legacy::{legacy_event, make_draft};
use selection::{SessionWorkspace, TimelineSelection, TimelineTurn};
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
        Self {
            branch,
            store: Box::new(store),
            selection_updates: watch::channel(workspace.selected().expect("selection set")).0,
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
                Ok(Self {
                    branch: selection.branch_id,
                    store: Box::new(store),
                    workspace: SessionWorkspace::from_selection(selection),
                    selection_updates: watch::channel(selection).0,
                    selection_repository: Some(selection_repository),
                })
            }
        }
    }

    pub(crate) fn capture_turn(
        &self,
        character: CharacterId,
        trigger: EventId,
    ) -> Result<TimelineTurn, domain::TimelineError> {
        self.workspace
            .capture(self.store.as_ref(), character, Some(trigger))
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
    ) -> Result<TimelineContextSnapshot, String> {
        let anchor = match position {
            ContextPosition::Latest => self.anchor(),
            ContextPosition::At(at) => at,
        };
        let history = self.history_at(character, anchor)?;
        let view = TimelineMessageQueries::capture(
            self.store.as_ref(),
            self.branch,
            character,
            Some(anchor.through),
        )
        .map_err(|error| error.to_string())?;
        Ok(TimelineContextSnapshot { history, view })
    }

    /// Count accepted replies in canonical history, independent of what the
    /// character currently remembers or what the UI projection hides.
    /// The value is branch-prefix scoped and survives context resets and reopen.
    pub(crate) fn accepted_replies_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<u64, domain::TimelineError> {
        if at.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "context belongs to another timeline branch",
            ));
        }
        if at.through > self.store.branch(self.branch)?.head_sequence {
            return Err(domain::TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        let mut count = 0_u64;
        let actor = character.to_string();
        for fact in self.store.world(self.branch)? {
            if fact.sequence > at.through {
                break;
            }
            if fact.draft.kind == EventKind::CharacterMessage
                && fact.draft.actor.as_deref() == Some(actor.as_str())
                && fact
                    .draft
                    .payload
                    .get("request_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| Uuid::parse_str(id).is_ok())
            {
                count = count
                    .checked_add(1)
                    .ok_or(domain::TimelineError::Invalid("reply count overflow"))?;
            }
        }
        Ok(count)
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

    pub(crate) fn history_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<HistoryView, String> {
        if at.branch_id != self.branch.0 {
            return Err("invalid timeline position".into());
        }
        let known = self
            .store
            .view(self.branch, character, Some(at.through))
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
        self.commit(format!("observe:{}", id.0), draft, None)
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
        self.commit(operation_key, draft, None)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn append_proactive_trigger(
        &mut self,
        character: CharacterId,
        request: RequestId,
        id: EventId,
        text: Arc<str>,
        intent: TurnIntent,
    ) -> Result<(), String> {
        let mut draft = make_draft(
            id,
            EventKind::ProactiveOpportunity,
            character,
            text,
            Some(request),
        );
        match intent {
            TurnIntent::Reply => return Err("reply requires an admitted player message".into()),
            TurnIntent::Initiate => {
                draft
                    .payload
                    .insert("legacy_intent".into(), json!("initiate"));
            }
            TurnIntent::React(target) => {
                if !self
                    .store
                    .knows(self.branch, character, target, None)
                    .map_err(|error| error.to_string())?
                {
                    return Err("reaction target is unknown to the character".into());
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
        self.commit(format!("input:{}", request.0), draft, None)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn append_reply(
        &mut self,
        character: CharacterId,
        request: RequestId,
        trigger: EventId,
        context: ContextAnchor,
        turn: TimelineTurn,
        text: Arc<str>,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_turn(turn)?;
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
        self.commit(format!("reply:{}", request.0), draft, Some(turn))?;
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
    ) -> Result<(), domain::TimelineError> {
        let current = self.selection();
        if turn.is_some_and(|turn| turn.selection != current) {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let guard = AppendGuard {
            selection_generation: current.generation,
            context: turn.map(|turn| (turn.character, turn.context_epoch, turn.trigger)),
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
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[test]
    fn legacy_accept_does_not_answer_a_forgotten_trigger() {
        let mut timeline = SessionTimeline::new();
        let request = RequestId::new();
        let trigger = EventId::new();
        let at = timeline.anchor();
        timeline
            .append_player_message(CharacterId::Mita, request, trigger, Arc::from("hello"))
            .unwrap();
        let turn = timeline.capture_turn(CharacterId::Mita, trigger).unwrap();
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
                    at,
                    turn,
                    Arc::from("answer")
                )
                .is_err()
        );
        assert_eq!(timeline.anchor().through, 2);
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
