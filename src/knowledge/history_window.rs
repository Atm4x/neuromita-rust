//! Read-only, single-Timeline-prefix split between active conversation and older RAG history.
//! Neither side owns messages: membership always comes from the authorized MitaView.

use std::collections::BTreeSet;

use crate::{
    identity::{CharacterId, EventId},
    timeline::{
        domain::{BranchId, MessageRole},
        view::MitaView,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryWindow {
    character: CharacterId,
    branch: BranchId,
    through: u64,
    active: BTreeSet<EventId>,
    archived: BTreeSet<EventId>,
}

impl HistoryWindow {
    /// Retain the most recently learned conversational messages, plus any
    /// explicitly targeted older Reply/React message. System and World entries
    /// remain in the ordinary context rather than entering History retrieval.
    pub fn capture(view: &MitaView, focus: Option<EventId>, active_messages: usize) -> Self {
        let mut messages = view.messages();
        messages.sort_by(|left, right| {
            left.learned_at
                .cmp(&right.learned_at)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        let eligible = messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::User | MessageRole::Assistant))
            .collect::<Vec<_>>();
        let mut active = eligible
            .iter()
            .rev()
            .take(active_messages)
            .map(|message| message.event_id)
            .collect::<BTreeSet<_>>();
        if let Some(focus) = focus {
            if eligible.iter().any(|message| message.event_id == focus) {
                active.insert(focus);
            }
        }
        let archived = eligible
            .into_iter()
            .filter(|message| !active.contains(&message.event_id))
            .map(|message| message.event_id)
            .collect();
        for message in &messages {
            if message.role == MessageRole::System {
                active.insert(message.event_id);
            }
        }
        Self {
            character: view.character(),
            branch: view.branch,
            through: view.through,
            active,
            archived,
        }
    }

    pub fn matches(&self, view: &MitaView) -> bool {
        self.character == view.character()
            && self.branch == view.branch
            && self.through == view.through
    }

    pub fn is_active(&self, id: EventId) -> bool {
        self.active.contains(&id)
    }

    pub fn is_archived(&self, id: EventId) -> bool {
        self.archived.contains(&id)
    }

    pub fn archived_count(&self) -> usize {
        self.archived.len()
    }
}
