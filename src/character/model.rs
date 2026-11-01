use std::sync::Arc;

use crate::{
    identity::CharacterId,
    timeline::{ContextAnchor, EventId, HistoryView, selection::TimelineTurn, view::MitaView},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterState {
    pub accepted_replies: u64,
    pub revision: u64,
}

pub(crate) struct Character {
    pub(super) id: CharacterId,
    pub(super) description: Arc<str>,
    pub(super) prompt: Arc<str>,
    pub(super) state: CharacterState,
}

impl Character {
    pub(super) fn create(id: CharacterId) -> Self {
        let (description, prompt) = match id {
            CharacterId::Mita => (
                "Мита — собеседница игрока.",
                "Отвечай от лица Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::KindMita => (
                "Добрая Мита — внимательная и спокойная собеседница игрока.",
                "Отвечай от лица Доброй Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
        };
        Self {
            id,
            description: Arc::from(description),
            prompt: Arc::from(prompt),
            state: CharacterState {
                accepted_replies: 0,
                revision: 0,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct CharacterContext {
    pub character_id: CharacterId,
    pub description: Arc<str>,
    pub prompt: Arc<str>,
    pub state: CharacterState,
    pub history: HistoryView,
    pub view: MitaView,
    pub current_input: Arc<str>,
    pub trigger: EventId,
    pub anchor: ContextAnchor,
    pub(crate) turn: TimelineTurn,
}

#[derive(Debug, Clone)]
pub struct CharacterView {
    pub character_id: CharacterId,
    pub description: Arc<str>,
    pub prompt: Arc<str>,
    pub state: CharacterState,
    pub history: HistoryView,
    pub view: MitaView,
}

#[derive(Debug, Clone, Copy)]
pub enum ResponseEffect {
    RecordAcceptedReply,
}
