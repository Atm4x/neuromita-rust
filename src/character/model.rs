use std::sync::Arc;

use crate::{
    games::GameProjection,
    identity::CharacterId,
    interaction::TurnIntent,
    timeline::{
        ContextAnchor, EventId, HistoryView,
        domain::{CharacterWorkingState, EventKind, MemoryRecord},
        selection::TimelineTurn,
        view::MitaView,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub struct CharacterState {
    pub accepted_replies: u64,
    pub revision: u64,
    pub attitude: f64,
    pub boredom: f64,
    pub stress: f64,
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
                attitude: 60.0,
                boredom: 10.0,
                stress: 5.0,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionCause {
    pub event_id: EventId,
    pub kind: EventKind,
    pub description: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationFocus {
    pub intent: TurnIntent,
    pub reaction: Option<ReactionCause>,
}

#[derive(Debug, Clone)]
pub struct CharacterContext {
    pub character_id: CharacterId,
    pub description: Arc<str>,
    pub prompt: Arc<str>,
    pub state: CharacterState,
    pub working_state: CharacterWorkingState,
    pub memories: Vec<MemoryRecord>,
    pub memory_islands: Vec<MemoryRecord>,
    pub history: HistoryView,
    pub view: MitaView,
    /// Role-limited board projection for this character only.
    pub game: Option<GameProjection>,
    /// Own active reminders of the selected branch; never another character's.
    pub reminders: Vec<crate::reminders::ReminderPrompt>,
    pub current_input: Arc<str>,
    pub focus: GenerationFocus,
    pub anchor: ContextAnchor,
    pub(crate) turn: TimelineTurn,
    pub(crate) game_key: Option<crate::identity::EventId>,
}

#[derive(Debug, Clone)]
pub struct CharacterView {
    pub game: Option<GameProjection>,
    pub character_id: CharacterId,
    pub description: Arc<str>,
    pub prompt: Arc<str>,
    pub state: CharacterState,
    pub working_state: CharacterWorkingState,
    pub memories: Vec<MemoryRecord>,
    pub memory_islands: Vec<MemoryRecord>,
    pub history: HistoryView,
    pub view: MitaView,
}
