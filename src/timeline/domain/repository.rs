use serde_json::Value;

use crate::identity::{CharacterId, EventId};

use super::{
    AppendGuard, BranchId, BranchInfo, EventDraft, KnownEvent, SessionId, TimelineError,
    TimelineFact, TimelineStore,
};

/// Semantic timeline operations; SQLite and future CFMD must obey identical laws.
/// Database revisions must never substitute for explicit branch heads.
pub trait TimelineRepository {
    fn create_session(
        &mut self,
        session: SessionId,
        root: BranchId,
    ) -> Result<BranchInfo, TimelineError>;
    fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError>;
    fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError>;
    fn event(&self, branch: BranchId, id: EventId) -> Result<TimelineFact, TimelineError>;
    /// Stable ascending page of direct facts; observer filters direct witnesses,
    /// not derived knowledge or later disclosures.
    fn read(
        &self,
        branch: BranchId,
        after: u64,
        through: Option<u64>,
        limit: usize,
        observer: Option<CharacterId>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        if limit == 0 {
            return Err(TimelineError::Invalid("read limit must be positive"));
        }
        let head = self.branch(branch)?.head_sequence;
        let ceiling = through.unwrap_or(head);
        if ceiling > head {
            return Err(TimelineError::Invalid("read position exceeds branch head"));
        }
        Ok(self
            .world(branch)?
            .into_iter()
            .filter(|fact| {
                fact.sequence > after
                    && fact.sequence <= ceiling
                    && observer.is_none_or(|id| fact.draft.observers.contains(&id))
            })
            .take(limit)
            .collect())
    }
    fn append(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError>;
    /// Append only when the supplied selection and optional turn context remain valid.
    /// Persistent implementations must validate all preconditions atomically with writes.
    fn append_guarded(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
        guard: AppendGuard,
    ) -> Result<Vec<TimelineFact>, TimelineError>;
    fn context_epoch_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<u64, TimelineError>;
    fn context_epoch(
        &self,
        branch: BranchId,
        character: CharacterId,
    ) -> Result<u64, TimelineError> {
        self.context_epoch_at(branch, character, None)
    }
    fn fork(
        &mut self,
        parent: BranchId,
        checkpoint: EventId,
        child: BranchId,
    ) -> Result<BranchInfo, TimelineError>;
    fn view(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError>;
    fn knows(
        &self,
        branch: BranchId,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError>;
}

impl TimelineRepository for TimelineStore {
    fn create_session(
        &mut self,
        session: SessionId,
        root: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        TimelineStore::create_session(self, session, root)
    }
    fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError> {
        TimelineStore::branch(self, id)
    }
    fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError> {
        TimelineStore::world(self, id)
    }
    fn event(&self, branch: BranchId, id: EventId) -> Result<TimelineFact, TimelineError> {
        TimelineStore::event(self, branch, id)
    }
    fn append(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        TimelineStore::append(self, branch, expected_head, key, command, drafts)
    }
    fn append_guarded(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
        guard: AppendGuard,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        if let Some((character, epoch, trigger)) = guard.context {
            if TimelineStore::context_epoch(self, branch, character)? != epoch {
                return Err(TimelineError::ConcurrentWrite);
            }
            if let Some(event) = trigger
                && !TimelineStore::knows(self, branch, character, event, None)?
            {
                return Err(TimelineError::ConcurrentWrite);
            }
        }
        if let Some((character, expected_revision)) = guard.accepted_reply_revision {
            let facts = TimelineStore::world(self, branch)?;
            if super::accepted_reply_count(&facts, character)? != expected_revision {
                return Err(TimelineError::ConcurrentWrite);
            }
        }
        TimelineStore::append(self, branch, expected_head, key, command, drafts)
    }
    fn context_epoch_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<u64, TimelineError> {
        TimelineStore::context_epoch_at(self, branch, character, through)
    }
    fn fork(
        &mut self,
        parent: BranchId,
        checkpoint: EventId,
        child: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        TimelineStore::fork(self, parent, checkpoint, child)
    }
    fn view(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError> {
        TimelineStore::view(self, branch, character, through)
    }
    fn knows(
        &self,
        branch: BranchId,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError> {
        TimelineStore::knows(self, branch, character, event, through)
    }
}
