use std::{collections::HashMap, sync::Arc};

use tokio::sync::watch;

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    identity::{CharacterId, RequestId},
    interaction::TurnIntent,
};

pub use crate::identity::EventId;
#[cfg(test)]
mod context_read_tests;
pub mod conversation;
pub mod domain;
pub mod file_attachments;
#[cfg(test)]
mod historical_input_fork_tests;
pub mod ingress;
#[cfg(test)]
mod knowledge_revision_tests;
mod legacy;
#[cfg(test)]
mod memory_maintenance_tests;
mod memory_operations;
mod portable_operations;
#[cfg(test)]
mod regenerate_dispatch_tests;
#[cfg(test)]
mod regenerate_enumeration_tests;
#[cfg(test)]
mod regenerate_owner_tests;
mod regenerate_receipt;
mod variable_operations;
pub(crate) use regenerate_receipt::SameBranchRegenerateReceipt;
use regenerate_receipt::SameBranchRegenerateRecoverySnapshot;
#[doc(hidden)]
pub use regenerate_receipt::{
    RegenerateAdmissionReceipt, RegenerateConfigurationReference, RegenerateRecoveryOutcome,
    RegenerateRecoverySnapshot,
};

// Administrative purge is not reachable from generation or runtime until an authorized UI exists.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod memory_purge;
#[cfg(test)]
mod memory_purge_tests;
pub mod message_actions;
pub mod selection;
pub mod storage;
pub mod view;

use conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker};
#[cfg(all(test, feature = "sqlite-system"))]
use domain::{CharacterBehavior, MemoryChange};
use legacy::{legacy_event, make_draft};
use message_actions::MessageAction;
use selection::{SessionWorkspace, TimelineAdmission, TimelineSelection, TimelineTurn};
use view::MitaView;
#[cfg(test)]
use view::TimelineMessageQueries;

#[cfg(feature = "sqlite-system")]
use selection::TimelineSelectionRepository;

use domain::{
    AppendGuard, AppliedEffect, BranchId, CausalLink, CausalRelation, CharacterMemories,
    CharacterWorkingState, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore,
};

/// Opaque, process-local read-only cursor. Its private fields bind the ordered
/// range to one session, character and persisted fact/selection snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct RegenerateAdmissionCursor {
    session: SessionId,
    character: CharacterId,
    after: BranchId,
    revision: Option<u64>,
    selection: TimelineSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct RegenerateAdmissionPage {
    pub entries: Vec<RegenerateRecoverySnapshot>,
    pub next: Option<RegenerateAdmissionCursor>,
}

/// Canonical proof supplied by the read-only Regenerate preview. The owner must
/// revalidate every field against its selected Timeline before forking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegenerateSource {
    pub character: CharacterId,
    pub accepted_message: EventId,
    pub accepted_request: RequestId,
    pub original_input: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegenerateKnowledgePlan {
    pub source: RegenerateSource,
    pub selection: TimelineSelection,
    pub head: u64,
    /// Only authored messages of the selected character, never another Mita.
    pub hide_messages: Vec<EventId>,
    /// Facts this character currently knows that are selected for revocation.
    pub forget_events: Vec<EventId>,
    /// Effects requiring separate reconciliation: knowledge mutation does not undo them.
    pub related_effects: Vec<EventId>,
}

#[derive(Debug, Clone)]
pub(crate) struct AcceptedReplyEvents {
    pub message: EventId,
    pub game: Vec<EventId>,
}

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
pub(crate) struct GameCommit {
    pub state: crate::games::GameSlot,
    pub expected_head: u64,
}

pub(crate) struct GeneratedReply {
    pub request: RequestId,
    pub intent: TurnIntent,
    pub turn: TimelineTurn,
    pub expected_revision: u64,
    pub content: serde_json::Value,
    pub audience: ConversationAudience,
    pub effects: Vec<AppliedEffect>,
}

pub(crate) struct TimelineContextSnapshot {
    pub(crate) state: crate::character::CharacterState,
    pub(crate) variables: std::collections::BTreeMap<String, serde_json::Value>,
    pub(crate) working_state: CharacterWorkingState,
    pub(crate) memories: CharacterMemories,
    pub(crate) known: Vec<domain::KnownEvent>,
    pub(crate) game: crate::games::GameSlot,
    pub(crate) game_token: Option<EventId>,
    pub(crate) history: HistoryView,
    pub(crate) view: MitaView,
}

pub enum TimelineConfig {
    Transient,
    Persistent {
        database: std::path::PathBuf,
    },
    #[cfg(feature = "sqlite-system")]
    Sqlite {
        database: std::path::PathBuf,
        session: SessionId,
        root: BranchId,
    },
}

/// Provenance of the exact texts that are about to leave Core for a provider.
/// The persistent Timeline authorizes these against its live canonical projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KnowledgeDispatchSource {
    Memory(domain::MemoryId, String),
    History(EventId, String),
}

/// RAII token proving a pinned Timeline view cannot be invalidated by any
/// SQLite writer until the outgoing provider invocation finishes or is cancelled.
#[must_use]
pub(crate) struct KnowledgeDispatchLease {
    #[cfg(feature = "sqlite-system")]
    _sqlite: Option<storage::SqliteKnowledgeDispatchLease>,
}

impl KnowledgeDispatchLease {
    #[cfg(feature = "sqlite-system")]
    pub(crate) fn from_sqlite(lease: storage::SqliteKnowledgeDispatchLease) -> Self {
        Self {
            _sqlite: Some(lease),
        }
    }
    #[cfg(test)]
    pub(crate) fn transient_for_test() -> Self {
        Self {
            #[cfg(feature = "sqlite-system")]
            _sqlite: None,
        }
    }
}

pub(crate) struct SessionTimeline {
    branch: BranchId,
    store: Box<dyn TimelineRepository + Send>,
    workspace: SessionWorkspace,
    selection_updates: watch::Sender<TimelineSelection>,
    context_updates: HashMap<CharacterId, watch::Sender<u64>>,
    memory_updates: HashMap<CharacterId, watch::Sender<u64>>,
    #[cfg(feature = "sqlite-system")]
    selection_repository: Option<storage::SqliteTimelineRepository>,
}

