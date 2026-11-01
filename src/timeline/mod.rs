use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    identity::{CharacterId, RequestId},
    interaction::TurnIntent,
};

pub use crate::identity::EventId;
pub mod domain;
pub mod selection;
pub mod storage;

use selection::{SessionWorkspace, TimelineSelection, TimelineTurn};

#[cfg(feature = "sqlite-system")]
use selection::TimelineSelectionRepository;

use domain::{
    BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId, TimelineFact,
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
        let mut draft = self.make_draft(id, EventKind::CharacterObservation, character, text, None);
        draft.actor = Some(character.to_string());
        self.commit(format!("observe:{}", id.0), draft)?;
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

    pub(crate) fn append_input(
        &mut self,
        character: CharacterId,
        request: RequestId,
        id: EventId,
        text: Arc<str>,
        intent: TurnIntent,
    ) -> Result<(), String> {
        let kind = match intent {
            TurnIntent::Reply => EventKind::PlayerMessage,
            TurnIntent::React(_) | TurnIntent::Initiate => EventKind::ProactiveOpportunity,
        };
        let mut draft = self.make_draft(id, kind, character, text, Some(request));
        match intent {
            TurnIntent::Reply => draft.actor = Some("player".to_string()),
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
        self.commit(format!("input:{}", request.0), draft)?;
        Ok(())
    }

    pub(crate) fn append_reply(
        &mut self,
        character: CharacterId,
        request: RequestId,
        trigger: EventId,
        context: ContextAnchor,
        turn: TimelineTurn,
        text: Arc<str>,
    ) -> Result<EventId, String> {
        self.assert_turn(turn).map_err(|error| error.to_string())?;
        if context.branch_id != self.branch.0 || context.through > self.anchor().through {
            return Err("context does not belong to the active timeline".into());
        }
        let trigger_fact = self
            .store
            .event(self.branch, trigger)
            .map_err(|error| error.to_string())?;
        if !self
            .store
            .knows(self.branch, character, trigger, None)
            .map_err(|error| error.to_string())?
            || trigger_fact.draft.kind == EventKind::CharacterMessage
        {
            return Err("reply target is not known to this character".into());
        }
        if self
            .store
            .world(self.branch)
            .map_err(|error| error.to_string())?
            .iter()
            .any(|fact| {
                fact.draft.kind == EventKind::CharacterMessage
                    && fact.draft.payload.get("request_id") == Some(&json!(request.0.to_string()))
            })
        {
            return Err("request result already accepted".into());
        }
        let id = EventId::new();
        let mut draft = self.make_draft(
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
        self.commit(format!("reply:{}", request.0), draft)?;
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

    fn make_draft(
        &self,
        id: EventId,
        kind: EventKind,
        character: CharacterId,
        text: Arc<str>,
        request: Option<RequestId>,
    ) -> EventDraft {
        let mut draft = EventDraft::new(kind);
        draft.id = id;
        draft.actor = Some(character.to_string());
        draft.observers.insert(character);
        draft.payload.insert("content".into(), json!(text.as_ref()));
        if let Some(request) = request {
            draft
                .payload
                .insert("request_id".into(), json!(request.0.to_string()));
        }
        draft
    }

    fn commit(&mut self, operation_key: String, draft: EventDraft) -> Result<(), String> {
        let head = self.anchor().through;
        self.store
            .append(self.branch, head, &operation_key, json!({}), vec![draft])
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn legacy_event(fact: &TimelineFact, observer: CharacterId) -> Option<TimelineEvent> {
    let draft = &fact.draft;
    let text: Arc<str> = draft.payload.get("content").and_then(Value::as_str)?.into();
    let content = match draft.kind {
        EventKind::PlayerMessage => EventContent::PlayerMessage { text },
        EventKind::CharacterObservation => EventContent::Observation { text },
        EventKind::ProactiveOpportunity => {
            match draft.payload.get("legacy_intent").and_then(Value::as_str)? {
                "initiate" => EventContent::Initiation { text },
                "react" => EventContent::Reaction {
                    text,
                    target: EventId(Uuid::parse_str(draft.payload.get("target")?.as_str()?).ok()?),
                },
                _ => return None,
            }
        }
        EventKind::CharacterMessage => EventContent::CharacterReply {
            text,
            reply_to: EventId(Uuid::parse_str(draft.payload.get("reply_to")?.as_str()?).ok()?),
            context: serde_json::from_value(draft.payload.get("context")?.clone()).ok()?,
        },
        _ => return None,
    };
    Some(TimelineEvent {
        id: draft.id,
        sequence: fact.sequence,
        observer,
        request_id: draft
            .payload
            .get("request_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .map(RequestId),
        content,
    })
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
            .append_input(
                CharacterId::Mita,
                request,
                trigger,
                Arc::from("hello"),
                TurnIntent::Reply,
            )
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

    #[test]
    fn legacy_api_is_backed_by_the_only_canonical_event_store() {
        let mut timeline = SessionTimeline::new();
        let request = RequestId::new();
        let event = EventId::new();
        timeline
            .append_input(
                CharacterId::Mita,
                request,
                event,
                Arc::from("hello"),
                TurnIntent::Reply,
            )
            .unwrap();
        assert_eq!(timeline.store.world(timeline.branch).unwrap().len(), 1);
        assert_eq!(timeline.events().len(), 1);
        assert_eq!(timeline.history(CharacterId::Mita).events.len(), 1);
        assert!(timeline.history(CharacterId::KindMita).events.is_empty());
    }
}