impl SessionTimeline {
    #[cfg(feature = "sqlite-system")]
    pub(crate) fn dispatch_repository(&self) -> Option<storage::SqliteTimelineRepository> {
        self.selection_repository.clone()
    }
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
            memory_updates: Self::memory_channels(),
            workspace,
            #[cfg(feature = "sqlite-system")]
            selection_repository: None,
        }
    }

    pub(crate) fn from_config(config: TimelineConfig) -> Result<Self, domain::TimelineError> {
        match config {
            TimelineConfig::Transient => Ok(Self::new()),
            TimelineConfig::Persistent { database } => {
                #[cfg(feature = "sqlite-system")]
                {
                    let mut repository = storage::SqliteTimelineRepository::open(&database)?;
                    let (session, root) = repository.open_or_create_session()?;
                    Self::from_config(TimelineConfig::Sqlite {
                        database,
                        session,
                        root,
                    })
                }
                #[cfg(not(feature = "sqlite-system"))]
                {
                    let _ = database;
                    Err(domain::TimelineError::Storage(
                        "persistent Timeline requires the sqlite-system feature".into(),
                    ))
                }
            }
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
                    memory_updates: Self::memory_channels(),
                    selection_repository: Some(selection_repository),
                })
            }
        }
    }

    pub(crate) fn capture_turn(
        &self,
        character: CharacterId,
        trigger: Option<EventId>,
        context: ContextAnchor,
    ) -> Result<TimelineTurn, domain::TimelineError> {
        if context.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "generation context belongs to another timeline branch",
            ));
        }
        self.workspace
            .capture_at(self.store.as_ref(), character, trigger, context.through)
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

    /// Acquire one provider-dispatch permit. The SQLite reservation remains
    /// alive outside the SessionTimeline mutex for the duration of the await.
    pub(crate) fn admit_knowledge_dispatch(
        &self,
        turn: TimelineTurn,
        sources: &[KnowledgeDispatchSource],
    ) -> Result<KnowledgeDispatchLease, domain::TimelineError> {
        self.assert_knowledge_turn(turn, false)?;
        #[cfg(feature = "sqlite-system")]
        if let Some(repository) = &self.selection_repository {
            return Ok(KnowledgeDispatchLease {
                _sqlite: Some(repository.admit_knowledge_dispatch(turn, sources)?),
            });
        }
        let _ = sources;
        Ok(KnowledgeDispatchLease {
            #[cfg(feature = "sqlite-system")]
            _sqlite: None,
        })
    }

    pub(crate) fn assert_knowledge_turn(
        &self,
        turn: TimelineTurn,
        latest: bool,
    ) -> Result<(), domain::TimelineError> {
        self.assert_turn(turn)?;
        if latest && self.store.branch(turn.selection.branch_id)?.head_sequence != turn.context_head
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

    fn memory_channels() -> HashMap<CharacterId, watch::Sender<u64>> {
        CharacterId::ALL
            .into_iter()
            .map(|character| (character, watch::channel(0).0))
            .collect()
    }

    /// In-process invalidation hint only. The Timeline remains the sole source of
    /// memory data, and receivers must read a new character snapshot after waking.
    pub(crate) fn observe_memory_changes(&self, character: CharacterId) -> watch::Receiver<u64> {
        self.memory_updates[&character].subscribe()
    }

    pub(crate) fn notify_memory_changed(&self, character: CharacterId) {
        self.memory_updates[&character].send_modify(|revision| {
            *revision = revision.wrapping_add(1);
        });
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

    /// Privileged, revisioned changes to a character's knowledge of specific
    /// existing events. This is not message routing or an implicit story transfer.
    /// History Reset remains a separate, broader CharacterContextReset event.
    pub(crate) fn change_character_knowledge(
        &mut self,
        expected: TimelineSelection,
        character: CharacterId,
        operation: EventId,
        reveal: bool,
        targets: Vec<EventId>,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_selection(expected)?;
        let mut draft = EventDraft::new(if reveal {
            EventKind::CharacterKnowledgeRevealed
        } else {
            EventKind::CharacterKnowledgeForgotten
        });
        draft.id = operation;
        draft.observers.insert(character);
        draft.payload.insert(
            "event_ids".into(),
            serde_json::to_value(
                targets
                    .iter()
                    .map(|id| id.0.to_string())
                    .collect::<Vec<_>>(),
            )
            .map_err(|_| domain::TimelineError::Invalid("invalid knowledge targets"))?,
        );
        self.store.append_guarded(
            self.branch,
            self.anchor().through,
            &format!("knowledge:{}", operation.0),
            json!({"character":character.to_string(),"reveal":reveal}),
            vec![draft],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        let epoch = self.store.context_epoch(self.branch, character)?;
        self.context_updates[&character].send_replace(epoch);
        Ok(operation)
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
        for character in CharacterId::ALL {
            self.notify_memory_changed(character);
        }
        Ok(new_selection)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn fork_and_select_checkpoint(
        &mut self,
        checkpoint: EventId,
        child: BranchId,
        expected: TimelineSelection,
        expected_parent_head: u64,
    ) -> Result<TimelineSelection, domain::TimelineError> {
        self.fork_and_select_anchor(
            checkpoint,
            None,
            child,
            expected,
            expected_parent_head,
            None,
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// Owner-only historical input fork; this does not admit or enqueue a
    /// Regenerate request and must never be exposed as that operation.
    pub(crate) fn fork_and_select_player_input(
        &mut self,
        input: EventId,
        character: CharacterId,
        child: BranchId,
        expected: TimelineSelection,
        expected_parent_head: u64,
    ) -> Result<TimelineSelection, domain::TimelineError> {
        self.fork_and_select_anchor(
            input,
            Some(character),
            child,
            expected,
            expected_parent_head,
            None,
        )
    }

    /// Owner-local receipt validation and the selected historical fork form one
    /// guarded operation. No queue reservation, request admission or accepted
    /// Regenerate is implied; callers must not expose this as a chat action.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn fork_and_select_regenerate_source(
        &mut self,
        source: RegenerateSource,
        child: BranchId,
        expected: TimelineSelection,
        expected_parent_head: u64,
    ) -> Result<TimelineSelection, domain::TimelineError> {
        self.validate_regenerate_source(source, expected, expected_parent_head)?;
        self.fork_and_select_player_input(
            source.original_input,
            source.character,
            child,
            expected,
            expected_parent_head,
        )
    }

    /// Captures the child knowledge epoch before committing the historical
    /// fork. After selection changes no fallible repository read remains.
    /// This is an owner primitive, not a durable request-acceptance receipt.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn fork_and_admit_regenerate_source(
        &mut self,
        source: RegenerateSource,
        child: BranchId,
        expected: TimelineSelection,
        expected_parent_head: u64,
        receipt: &RegenerateAdmissionReceipt,
    ) -> Result<TimelineAdmission, domain::TimelineError> {
        self.validate_regenerate_source(source, expected, expected_parent_head)?;
        if receipt.parent_branch != expected.branch_id
            || receipt.parent_head != expected_parent_head
            || receipt.parent_generation != expected.generation
            || receipt.child_branch != child
            || receipt.accepted_request != source.accepted_request
            || receipt.accepted_message != source.accepted_message
            || receipt.original_input != source.original_input
            || receipt.character != source.character
        {
            return Err(domain::TimelineError::Invalid(
                "regenerate admission receipt mismatched",
            ));
        }
        let input = self.store.event(self.branch, source.original_input)?;
        let context_epoch =
            self.store
                .context_epoch_at(self.branch, source.character, Some(input.sequence))?;
        let selection = self.fork_and_select_anchor(
            source.original_input,
            Some(source.character),
            child,
            expected,
            expected_parent_head,
            Some(receipt),
        )?;
        Ok(TimelineAdmission {
            selection,
            context_epoch,
        })
    }

    pub(crate) fn regenerate_admission_receipt(
        &self,
        branch: BranchId,
    ) -> Result<Option<RegenerateAdmissionReceipt>, domain::TimelineError> {
        let info = self.store.branch(branch)?;
        if info.parent.is_none() {
            return Ok(None);
        }
        let marker = self
            .store
            .read(
                branch,
                info.fork_sequence,
                Some(info.fork_sequence + 1),
                1,
                None,
            )?
            .into_iter()
            .next()
            .filter(|fact| fact.branch_id == branch && fact.draft.kind == EventKind::BranchCreated)
            .ok_or(domain::TimelineError::NotFound)?;
        RegenerateAdmissionReceipt::from_marker(&marker.draft.payload)
    }

    /// Read-only recovery diagnostic. Consume the same bounded source pages as
    /// the recovery pager rather than scanning the full event or branch history.
    /// The returned Vec is inherently unbounded; callers needing bounded memory
    /// must use `enumerate_regenerate_admissions_page` directly.
    /// A concurrent writer or selection A→B→A invalidates the scan.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn enumerate_regenerate_admissions(
        &self,
        character: CharacterId,
    ) -> Result<Vec<RegenerateRecoverySnapshot>, domain::TimelineError> {
        let mut snapshots = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.enumerate_regenerate_admissions_page(character, 128, cursor)?;
            snapshots.extend(page.entries);
            if let Some(next) = page.next {
                cursor = Some(next);
            } else {
                return Ok(snapshots);
            }
        }
    }

    /// Bounded, read-only keyset enumeration. The next cursor is valid only
    /// while both fact revision and persisted active-selection generation match.
    /// A page cannot authorize replay of an unknown provider invocation.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn enumerate_regenerate_admissions_page(
        &self,
        character: CharacterId,
        limit: usize,
        cursor: Option<RegenerateAdmissionCursor>,
    ) -> Result<RegenerateAdmissionPage, domain::TimelineError> {
        if !(1..=128).contains(&limit) {
            return Err(domain::TimelineError::Invalid(
                "invalid regenerate page size",
            ));
        }
        let session = self.store.branch(self.branch)?.session_id;
        let revision = self.store.enumeration_revision()?;
        #[cfg(feature = "sqlite-system")]
        let selection = if let Some(repository) = &self.selection_repository {
            repository
                .selection()?
                .ok_or(domain::TimelineError::NotFound)?
        } else {
            self.selection()
        };
        #[cfg(not(feature = "sqlite-system"))]
        let selection = self.selection();
        if let Some(cursor) = cursor
            && (cursor.session != session || cursor.character != character)
        {
            return Err(domain::TimelineError::Conflict);
        }
        if let Some(cursor) = cursor
            && (cursor.revision != revision || cursor.selection != selection)
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let after = cursor.map(|cursor| cursor.after);
        let candidates =
            self.store
                .regenerate_admission_branches_page(session, character, after, limit + 1)?;
        if candidates.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            || candidates
                .first()
                .is_some_and(|first| after.is_some_and(|after| first.0 <= after.0))
            || candidates.len() > limit + 1
        {
            return Err(domain::TimelineError::Conflict);
        }
        let has_more = candidates.len() > limit;
        let mut entries = Vec::with_capacity(candidates.len().min(limit));
        for child in candidates.into_iter().take(limit) {
            let info = self.store.branch(child)?;
            let receipt =
                self.regenerate_admission_receipt(child)?
                    .ok_or(domain::TimelineError::Invalid(
                        "missing regenerate admission marker",
                    ))?;
            if info.session_id != session
                || info.parent != Some(receipt.parent_branch)
                || receipt.character != character
                || receipt.child_branch != child
            {
                return Err(domain::TimelineError::Conflict);
            }
            if self.store.regenerate_admission_request_duplicate(
                session,
                child,
                receipt.request.0,
            )? {
                return Err(domain::TimelineError::Conflict);
            }
            receipt.validate(
                self.store.as_ref(),
                receipt.parent_branch,
                child,
                receipt.original_input,
                character,
            )?;
            entries.push(self.reconcile_regenerate(child, receipt.request)?);
        }
        if self.store.enumeration_revision()? != revision {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        #[cfg(feature = "sqlite-system")]
        if let Some(repository) = &self.selection_repository {
            if repository.selection()? != Some(selection) {
                return Err(domain::TimelineError::ConcurrentWrite);
            }
        } else if self.selection() != selection {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        #[cfg(not(feature = "sqlite-system"))]
        if self.selection() != selection {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let next = if has_more {
            entries.last().map(|entry| RegenerateAdmissionCursor {
                session,
                character,
                after: entry.admission.child_branch,
                revision,
                selection,
            })
        } else {
            None
        };
        Ok(RegenerateAdmissionPage { entries, next })
    }

    /// Inspect one admitted historical fork after an uncertain shutdown.
    /// This is a strictly read-only operation: an absent accepted reply means
    /// dispatch is unknown, NOT cancelled, failed, or safe to repeat.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reconcile_regenerate(
        &self,
        child: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoverySnapshot, domain::TimelineError> {
        let info = self.store.branch(child)?;
        if info.parent.is_none() {
            return Err(domain::TimelineError::Invalid(
                "regenerate recovery requires a child branch",
            ));
        }
        let admission =
            self.regenerate_admission_receipt(child)?
                .ok_or(domain::TimelineError::Invalid(
                    "branch has no durable regenerate admission",
                ))?;
        if admission.request != request {
            return Err(domain::TimelineError::Conflict);
        }
        let facts = self.store.regenerate_request_facts(child, request.0)?;
        let abandoned = domain::abandoned_regenerate(&facts, request.0)?;
        let may_have_dispatched = domain::dispatch_may_have_started(&facts, request.0)?;
        let mut accepted = None;
        for fact in &facts {
            if domain::accepted_reply_request(fact, admission.character) == Some(request.0)
                && accepted.replace(fact).is_some()
            {
                return Err(domain::TimelineError::Conflict);
            }
        }
        let outcome = match accepted {
            None => match (abandoned, may_have_dispatched) {
                (Some(fact), _) => RegenerateRecoveryOutcome::Abandoned(fact.draft.id),
                (None, Some(fact)) => RegenerateRecoveryOutcome::MayHaveDispatched(fact.draft.id),
                (None, None) => RegenerateRecoveryOutcome::DispatchUnknown,
            },
            Some(accepted) => {
                if abandoned.is_some() {
                    return Err(domain::TimelineError::Conflict);
                }
                if accepted.branch_id != child
                    || accepted.sequence <= info.fork_sequence + 1
                    || accepted.draft.links.len() != 1
                    || accepted.draft.links[0].relation != CausalRelation::ReplyTo
                    || accepted.draft.links[0].event_id != admission.original_input
                    || accepted
                        .draft
                        .payload
                        .get("reply_to")
                        .and_then(|value| value.as_str())
                        != Some(admission.original_input.0.to_string().as_str())
                {
                    return Err(domain::TimelineError::Invalid(
                        "regenerate acceptance has invalid causal source",
                    ));
                }
                RegenerateRecoveryOutcome::AcceptedReply(accepted.draft.id)
            }
        };
        #[cfg(feature = "sqlite-system")]
        let selection = if let Some(repository) = &self.selection_repository {
            repository
                .selection()?
                .ok_or(domain::TimelineError::NotFound)?
        } else {
            self.selection()
        };
        #[cfg(not(feature = "sqlite-system"))]
        let selection = self.selection();
        Ok(RegenerateRecoverySnapshot {
            admission,
            selection,
            outcome,
        })
    }

    /// Durably authorize at most one potential provider invocation. The event
    /// is written BEFORE calling external code. After a crash this is only
    /// "may have dispatched", never confirmation or a replay permit.
    pub(crate) fn permit_regenerate_dispatch(
        &mut self,
        child: BranchId,
        request: RequestId,
        expected: TimelineSelection,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_selection(expected)?;
        if child != self.branch || expected.branch_id != child {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let admission =
            self.regenerate_admission_receipt(child)?
                .ok_or(domain::TimelineError::Invalid(
                    "branch has no durable regenerate admission",
                ))?;
        if admission.request != request {
            return Err(domain::TimelineError::Conflict);
        }
        let world = self.store.world(child)?;
        if domain::dispatch_may_have_started(&world, request.0)?.is_some()
            || domain::abandoned_regenerate(&world, request.0)?.is_some()
            || self
                .store
                .accepted_reply_at(child, admission.character, request.0, None)?
                .is_some()
        {
            return Err(domain::TimelineError::Conflict);
        }
        let mut draft = EventDraft::new(EventKind::RegenerateMayHaveDispatched);
        let id = draft.id;
        draft
            .payload
            .insert("request_id".into(), json!(request.0.to_string()));
        self.store.append_guarded(
            child,
            self.store.branch(child)?.head_sequence,
            &format!("regenerate-dispatch:{}", request.0),
            json!({"request_id": request.0.to_string()}),
            vec![draft],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        Ok(id)
    }

    /// Terminally suppress late acceptance in this Timeline world. This does
    /// not cancel any already-dispatched external provider invocation.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn abandon_regenerate(
        &mut self,
        child: BranchId,
        request: RequestId,
        expected: TimelineSelection,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_selection(expected)?;
        if child != self.branch || expected.branch_id != child {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let admission =
            self.regenerate_admission_receipt(child)?
                .ok_or(domain::TimelineError::Invalid(
                    "branch has no durable regenerate admission",
                ))?;
        if admission.request != request {
            return Err(domain::TimelineError::Conflict);
        }
        let world = self.store.world(child)?;
        if let Some(fact) = domain::abandoned_regenerate(&world, request.0)? {
            return Ok(fact.draft.id);
        }
        let mut draft = EventDraft::new(EventKind::RegenerateAbandoned);
        let id = draft.id;
        draft
            .payload
            .insert("request_id".into(), json!(request.0.to_string()));
        self.store.append_guarded(
            child,
            self.store.branch(child)?.head_sequence,
            &format!("regenerate-abandon:{}", request.0),
            json!({"request_id": request.0.to_string()}),
            vec![draft],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        Ok(id)
    }

    /// A request may be cancelled after its selected child becomes historical.
    /// The admitted RequestId and native branch head CAS, rather than the
    /// current UI selection, fence this terminal append.
    pub(crate) fn abandon_regenerate_request(
        &mut self,
        child: BranchId,
        request: RequestId,
        admitted: TimelineSelection,
    ) -> Result<EventId, domain::TimelineError> {
        if child != admitted.branch_id {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let receipt =
            self.regenerate_admission_receipt(child)?
                .ok_or(domain::TimelineError::Invalid(
                    "branch has no durable regenerate admission",
                ))?;
        if receipt.request != request {
            return Err(domain::TimelineError::Conflict);
        }
        let world = self.store.world(child)?;
        if let Some(fact) = domain::abandoned_regenerate(&world, request.0)? {
            return Ok(fact.draft.id);
        }
        if self
            .store
            .accepted_reply_at(child, receipt.character, request.0, None)?
            .is_some()
        {
            return Err(domain::TimelineError::Conflict);
        }
        let mut draft = EventDraft::new(EventKind::RegenerateAbandoned);
        let id = draft.id;
        draft
            .payload
            .insert("request_id".into(), json!(request.0.to_string()));
        self.store.append(
            child,
            self.store.branch(child)?.head_sequence,
            &format!("regenerate-abandon:{}", request.0),
            json!({"request_id": request.0.to_string()}),
            vec![draft],
        )?;
        Ok(id)
    }

    /// Locate a same-branch admission by canonical RequestId. A legacy child
    /// BranchCreated receipt never substitutes for this explicit marker.
    fn same_branch_regenerate_receipt(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<(SameBranchRegenerateReceipt, u64), domain::TimelineError> {
        let mut found = None;
        for fact in self.store.world(branch)? {
            if fact.branch_id != branch || fact.draft.kind != EventKind::RegenerateAdmitted {
                continue;
            }
            let receipt = SameBranchRegenerateReceipt::from_fact(&fact.draft)?;
            if receipt.request == request {
                if receipt.branch != branch || found.replace((receipt, fact.sequence)).is_some() {
                    return Err(domain::TimelineError::Conflict);
                }
            }
        }
        found.ok_or(domain::TimelineError::NotFound)
    }

    /// Inspect only the durable facts; no registry re-creation or provider replay.
    pub(crate) fn reconcile_same_branch_regenerate(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<SameBranchRegenerateRecoverySnapshot, domain::TimelineError> {
        let (admission, admitted_at) = self.same_branch_regenerate_receipt(branch, request)?;
        let facts = self.store.regenerate_request_facts(branch, request.0)?;
        let abandoned = domain::abandoned_regenerate(&facts, request.0)?;
        let dispatched = domain::dispatch_may_have_started(&facts, request.0)?;
        let mut accepted = None;
        for fact in &facts {
            if domain::accepted_reply_request(fact, admission.character) == Some(request.0) {
                if fact.branch_id != branch
                    || fact.sequence <= admitted_at
                    || fact
                        .draft
                        .links
                        .iter()
                        .filter(|link| link.relation == CausalRelation::ReplyTo)
                        .map(|link| link.event_id)
                        .collect::<Vec<_>>()
                        != vec![admission.original_input]
                    || fact.draft.payload.get("reply_to").and_then(|v| v.as_str())
                        != Some(admission.original_input.0.to_string().as_str())
                    || accepted.replace(fact).is_some()
                {
                    return Err(domain::TimelineError::Conflict);
                }
            }
        }
        if accepted.is_some() && abandoned.is_some() {
            return Err(domain::TimelineError::Conflict);
        }
        let outcome = match (accepted, abandoned, dispatched) {
            (Some(fact), _, _) => RegenerateRecoveryOutcome::AcceptedReply(fact.draft.id),
            (None, Some(fact), _) => RegenerateRecoveryOutcome::Abandoned(fact.draft.id),
            (None, None, Some(fact)) => RegenerateRecoveryOutcome::MayHaveDispatched(fact.draft.id),
            (None, None, None) => RegenerateRecoveryOutcome::DispatchUnknown,
        };
        Ok(SameBranchRegenerateRecoverySnapshot { admission, outcome })
    }

    /// Durable one-shot pre-provider fence for a same-branch admission.
    /// A crash after this commit is an uncertain dispatch, not a replay permit.
    pub(crate) fn permit_same_branch_regenerate_dispatch(
        &mut self,
        branch: BranchId,
        request: RequestId,
        expected: TimelineSelection,
    ) -> Result<EventId, domain::TimelineError> {
        self.assert_selection(expected)?;
        if self.branch != branch || expected.branch_id != branch {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let snapshot = self.reconcile_same_branch_regenerate(branch, request)?;
        if expected.generation != snapshot.admission.selection_generation {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let expected_epoch = if snapshot.admission.forget_operation.is_some() {
            snapshot.admission.source_head + 2
        } else {
            self.store.context_epoch_at(
                branch,
                snapshot.admission.character,
                Some(snapshot.admission.source_head),
            )?
        };
        if self
            .store
            .context_epoch(branch, snapshot.admission.character)?
            != expected_epoch
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        if snapshot.outcome != RegenerateRecoveryOutcome::DispatchUnknown {
            return Err(domain::TimelineError::Conflict);
        }
        let mut draft = EventDraft::new(EventKind::RegenerateMayHaveDispatched);
        let id = draft.id;
        draft
            .payload
            .insert("request_id".into(), json!(request.0.to_string()));
        self.store.append_guarded(
            branch,
            self.store.branch(branch)?.head_sequence,
            &format!("regenerate-dispatch:{}", request.0),
            json!({"request_id": request.0.to_string()}),
            vec![draft],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        Ok(id)
    }

    /// The terminal append is bound to the original request's branch, even
    /// when the user has changed the active Timeline selection since admission.
    pub(crate) fn abandon_same_branch_regenerate_request(
        &mut self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<EventId, domain::TimelineError> {
        let snapshot = self.reconcile_same_branch_regenerate(branch, request)?;
        if let RegenerateRecoveryOutcome::Abandoned(id) = snapshot.outcome {
            return Ok(id);
        }
        if matches!(
            snapshot.outcome,
            RegenerateRecoveryOutcome::AcceptedReply(_)
        ) {
            return Err(domain::TimelineError::Conflict);
        }
        let mut draft = EventDraft::new(EventKind::RegenerateAbandoned);
        let id = draft.id;
        draft
            .payload
            .insert("request_id".into(), json!(request.0.to_string()));
        self.store.append(
            branch,
            self.store.branch(branch)?.head_sequence,
            &format!("regenerate-abandon:{}", request.0),
            json!({"request_id": request.0.to_string()}),
            vec![draft],
        )?;
        Ok(id)
    }

    /// Read-only, single-world proposal for the non-fork Regenerate design.
    /// This cannot be committed separately from the new request admission.
    pub(crate) fn plan_knowledge_regenerate(
        &self,
        source: RegenerateSource,
        expected: TimelineSelection,
        expected_head: u64,
    ) -> Result<RegenerateKnowledgePlan, domain::TimelineError> {
        self.assert_selection(expected)?;
        if self.branch != expected.branch_id
            || self.store.branch(self.branch)?.head_sequence != expected_head
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let facts = self.store.world(self.branch)?;
        let accepted = self
            .store
            .accepted_reply_at(
                self.branch,
                source.character,
                source.accepted_request.0,
                Some(expected_head),
            )?
            .filter(|fact| fact.draft.id == source.accepted_message)
            .ok_or(domain::TimelineError::Invalid(
                "unknown accepted regenerate source",
            ))?;
        let input = self.store.event(self.branch, source.original_input)?;
        let valid_reply = accepted
            .draft
            .links
            .iter()
            .filter(|link| link.relation == CausalRelation::ReplyTo)
            .collect::<Vec<_>>();
        if input.draft.kind != EventKind::PlayerMessage
            || input.draft.actor.as_deref() != Some("player")
            || !input.draft.observers.contains(&source.character)
            || input.sequence >= accepted.sequence
            || valid_reply.len() != 1
            || valid_reply[0].event_id != source.original_input
            || accepted
                .draft
                .payload
                .get("reply_to")
                .and_then(serde_json::Value::as_str)
                != Some(source.original_input.0.to_string().as_str())
        {
            return Err(domain::TimelineError::Invalid(
                "regenerate source has invalid causality",
            ));
        }
        let view = MitaView::from_facts(self.branch, expected_head, source.character, &facts);
        if !view
            .messages()
            .iter()
            .any(|message| message.event_id == source.accepted_message)
            || !view
                .messages()
                .iter()
                .any(|message| message.event_id == source.original_input)
        {
            return Err(domain::TimelineError::Invalid(
                "regenerate source is not visible",
            ));
        }
        let hide_messages = view
            .messages()
            .iter()
            .filter(|message| {
                message.sequence >= accepted.sequence
                    && message.kind == EventKind::CharacterMessage
                    && message.speaker.as_deref() == Some(source.character.as_str())
            })
            .map(|message| message.event_id)
            .collect::<Vec<_>>();
        let mut selected = hide_messages
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut related_effects = Vec::new();
        for fact in facts
            .iter()
            .filter(|fact| fact.sequence > accepted.sequence)
        {
            if fact.draft.actor.as_deref() != Some(source.character.as_str())
                || !matches!(
                    fact.draft.kind,
                    EventKind::CharacterStateChanged
                        | EventKind::CharacterWorkingStateReplaced
                        | EventKind::CharacterVariablesChanged
                        | EventKind::CharacterMemoryChanged
                )
            {
                continue;
            }
            if fact.draft.links.iter().any(|link| {
                link.relation == CausalRelation::CausedBy && selected.contains(&link.event_id)
            }) {
                related_effects.push(fact.draft.id);
                selected.insert(fact.draft.id);
            }
        }
        let forget_events = facts
            .iter()
            .filter(|fact| selected.contains(&fact.draft.id))
            .map(|fact| {
                self.store
                    .knows(
                        self.branch,
                        source.character,
                        fact.draft.id,
                        Some(expected_head),
                    )
                    .map(|known| known.then_some(fact.draft.id))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        Ok(RegenerateKnowledgePlan {
            source,
            selection: expected,
            head: expected_head,
            hide_messages,
            forget_events,
            related_effects,
        })
    }

    /// The only same-branch durable admission preparation. Caller MUST first
    /// reserve its queue permit and freeze settings/preset/prompt; the request
    /// lifecycle is not wired to this receipt until dispatch and cancellation
    /// are migrated from fork markers.
    #[allow(dead_code)]
    pub(crate) fn admit_same_branch_regenerate(
        &mut self,
        proposed: &RegenerateKnowledgePlan,
        receipt: &SameBranchRegenerateReceipt,
        marker_id: EventId,
    ) -> Result<TimelineAdmission, domain::TimelineError> {
        let source = proposed.source;
        if receipt.request == source.accepted_request
            || receipt.request.0.is_nil()
            || receipt.accepted_request != source.accepted_request
            || receipt.accepted_message != source.accepted_message
            || receipt.original_input != source.original_input
            || receipt.character != source.character
            || receipt.branch != proposed.selection.branch_id
            || receipt.source_head != proposed.head
            || receipt.selection_generation != proposed.selection.generation
            || receipt.hidden_messages != proposed.hide_messages
            || receipt.forgotten_events != proposed.forget_events
            || receipt.hidden_messages.is_empty()
            || receipt.configuration.provider_model.trim().is_empty()
            || receipt
                .configuration
                .prompt_set
                .as_deref()
                .is_some_and(|s| s.trim().is_empty())
            || marker_id == receipt.hide_operation
            || receipt
                .forget_operation
                .is_some_and(|id| id == marker_id || id == receipt.hide_operation)
            || receipt.forget_operation.is_some() != !receipt.forgotten_events.is_empty()
        {
            return Err(domain::TimelineError::Invalid(
                "invalid same-branch regenerate admission",
            ));
        }
        let plan = self.plan_knowledge_regenerate(source, proposed.selection, proposed.head)?;
        if &plan != proposed || !plan.related_effects.is_empty() {
            return Err(domain::TimelineError::Invalid(
                "stale regenerate knowledge plan",
            ));
        }
        self.assert_regenerate_effects_safe(&plan)?;
        let mut drafts = vec![
            MessageAction::Hide {
                targets: plan.hide_messages.clone(),
            }
            .into_draft(receipt.hide_operation, source.character),
        ];
        if let Some(id) = receipt.forget_operation {
            let mut forget = EventDraft::new(EventKind::CharacterKnowledgeForgotten);
            forget.id = id;
            forget.observers.insert(source.character);
            forget.payload.insert(
                "event_ids".into(),
                json!(
                    plan.forget_events
                        .iter()
                        .map(|id| id.0.to_string())
                        .collect::<Vec<_>>()
                ),
            );
            drafts.push(forget);
        }
        drafts.push(receipt.draft(marker_id)?);
        self.store.append_guarded(
            self.branch,
            plan.head,
            &format!("regenerate-admission:{}", receipt.request.0),
            json!({"request": receipt.request.0.to_string(), "character": source.character.to_string()}),
            drafts,
            AppendGuard {
                selection_generation: plan.selection.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        let epoch = self.store.context_epoch(self.branch, source.character)?;
        self.context_updates[&source.character].send_replace(epoch);
        self.notify_memory_changed(source.character);
        self.admission(source.character, plan.selection)
    }

    fn assert_regenerate_effects_safe(
        &self,
        plan: &RegenerateKnowledgePlan,
    ) -> Result<(), domain::TimelineError> {
        let accepted_sequence = self
            .store
            .event(self.branch, plan.source.accepted_message)?
            .sequence;
        if self.store.world(self.branch)?.iter().any(|fact| {
            fact.sequence >= accepted_sequence
                && fact.draft.actor.as_deref() == Some(plan.source.character.as_str())
                && matches!(
                    fact.draft.kind,
                    EventKind::CharacterStateChanged
                        | EventKind::CharacterWorkingStateReplaced
                        | EventKind::CharacterVariablesChanged
                        | EventKind::CharacterMemoryChanged
                        | EventKind::GameAction
                        | EventKind::GameStateCaptured
                )
        }) {
            return Err(domain::TimelineError::Invalid(
                "regenerate needs explicit reconciliation of stateful effects",
            ));
        }
        Ok(())
    }

    /// Atomically apply only the character-scoped knowledge/presentation changes
    /// of a prevalidated Regenerate plan. This is NOT request admission: no caller
    /// may expose it until a durable same-branch receipt and queue permit join
    /// this exact transaction. In particular it never selects or forks a branch.
    #[allow(dead_code)]
    pub(crate) fn commit_regenerate_knowledge_controls(
        &mut self,
        proposed: &RegenerateKnowledgePlan,
        hide_operation: EventId,
        forget_operation: EventId,
    ) -> Result<ContextAnchor, domain::TimelineError> {
        if hide_operation == forget_operation || proposed.hide_messages.is_empty() {
            return Err(domain::TimelineError::Invalid(
                "regenerate controls need distinct operations and visible replies",
            ));
        }
        let checked =
            self.plan_knowledge_regenerate(proposed.source, proposed.selection, proposed.head)?;
        if checked != *proposed || !checked.related_effects.is_empty() {
            return Err(domain::TimelineError::Invalid(
                "regenerate plan is stale or needs effect reconciliation",
            ));
        }
        self.assert_regenerate_effects_safe(&checked)?;
        let mut drafts = vec![
            MessageAction::Hide {
                targets: checked.hide_messages.clone(),
            }
            .into_draft(hide_operation, checked.source.character),
        ];
        if !checked.forget_events.is_empty() {
            let mut forget = EventDraft::new(EventKind::CharacterKnowledgeForgotten);
            forget.id = forget_operation;
            forget.observers.insert(checked.source.character);
            forget.payload.insert(
                "event_ids".into(),
                json!(
                    checked
                        .forget_events
                        .iter()
                        .map(|id| id.0.to_string())
                        .collect::<Vec<_>>()
                ),
            );
            drafts.push(forget);
        }
        let committed = self.store.append_guarded(
            self.branch,
            checked.head,
            &format!("regenerate-controls:{hide_operation:?}:{forget_operation:?}"),
            json!({
                "character": checked.source.character.to_string(),
                "accepted_message": checked.source.accepted_message.0.to_string(),
                "original_input": checked.source.original_input.0.to_string(),
            }),
            drafts,
            AppendGuard {
                selection_generation: checked.selection.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        let epoch = self
            .store
            .context_epoch(self.branch, checked.source.character)?;
        self.context_updates[&checked.source.character].send_replace(epoch);
        self.notify_memory_changed(checked.source.character);
        Ok(ContextAnchor {
            branch_id: self.branch.0,
            through: committed
                .last()
                .expect("at least one regenerate control")
                .sequence,
        })
    }

    fn validate_regenerate_source(
        &self,
        source: RegenerateSource,
        expected: TimelineSelection,
        expected_parent_head: u64,
    ) -> Result<(), domain::TimelineError> {
        self.assert_selection(expected)?;
        if self.branch != expected.branch_id
            || self.store.branch(self.branch)?.head_sequence != expected_parent_head
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let facts = self.store.world(self.branch)?;
        let visible =
            MitaView::from_facts(self.branch, expected_parent_head, source.character, &facts);
        let messages = visible.messages();
        let latest = messages
            .iter()
            .rev()
            .find(|message| {
                message.role == domain::MessageRole::Assistant
                    && message.speaker.as_deref() == Some(source.character.as_str())
            })
            .ok_or(domain::TimelineError::Invalid("no visible accepted reply"))?;
        if latest.event_id != source.accepted_message
            || latest.request_id != Some(source.accepted_request)
        {
            return Err(domain::TimelineError::Invalid(
                "regenerate source is not the latest accepted reply",
            ));
        }
        let receipt = self
            .store
            .accepted_reply_at(
                self.branch,
                source.character,
                source.accepted_request.0,
                Some(expected_parent_head),
            )?
            .filter(|fact| fact.draft.id == source.accepted_message)
            .ok_or(domain::TimelineError::Invalid(
                "regenerate request has no canonical acceptance receipt",
            ))?;
        let mut replies = receipt
            .draft
            .links
            .iter()
            .filter(|link| link.relation == CausalRelation::ReplyTo);
        if replies.next().map(|link| link.event_id) != Some(source.original_input)
            || replies.next().is_some()
            || receipt
                .draft
                .payload
                .get("reply_to")
                .and_then(serde_json::Value::as_str)
                != Some(source.original_input.0.to_string().as_str())
        {
            return Err(domain::TimelineError::Invalid(
                "regenerate receipt does not refer to original player input",
            ));
        }
        let input = self.store.event(self.branch, source.original_input)?;
        if input.draft.kind != EventKind::PlayerMessage
            || input.draft.actor.as_deref() != Some("player")
            || !input.draft.observers.contains(&source.character)
            || input.sequence >= receipt.sequence
            || !messages.iter().any(|message| {
                message.event_id == source.original_input
                    && message.role == domain::MessageRole::User
                    && message.speaker.as_deref() == Some("player")
                    && message.sequence < receipt.sequence
            })
        {
            return Err(domain::TimelineError::Invalid(
                "original player input is not visible and causally prior",
            ));
        }
        Ok(())
    }

    fn fork_and_select_anchor(
        &mut self,
        anchor: EventId,
        player_recipient: Option<CharacterId>,
        child: BranchId,
        expected: TimelineSelection,
        expected_parent_head: u64,
        admission: Option<&RegenerateAdmissionReceipt>,
    ) -> Result<TimelineSelection, domain::TimelineError> {
        self.assert_selection(expected)?;
        if self.branch != expected.branch_id
            || self.store.branch(self.branch)?.head_sequence != expected_parent_head
        {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        if self.store.branch(child).is_ok() {
            return Err(domain::TimelineError::Conflict);
        }
        let source = self.store.event(self.branch, anchor)?;
        match player_recipient {
            None if source.draft.kind != EventKind::Checkpoint => {
                return Err(domain::TimelineError::Invalid("fork requires a checkpoint"));
            }
            Some(character) => {
                if source.draft.kind != EventKind::PlayerMessage
                    || source.draft.actor.as_deref() != Some("player")
                    || !source.draft.observers.contains(&character)
                {
                    return Err(domain::TimelineError::Invalid(
                        "fork requires an addressed player input",
                    ));
                }
                let world = self.store.world(self.branch)?;
                if !MitaView::from_facts(self.branch, expected_parent_head, character, &world)
                    .messages()
                    .iter()
                    .any(|message| message.event_id == anchor)
                {
                    return Err(domain::TimelineError::Invalid(
                        "player input is not currently visible",
                    ));
                }
            }
            _ => {}
        }
        let next_generation = expected
            .generation
            .checked_add(1)
            .filter(|generation| *generation <= i64::MAX as u64)
            .ok_or(domain::TimelineError::Invalid(
                "selection generation overflow",
            ))?;
        let epochs = CharacterId::ALL
            .into_iter()
            .map(|character| {
                self.store
                    .context_epoch_at(self.branch, character, Some(source.sequence))
                    .map(|epoch| (character, epoch))
            })
            .collect::<Result<Vec<_>, _>>()?;

        #[cfg(feature = "sqlite-system")]
        let selection = if let Some(repository) = &mut self.selection_repository {
            match (player_recipient, admission) {
                (Some(character), Some(receipt)) => repository.fork_and_select_regenerate_input(
                    self.branch,
                    expected_parent_head,
                    anchor,
                    character,
                    child,
                    expected,
                    receipt,
                )?,
                (Some(character), None) => repository.fork_and_select_player_input(
                    self.branch,
                    expected_parent_head,
                    anchor,
                    character,
                    child,
                    expected,
                )?,
                (None, None) => repository.fork_and_select_checkpoint(
                    self.branch,
                    expected_parent_head,
                    anchor,
                    child,
                    expected,
                )?,
                (None, Some(_)) => {
                    return Err(domain::TimelineError::Invalid("invalid regenerate anchor"));
                }
            }
        } else {
            match (player_recipient, admission) {
                (Some(character), Some(receipt)) => {
                    self.store.fork_at_regenerate_input(
                        self.branch,
                        anchor,
                        character,
                        child,
                        receipt,
                    )?;
                }
                (Some(character), None) => {
                    self.store
                        .fork_at_player_input(self.branch, anchor, character, child)?;
                }
                (None, None) => {
                    self.store.fork(self.branch, anchor, child)?;
                }
                (None, Some(_)) => {
                    return Err(domain::TimelineError::Invalid("invalid regenerate anchor"));
                }
            };
            TimelineSelection {
                branch_id: child,
                generation: next_generation,
            }
        };
        #[cfg(not(feature = "sqlite-system"))]
        let selection = {
            match (player_recipient, admission) {
                (Some(character), Some(receipt)) => {
                    self.store.fork_at_regenerate_input(
                        self.branch,
                        anchor,
                        character,
                        child,
                        receipt,
                    )?;
                }
                (Some(character), None) => {
                    self.store
                        .fork_at_player_input(self.branch, anchor, character, child)?;
                }
                (None, None) => {
                    self.store.fork(self.branch, anchor, child)?;
                }
                (None, Some(_)) => {
                    return Err(domain::TimelineError::Invalid("invalid regenerate anchor"));
                }
            };
            TimelineSelection {
                branch_id: child,
                generation: next_generation,
            }
        };
        self.workspace.install_selection(selection);
        self.branch = child;
        for (character, epoch) in epochs {
            self.context_updates[&character].send_replace(epoch);
        }
        self.selection_updates.send_replace(selection);
        for character in CharacterId::ALL {
            self.notify_memory_changed(character);
        }
        Ok(selection)
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

    /// Apply expired, unprotected memories in one guarded Timeline transaction.
    /// The scheduler/caller supplies time and selection; no read-side mutation occurs.
    pub(crate) fn expire_character_memories(
        &mut self,
        expected: TimelineSelection,
        character: CharacterId,
        as_of_unix_ms: u64,
    ) -> Result<usize, domain::TimelineError> {
        self.assert_selection(expected)?;
        let head = self.anchor().through;
        let facts = self.store.world(self.branch)?;
        let memories = crate::character::projection::character_memories_at(&facts, character)?;
        let changes = memories.resolve_expirations(as_of_unix_ms);
        if changes.is_empty() {
            return Ok(0);
        }
        let count = changes.len();
        let mut drafts = Vec::with_capacity(count);
        for change in changes {
            let mut draft = EventDraft::new(EventKind::CharacterMemoryChanged);
            draft.actor = Some(character.to_string());
            draft.observers.insert(character);
            draft.payload.insert(
                "change".into(),
                serde_json::to_value(change)
                    .map_err(|_| domain::TimelineError::Invalid("invalid expiry change"))?,
            );
            drafts.push(draft);
        }
        self.store.append_guarded(
            self.branch,
            head,
            &format!("memory-expiry:{character}:{as_of_unix_ms}:{head}"),
            json!({"character": character.to_string(), "as_of_unix_ms": as_of_unix_ms}),
            drafts,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        self.notify_memory_changed(character);
        Ok(count)
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
        let facts = self.read_prefix(at).map_err(|error| error.to_string())?;
        let known = domain::character_view(&facts, character);
        let history = HistoryView {
            at,
            events: known
                .iter()
                .filter_map(|item| legacy_event(&item.fact, character))
                .collect(),
        };
        let view = MitaView::from_facts(BranchId(at.branch_id), at.through, character, &facts);
        let state = crate::character::projection::character_state(&facts, character)
            .map_err(|error| error.to_string())?;
        let working_state =
            crate::character::projection::character_working_state_at(&facts, character)
                .map_err(|error| error.to_string())?;
        let memories = crate::character::projection::character_memories_at(&facts, character)
            .map_err(|error| error.to_string())?;
        let variables = crate::character::projection::character_variables_at(&facts, character)
            .map_err(|error| error.to_string())?;
        let (game, game_token) = Self::game_from_facts(character, at, &facts)?;
        Ok(TimelineContextSnapshot {
            variables,
            game,
            game_token,
            history,
            view,
            state,
            working_state,
            memories,
            known,
        })
    }

    pub(crate) fn game_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<(crate::games::GameSlot, Option<EventId>), String> {
        let facts = self.read_prefix(at).map_err(|error| error.to_string())?;
        Self::game_from_facts(character, at, &facts)
    }

    fn game_from_facts(
        character: CharacterId,
        at: ContextAnchor,
        facts: &[domain::TimelineFact],
    ) -> Result<(crate::games::GameSlot, Option<EventId>), String> {
        let mut game = crate::games::GameSlot::default();
        let mut token = None;
        for fact in facts.iter().rev() {
            if fact.draft.kind == EventKind::GameStateCaptured
                && fact.draft.actor.as_deref() == Some(character.to_string().as_str())
            {
                game = serde_json::from_value(fact.draft.payload["game_state"].clone())
                    .map_err(|error| format!("invalid canonical game: {error}"))?;
                game.validate().map_err(str::to_owned)?;
                token = Some(fact.draft.id);
                break;
            }
        }
        game.bind_branch(BranchId(at.branch_id));
        Ok((game, token))
    }

    fn game_state_draft(
        character: CharacterId,
        game: crate::games::GameSlot,
    ) -> Result<EventDraft, domain::TimelineError> {
        game.validate().map_err(domain::TimelineError::Invalid)?;
        let mut draft = EventDraft::new(EventKind::GameStateCaptured);
        draft.actor = Some(character.to_string());
        draft.observers.insert(character);
        draft.payload.insert(
            "game_state".into(),
            serde_json::to_value(game)
                .map_err(|_| domain::TimelineError::Invalid("invalid game state"))?,
        );
        Ok(draft)
    }

    pub(crate) fn character_debug_variables(
        &self,
        character: CharacterId,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, domain::TimelineError> {
        let facts = self.read_prefix(self.anchor())?;
        let state = crate::character::projection::character_state(&facts, character)?;
        let mut variables = std::collections::BTreeMap::from([
            ("attitude".into(), json!(state.attitude)),
            ("boredom".into(), json!(state.boredom)),
            ("stress".into(), json!(state.stress)),
            ("ACCEPTED_REPLIES".into(), json!(state.accepted_replies)),
        ]);
        variables.extend(crate::character::projection::character_variables_at(
            &facts, character,
        )?);
        Ok(variables)
    }

    pub(crate) fn character_state_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<crate::character::CharacterState, domain::TimelineError> {
        crate::character::projection::character_state(&self.read_prefix(at)?, character)
    }

    fn read_prefix(
        &self,
        at: ContextAnchor,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .read(branch, 0, Some(at.through), i64::MAX as usize, None)
    }

    pub(crate) fn reminder_facts_at(
        &self,
        at: ContextAnchor,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        self.read_prefix(at)
    }

    /// Facts of the selected branch that can carry reminder state. Unselected
    /// branches stay untouched, so their pending deliveries are never lost.
    pub(crate) fn reminder_facts(
        &self,
        selection: crate::timeline::selection::TimelineSelection,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        self.assert_selection(selection)?;
        self.read_prefix(self.anchor())
    }

    /// Head and facts of one branch in the active session, read as a single
    /// consistent prefix so a delivery guard and its append can use one CAS.
    pub(crate) fn reminder_snapshot_on(
        &self,
        branch: BranchId,
    ) -> Result<(u64, Vec<domain::TimelineFact>), domain::TimelineError> {
        if self.store.branch(branch)?.session_id != self.store.branch(self.branch)?.session_id {
            return Err(domain::TimelineError::Invalid(
                "branch belongs to another session",
            ));
        }
        let head = self.store.branch(branch)?.head_sequence;
        let facts = self.store.read(branch, 0, None, i64::MAX as usize, None)?;
        Ok((head, facts))
    }

    /// Append a delivery transition to the branch that owns the fired
    /// occurrence, even when another branch is selected. Only the three
    /// delivery transition kinds are accepted; this is not a general write
    /// bypass. The caller supplies the head it validated against, so a stale
    /// settlement is rejected instead of being re-read and applied blindly.
    pub(crate) fn append_delivery_transition(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        draft: domain::EventDraft,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        if !matches!(
            draft.kind,
            domain::EventKind::ReminderDeliverySettled
                | domain::EventKind::ReminderDeliveryFailed
                | domain::EventKind::ReminderDeliveryRetried
        ) {
            return Err(domain::TimelineError::Invalid(
                "only delivery transitions may target another branch",
            ));
        }
        if self.store.branch(branch)?.session_id != self.store.branch(self.branch)?.session_id {
            return Err(domain::TimelineError::Invalid(
                "branch belongs to another session",
            ));
        }
        self.store
            .append(branch, expected_head, key, json!({}), vec![draft])
    }

    pub(crate) fn append_reminder_facts(
        &mut self,
        expected: TimelineSelection,
        key: &str,
        drafts: Vec<domain::EventDraft>,
    ) -> Result<Vec<domain::TimelineFact>, domain::TimelineError> {
        if self.selection() != expected {
            return Err(domain::TimelineError::ConcurrentWrite);
        }
        let anchor = self.anchor();
        self.store.append_guarded(
            self.branch,
            anchor.through,
            key,
            json!({}),
            drafts,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )
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
    #[cfg(test)]
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

    #[cfg(all(test, feature = "sqlite-system"))]
    pub(crate) fn character_behavior_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterBehavior, domain::TimelineError> {
        crate::character::projection::character_behavior_at(&self.read_prefix(at)?, character)
    }

    #[cfg(all(test, feature = "sqlite-system"))]
    pub(crate) fn character_working_state_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterWorkingState, domain::TimelineError> {
        crate::character::projection::character_working_state_at(&self.read_prefix(at)?, character)
    }

    #[cfg(all(test, feature = "sqlite-system"))]
    pub(crate) fn character_memories_at(
        &self,
        character: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterMemories, domain::TimelineError> {
        crate::character::projection::character_memories_at(&self.read_prefix(at)?, character)
    }

    /// Find only a committed character reply in an immutable branch prefix.
    /// Unanswered/cancelled/unfinished requests are intentionally indistinguishable.
    pub(crate) fn accepted_reply_at(
        &self,
        character: CharacterId,
        request: RequestId,
        at: ContextAnchor,
    ) -> Result<Option<domain::TimelineFact>, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .accepted_reply_at(branch, character, request.0, Some(at.through))
    }

    /// Read-only evidence of ingress, proactive dispatch or committed reply.
    /// It does not recover the process-local request state or replay work.
    pub(crate) fn request_evidence_at(
        &self,
        character: CharacterId,
        request: RequestId,
        at: ContextAnchor,
    ) -> Result<domain::RequestEvidence, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .request_evidence_at(branch, character, request.0, Some(at.through))
    }

    /// Recover the request associated with an input identity retained before
    /// submitting it, without resurrecting process-local execution.
    pub(crate) fn request_evidence_for_input_at(
        &self,
        character: CharacterId,
        input: EventId,
        at: ContextAnchor,
    ) -> Result<Option<(RequestId, domain::RequestEvidence)>, domain::TimelineError> {
        let branch = self.inspection_branch(at)?;
        self.store
            .request_evidence_for_input_at(branch, character, input, Some(at.through))
            .map(|value| value.map(|(request, evidence)| (RequestId(request), evidence)))
    }

    pub(crate) fn reaction_cause_at(
        &self,
        character: CharacterId,
        intent: TurnIntent,
        at: ContextAnchor,
    ) -> Result<Option<domain::TimelineFact>, domain::TimelineError> {
        let target = match intent {
            TurnIntent::React(target) | TurnIntent::Reply(target) => target,
            TurnIntent::Initiate => return Ok(None),
        };
        if at.branch_id != self.branch.0 {
            return Err(domain::TimelineError::Invalid(
                "reaction context branch mismatch",
            ));
        }
        let fact = self.store.event(self.branch, target)?;
        if matches!(intent, TurnIntent::Reply(_)) && !fact.draft.kind.is_message() {
            return Err(domain::TimelineError::Invalid(
                "reply target must be a conversation message",
            ));
        }
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
    pub(crate) fn knows_event_at(
        &self,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, domain::TimelineError> {
        self.store.knows(self.branch, character, event, through)
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

    #[cfg(test)]
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

    /// Commit a shared conversation message once for all its witnesses.
    /// The message is independent of LLM admission and never starts generation.
    pub(crate) fn publish_conversation_message(
        &mut self,
        expected: TimelineSelection,
        message: conversation::ConversationMessage,
    ) -> Result<domain::TimelineFact, domain::TimelineError> {
        self.assert_selection(expected)?;
        let head = self.anchor().through;
        conversation::append_conversation_message(
            self.store.as_mut(),
            self.branch,
            head,
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
            message,
        )
    }

    /// Edit, hide, or restore the current character's visible conversation view.
    /// Changes are immutable facts and never modify another character's history.
    pub(crate) fn change_message(
        &mut self,
        expected: TimelineSelection,
        character: CharacterId,
        operation: EventId,
        action: MessageAction,
    ) -> Result<domain::TimelineFact, domain::TimelineError> {
        self.assert_selection(expected)?;
        let draft = action.into_draft(operation, character);
        let facts = self.store.append_guarded(
            self.branch,
            self.anchor().through,
            &format!("message-action:{}", operation.0),
            json!({"character": character.to_string()}),
            vec![draft],
            AppendGuard {
                selection_generation: expected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        facts
            .into_iter()
            .next()
            .ok_or(domain::TimelineError::Invalid(
                "message action must commit one fact",
            ))
    }

    pub(crate) fn publish_player_message(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
    ) -> Result<(), String> {
        self.write_player_message(
            character,
            id,
            text,
            None,
            ConversationAudience::player_to(character),
        )
    }

    fn write_player_message(
        &mut self,
        character: CharacterId,
        id: EventId,
        text: Arc<str>,
        request: Option<RequestId>,
        audience: ConversationAudience,
    ) -> Result<(), String> {
        let mut message = ConversationMessage::new(
            id.0.to_string(),
            ConversationSpeaker::Player,
            json!(text.as_ref()),
            [character],
        );
        message.id = id;
        message.request_id = request;
        message.audience = audience;
        let draft = message.into_draft().map_err(|error| error.to_string())?;
        let operation_key = request.map_or_else(
            || format!("message:{}", id.0),
            |request| format!("input:{}", request.0),
        );
        self.commit(operation_key, draft, None, None)
            .map_err(|error| error.to_string())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn append_reply(
        &mut self,
        reply: GeneratedReply,
    ) -> Result<EventId, domain::TimelineError> {
        self.append_reply_tracked(reply, None)
            .map(|accepted| accepted.message)
    }

    pub(crate) fn append_reply_tracked(
        &mut self,
        reply: GeneratedReply,
        game_state: Option<GameCommit>,
    ) -> Result<AcceptedReplyEvents, domain::TimelineError> {
        let GeneratedReply {
            request,
            intent,
            turn,
            expected_revision,
            content,
            audience,
            effects,
        } = reply;
        let character = turn.character;
        self.assert_turn(turn)?;
        match self.same_branch_regenerate_receipt(turn.selection.branch_id, request) {
            Ok((receipt, _)) => {
                if receipt.character != character
                    || !matches!(intent, TurnIntent::Reply(id) if id == receipt.original_input)
                    || receipt.selection_generation != turn.selection.generation
                {
                    return Err(domain::TimelineError::ConcurrentWrite);
                }
            }
            Err(domain::TimelineError::NotFound) => {}
            Err(error) => return Err(error),
        }
        // The durable fork receipt is authoritative for the new RequestId.
        // Validate before constructing any reply/effect drafts and before the
        // guarded append, so an unrelated callback cannot use that identity.
        if self.store.branch(self.branch)?.parent.is_some()
            && let Some(admitted) = self.regenerate_admission_receipt(self.branch)?
            && admitted.request == request
        {
            if intent != TurnIntent::Reply(admitted.original_input)
                || character != admitted.character
                || turn.selection.branch_id != admitted.child_branch
            {
                return Err(domain::TimelineError::Invalid(
                    "regenerate acceptance violates durable admission",
                ));
            }
            if self
                .store
                .accepted_reply_at(self.branch, character, request.0, None)?
                .is_some()
            {
                return Err(domain::TimelineError::Conflict);
            }
        }
        let context = ContextAnchor {
            branch_id: turn.selection.branch_id.0,
            through: turn.context_head,
        };
        if context.branch_id != self.branch.0 || context.through > self.anchor().through {
            return Err(domain::TimelineError::Invalid(
                "context does not belong to the active timeline",
            ));
        }
        let cause = match intent {
            TurnIntent::Reply(id) | TurnIntent::React(id) => {
                if turn.trigger != Some(id) {
                    return Err(domain::TimelineError::Invalid(
                        "generation cause does not match the captured turn",
                    ));
                }
                let fact = self.store.event(self.branch, id)?;
                if matches!(intent, TurnIntent::Reply(_)) && !fact.draft.kind.is_message() {
                    return Err(domain::TimelineError::Invalid(
                        "reply target must be a message",
                    ));
                }
                if fact.sequence > context.through
                    || !self.store.knows(self.branch, character, id, None)?
                {
                    return Err(domain::TimelineError::ConcurrentWrite);
                }
                Some(id)
            }
            TurnIntent::Initiate => {
                if turn.trigger.is_some() {
                    return Err(domain::TimelineError::Invalid(
                        "initiate must not have an event trigger",
                    ));
                }
                None
            }
        };
        let id = EventId::new();
        let mut message = ConversationMessage::new(
            id.0.to_string(),
            ConversationSpeaker::Character(character),
            content,
            [],
        );
        message.id = id;
        message.audience = audience;
        message.request_id = Some(request);
        message.context = Some(context);
        if let Some(cause) = cause {
            let relation = match intent {
                TurnIntent::Reply(_) => {
                    message.reply_to = Some(cause);
                    CausalRelation::ReplyTo
                }
                TurnIntent::React(_) => CausalRelation::ReactTo,
                TurnIntent::Initiate => unreachable!(),
            };
            message.links.push(CausalLink {
                relation,
                event_id: cause,
            });
        }
        let draft = message.into_draft()?;
        let mut drafts = vec![draft];
        let mut changes_memory = false;
        let occurred_at = crate::reminders::UtcInstant::from_unix_micros(
            i64::try_from(drafts[0].occurred_at_unix_ms)
                .ok()
                .and_then(|time| time.checked_mul(1000))
                .ok_or(domain::TimelineError::Invalid(
                    "reply timestamp out of reminder range",
                ))?,
        );
        for effect in effects {
            match effect {
                AppliedEffect::CharacterStateChanged(behavior) => {
                    if ![behavior.attitude, behavior.boredom, behavior.stress]
                        .iter()
                        .all(|value| value.is_finite() && (0.0..=100.0).contains(value))
                    {
                        return Err(domain::TimelineError::Invalid("invalid character state"));
                    }
                    let mut change = EventDraft::new(EventKind::CharacterStateChanged);
                    change.actor = Some(character.to_string());
                    change.observers.insert(character);
                    change.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    change.payload.insert(
                        "state".into(),
                        serde_json::to_value(behavior).map_err(|_| {
                            domain::TimelineError::Invalid("invalid character state")
                        })?,
                    );
                    drafts.push(change);
                }
                AppliedEffect::CharacterMemoryChanged(memory_change) => {
                    changes_memory = true;
                    let mut memory = EventDraft::new(EventKind::CharacterMemoryChanged);
                    memory.actor = Some(character.to_string());
                    memory.observers.insert(character);
                    memory.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    memory.payload.insert(
                        "change".into(),
                        serde_json::to_value(memory_change)
                            .map_err(|_| domain::TimelineError::Invalid("invalid memory change"))?,
                    );
                    drafts.push(memory);
                }
                AppliedEffect::CharacterVariablesChanged(vars) => {
                    let mut change = EventDraft::new(EventKind::CharacterVariablesChanged);
                    change.actor = Some(character.to_string());
                    change.observers.insert(character);
                    change.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    change.payload.insert(
                        "variables".into(),
                        serde_json::to_value(vars).map_err(|_| {
                            domain::TimelineError::Invalid("invalid character variables")
                        })?,
                    );
                    drafts.push(change);
                }
                AppliedEffect::CharacterWorkingStateReplaced(state) => {
                    if !state.is_valid() {
                        return Err(domain::TimelineError::Invalid("invalid working state"));
                    }
                    let mut handoff = EventDraft::new(EventKind::CharacterWorkingStateReplaced);
                    handoff.actor = Some(character.to_string());
                    handoff.observers.insert(character);
                    handoff.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    handoff.payload.insert(
                        "working_state".into(),
                        serde_json::to_value(state)
                            .map_err(|_| domain::TimelineError::Invalid("invalid working state"))?,
                    );
                    drafts.push(handoff);
                }
                AppliedEffect::GameAdvanced(fact) => {
                    let kind = match &fact {
                        crate::games::GameFact::Started { .. } => EventKind::GameStarted,
                        crate::games::GameFact::PhaseChanged { .. }
                        | crate::games::GameFact::Shot { .. } => EventKind::GameAction,
                        crate::games::GameFact::Ended { .. } => EventKind::GameEnded,
                    };
                    let mut game = EventDraft::new(kind);
                    game.actor = Some(character.to_string());
                    game.observers.insert(character);
                    game.links.push(CausalLink {
                        relation: CausalRelation::CausedBy,
                        event_id: id,
                    });
                    for (key, value) in fact.payload() {
                        game.payload.insert(key, value);
                    }
                    drafts.push(game);
                }
                AppliedEffect::ReminderChanged(change) => {
                    let reminder = crate::reminders::change_draft(&change, Some(id), occurred_at)
                        .map_err(|_| {
                        domain::TimelineError::Invalid("invalid reminder change")
                    })?;
                    drafts.push(reminder);
                }
                AppliedEffect::ReminderToolResult(content) => {
                    let result = crate::reminders::tool_result_draft(
                        character,
                        &content,
                        Some(id),
                        occurred_at,
                    )
                    .map_err(|_| domain::TimelineError::Invalid("invalid reminder tool result"))?;
                    drafts.push(result);
                }
            }
        }
        let expected_head = game_state
            .as_ref()
            .map_or_else(|| self.anchor().through, |commit| commit.expected_head);
        if let Some(GameCommit {
            state: mut game, ..
        }) = game_state
        {
            for draft in &drafts {
                if matches!(
                    draft.kind,
                    EventKind::GameStarted | EventKind::GameAction | EventKind::GameEnded
                ) {
                    game.record_event(draft.id);
                }
            }
            drafts.push(Self::game_state_draft(character, game)?);
        }
        let game_events = drafts
            .iter()
            .filter(|draft| {
                matches!(
                    draft.kind,
                    EventKind::GameStarted | EventKind::GameAction | EventKind::GameEnded
                )
            })
            .map(|draft| draft.id)
            .collect();
        let selected = self.selection();
        self.store.append_guarded(
            self.branch,
            expected_head,
            &format!("reply:{}", request.0),
            json!({}),
            drafts,
            AppendGuard {
                selection_generation: selected.generation,
                context: Some((turn.character, turn.context_epoch, turn.trigger)),
                accepted_reply_revision: Some((character, expected_revision)),
            },
        )?;
        if changes_memory {
            self.notify_memory_changed(character);
        }
        Ok(AcceptedReplyEvents {
            message: id,
            game: game_events,
        })
    }

    /// Commit an owner command and its private state in one guarded batch.
    pub(crate) fn append_game_facts(
        &mut self,
        character: CharacterId,
        facts: &[crate::games::GameFact],
        state: &mut crate::games::GameSlot,
        expected_head: u64,
    ) -> Result<Vec<EventId>, domain::TimelineError> {
        let selected = self.selection();
        let mut drafts = Vec::with_capacity(facts.len());
        for fact in facts {
            let kind = match fact {
                crate::games::GameFact::Started { .. } => EventKind::GameStarted,
                crate::games::GameFact::PhaseChanged { .. }
                | crate::games::GameFact::Shot { .. } => EventKind::GameAction,
                crate::games::GameFact::Ended { .. } => EventKind::GameEnded,
            };
            let mut draft = EventDraft::new(kind);
            draft.actor = Some(character.to_string());
            draft.observers.insert(character);
            for (key, value) in fact.payload() {
                draft.payload.insert(key, value);
            }
            drafts.push(draft);
        }
        let ids: Vec<_> = drafts.iter().map(|draft| draft.id).collect();
        for id in &ids {
            state.record_event(*id);
        }
        drafts.push(Self::game_state_draft(character, state.clone())?);
        let key = format!("game:{character}:{}", uuid::Uuid::new_v4());
        self.store.append_guarded(
            self.branch,
            expected_head,
            &key,
            json!({}),
            drafts,
            AppendGuard {
                selection_generation: selected.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        Ok(ids)
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
mod message_action_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[test]
    fn transient_checkpoint_fork_preserves_original_and_excludes_downstream_reset() {
        let mut timeline = SessionTimeline::new();
        let parent = timeline.branch;
        let expected = timeline.selection();
        let original = EventId::new();
        timeline
            .publish_player_message(
                CharacterId::Crazy,
                original,
                Arc::from("with original media id"),
            )
            .unwrap();
        let mut marker = EventDraft::new(EventKind::Checkpoint);
        marker.payload.insert("snapshot".into(), json!({}));
        let checkpoint_id = marker.id;
        timeline.append_test_fact("mark", marker).unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        timeline.append_test_fact("reset", reset).unwrap();
        assert_eq!(
            timeline.store.context_epoch(parent, CharacterId::Crazy),
            Ok(3)
        );
        let child = BranchId::new();
        let selection = timeline
            .fork_and_select_checkpoint(checkpoint_id, child, expected, 3)
            .unwrap();
        assert_eq!(selection.branch_id, child);
        assert_eq!(selection.generation, expected.generation + 1);
        assert_eq!(timeline.anchor().through, 3);
        assert_eq!(timeline.store.world(parent).unwrap().len(), 3);
        assert_eq!(timeline.store.world(child).unwrap().len(), 3);
        assert_eq!(
            timeline.store.event(child, original).unwrap().draft.id,
            original
        );
        assert_eq!(
            timeline
                .store
                .knows(child, CharacterId::Crazy, original, None),
            Ok(true)
        );
        assert_eq!(
            timeline
                .store
                .knows(child, CharacterId::Kind, original, None),
            Ok(false)
        );
        assert_eq!(
            timeline.store.context_epoch(child, CharacterId::Crazy),
            Ok(0)
        );
        let another = BranchId::new();
        assert_eq!(
            timeline.fork_and_select_checkpoint(checkpoint_id, another, expected, 3),
            Err(domain::TimelineError::ConcurrentWrite)
        );
        assert_eq!(
            timeline.store.branch(another),
            Err(domain::TimelineError::NotFound)
        );
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn persistent_checkpoint_fork_is_restored_as_selected_after_reopen() {
        let path = std::env::temp_dir().join(format!("neuromita-b58-fork-{}.db", Uuid::new_v4()));
        let session = SessionId::new();
        let parent = BranchId::new();
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root: parent,
        })
        .unwrap();
        let expected = timeline.selection();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, original, Arc::from("original"))
            .unwrap();
        let mut marker = EventDraft::new(EventKind::Checkpoint);
        marker.payload.insert("snapshot".into(), json!({}));
        let checkpoint_id = marker.id;
        timeline.append_test_fact("mark", marker).unwrap();
        let child = BranchId::new();
        let selection = timeline
            .fork_and_select_checkpoint(checkpoint_id, child, expected, 2)
            .unwrap();
        drop(timeline);
        let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root: parent,
        })
        .unwrap();
        assert_eq!(reopened.selection(), selection);
        assert_eq!(reopened.anchor().through, 3);
        assert_eq!(
            reopened.store.event(child, original).unwrap().draft.id,
            original
        );
        drop(reopened);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[test]
    fn legacy_accept_does_not_answer_a_forgotten_trigger() {
        let mut timeline = SessionTimeline::new();
        let request = RequestId::new();
        let trigger = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, trigger, Arc::from("hello"))
            .unwrap();
        let turn = timeline
            .capture_turn(CharacterId::Crazy, Some(trigger), timeline.anchor())
            .unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        timeline
            .store
            .append(timeline.branch, 1, "reset", json!({}), vec![reset])
            .unwrap();
        assert!(
            timeline
                .append_reply(GeneratedReply {
                    request,
                    intent: TurnIntent::Reply(trigger),
                    turn,
                    expected_revision: 0,
                    content: json!("answer"),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: Vec::new(),
                })
                .is_err()
        );
        assert_eq!(timeline.anchor().through, 2);
    }

    fn reset_and_retell(
        store: &mut dyn TimelineRepository,
        branch: BranchId,
        original: EventId,
        key: &str,
    ) {
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        let mut retelling = EventDraft::new(EventKind::PlayerMessage);
        retelling.actor = Some("player".into());
        retelling.observers.insert(CharacterId::Crazy);
        retelling.links.push(CausalLink {
            relation: CausalRelation::CausedBy,
            event_id: original,
        });
        let retelling_id = retelling.id;
        let head = store.branch(branch).unwrap().head_sequence;
        store
            .append(branch, head, key, json!({}), vec![reset, retelling])
            .unwrap();
        assert!(
            !store
                .knows(branch, CharacterId::Crazy, original, None)
                .unwrap()
        );
        assert!(
            store
                .knows(branch, CharacterId::Crazy, retelling_id, None)
                .unwrap()
        );
    }

    fn assert_stale_pin(
        timeline: &SessionTimeline,
        pin: TimelineTurn,
        original: EventId,
        at: ContextAnchor,
    ) {
        assert!(
            timeline
                .capture_turn(CharacterId::Crazy, Some(original), at)
                .is_err()
        );
        assert_eq!(pin.context_head, at.through);
        assert_eq!(pin.context_epoch, 0);
        assert!(
            timeline
                .store
                .context_epoch(timeline.branch, CharacterId::Crazy)
                .unwrap()
                > 0
        );
        assert_eq!(
            timeline.assert_turn(pin),
            Err(domain::TimelineError::ConcurrentWrite)
        );
    }

    #[test]
    fn captured_epoch_cannot_be_replaced_by_later_reset_and_retelling() {
        let mut timeline = SessionTimeline::new();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, original, Arc::from("original"))
            .unwrap();
        let captured = timeline
            .capture_context(CharacterId::Crazy, ContextPosition::Latest)
            .unwrap();
        let at = captured.history.at;
        assert_eq!(captured.view.messages()[0].content, "original");

        let pin = timeline
            .capture_turn(CharacterId::Crazy, Some(original), at)
            .unwrap();
        let branch = timeline.branch;
        reset_and_retell(
            timeline.store.as_mut(),
            branch,
            original,
            "reset-and-retell",
        );
        assert_stale_pin(&timeline, pin, original, at);
        assert_eq!(captured.view.messages()[0].content, "original");

        let before = timeline.anchor().through;
        assert_eq!(
            timeline.append_reply(GeneratedReply {
                request: RequestId::new(),
                intent: TurnIntent::React(original),
                turn: pin,
                expected_revision: 0,
                content: json!("reaction"),
                audience: ConversationAudience::private_to(CharacterId::Crazy),
                effects: Vec::new(),
            }),
            Err(domain::TimelineError::ConcurrentWrite)
        );
        assert_eq!(timeline.anchor().through, before);
    }

    #[test]
    fn newer_unrelated_facts_do_not_invalidate_a_captured_epoch() {
        let mut timeline = SessionTimeline::new();
        let original = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, original, Arc::from("original"))
            .unwrap();
        let at = timeline
            .capture_context(CharacterId::Crazy, ContextPosition::Latest)
            .unwrap()
            .history
            .at;
        timeline
            .publish_player_message(CharacterId::Crazy, EventId::new(), Arc::from("later"))
            .unwrap();
        let pin = timeline
            .capture_turn(CharacterId::Crazy, Some(original), at)
            .unwrap();
        assert_eq!(pin.context_head, at.through);
        assert_eq!(pin.context_epoch, 0);
        timeline.assert_turn(pin).unwrap();
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn external_sqlite_reset_and_retelling_cannot_repin_old_prompt() {
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
            .publish_player_message(CharacterId::Crazy, original, Arc::from("original"))
            .unwrap();
        let at = timeline
            .capture_context(CharacterId::Crazy, ContextPosition::Latest)
            .unwrap()
            .history
            .at;
        let pin = timeline
            .capture_turn(CharacterId::Crazy, Some(original), at)
            .unwrap();
        let mut external = storage::SqliteTimelineRepository::open(&path).unwrap();
        reset_and_retell(&mut external, branch, original, "remote-reset-and-retell");
        assert_stale_pin(&timeline, pin, original, at);

        let before = timeline.anchor().through;
        assert_eq!(
            timeline.append_reply(GeneratedReply {
                request: RequestId::new(),
                intent: TurnIntent::React(original),
                turn: pin,
                expected_revision: 0,
                content: json!("reaction"),
                audience: ConversationAudience::private_to(CharacterId::Crazy),
                effects: Vec::new(),
            }),
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
                    .publish_player_message(CharacterId::Crazy, id, Arc::from("independent input"))
                    .unwrap();
            }
            let events = timeline.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].id, id);
            assert_eq!(events[0].request_id, None);
            let captured = timeline
                .capture_context(CharacterId::Crazy, ContextPosition::Latest)
                .unwrap();
            assert_eq!(captured.view.messages()[0].content, "independent input");
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn memory_replays_historical_branch_prefix_and_sqlite_reopen() {
        use domain::{MemoryId, MemoryPriority, MemoryRecord};
        let path = std::env::temp_dir().join(format!("neuromita-memory-{}.db", Uuid::new_v4()));
        let session = SessionId::new();
        let root = BranchId::new();
        let memory_id = MemoryId::new();
        let initial;
        let updated;
        {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let original = MemoryRecord {
                entities: Vec::new(),
                id: memory_id,
                priority: MemoryPriority::High,
                content: "first fact".into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            };
            let turn = timeline
                .capture_turn(CharacterId::Crazy, None, timeline.anchor())
                .unwrap();
            timeline
                .append_reply(GeneratedReply {
                    request: RequestId::new(),
                    intent: TurnIntent::Initiate,
                    turn,
                    expected_revision: 0,
                    content: json!([{"text":"One"}]),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: vec![AppliedEffect::CharacterMemoryChanged(MemoryChange::Upsert(
                        original,
                    ))],
                })
                .unwrap();
            initial = timeline.anchor();
            let changed = MemoryRecord {
                entities: Vec::new(),
                id: memory_id,
                priority: MemoryPriority::Critical,
                content: "corrected fact".into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                protected: false,
                expires_at_unix_ms: None,
            };
            let turn = timeline
                .capture_turn(CharacterId::Crazy, None, timeline.anchor())
                .unwrap();
            timeline
                .append_reply(GeneratedReply {
                    request: RequestId::new(),
                    intent: TurnIntent::Initiate,
                    turn,
                    expected_revision: 1,
                    content: json!([{"text":"Two"}]),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: vec![AppliedEffect::CharacterMemoryChanged(MemoryChange::Upsert(
                        changed,
                    ))],
                })
                .unwrap();
            updated = timeline.anchor();
            timeline
                .reset_character_context(timeline.selection(), CharacterId::Crazy, EventId::new())
                .unwrap();
        }
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let first = timeline
                .character_memories_at(CharacterId::Crazy, initial)
                .unwrap()
                .list();
            assert_eq!(first.len(), 1);
            assert_eq!(first[0].content, "first fact");
            let later = timeline
                .character_memories_at(CharacterId::Crazy, updated)
                .unwrap()
                .list();
            assert_eq!(later[0].id, memory_id);
            assert_eq!(later[0].content, "corrected fact");
            assert_eq!(
                timeline
                    .character_memories_at(CharacterId::Crazy, timeline.anchor())
                    .unwrap()
                    .list(),
                later
            );
            assert!(
                timeline
                    .character_memories_at(CharacterId::Kind, timeline.anchor())
                    .unwrap()
                    .list()
                    .is_empty()
            );
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn working_state_replays_from_sqlite_and_resets_at_historical_anchor() {
        let path =
            std::env::temp_dir().join(format!("neuromita-working-effect-{}.db", Uuid::new_v4()));
        let session = SessionId::new();
        let root = BranchId::new();
        let original_anchor;
        let reset_anchor;
        let snapshot = CharacterWorkingState {
            focus: "Need to clarify the question".into(),
            open_loops: vec!["Confirm intent".into()],
            ..CharacterWorkingState::default()
        };
        {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let turn = timeline
                .capture_turn(CharacterId::Crazy, None, timeline.anchor())
                .unwrap();
            timeline
                .append_reply(GeneratedReply {
                    request: RequestId::new(),
                    intent: TurnIntent::Initiate,
                    turn,
                    expected_revision: 0,
                    content: json!([{"text":"Hello"}]),
                    audience: ConversationAudience::private_to(CharacterId::Crazy),
                    effects: vec![AppliedEffect::CharacterWorkingStateReplaced(
                        snapshot.clone(),
                    )],
                })
                .unwrap();
            assert_eq!(timeline.anchor().through, 2);
            original_anchor = timeline.anchor();
            timeline
                .reset_character_context(timeline.selection(), CharacterId::Crazy, EventId::new())
                .unwrap();
            reset_anchor = timeline.anchor();
        }
        {
            let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            assert_eq!(
                timeline
                    .character_working_state_at(CharacterId::Crazy, original_anchor)
                    .unwrap(),
                snapshot
            );
            assert!(
                timeline
                    .character_working_state_at(CharacterId::Crazy, reset_anchor)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                timeline
                    .character_working_state_at(CharacterId::Kind, original_anchor)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                timeline
                    .character_behavior_at(CharacterId::Crazy, reset_anchor)
                    .unwrap(),
                CharacterBehavior::default()
            );
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn legacy_api_is_backed_by_the_only_canonical_event_store() {
        let mut timeline = SessionTimeline::new();
        let event = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, event, Arc::from("hello"))
            .unwrap();
        assert_eq!(timeline.store.world(timeline.branch).unwrap().len(), 1);
        assert_eq!(timeline.events().len(), 1);
        assert_eq!(timeline.history(CharacterId::Crazy).events.len(), 1);
        assert!(timeline.history(CharacterId::Kind).events.is_empty());
    }
}
