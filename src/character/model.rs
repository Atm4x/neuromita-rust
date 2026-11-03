use std::sync::Arc;

use crate::{
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
            CharacterId::Crazy => (
                "Безумная Мита — собеседница игрока.",
                "Отвечай от лица Безумной Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::Kind => (
                "Добрая Мита — внимательная и спокойная собеседница игрока.",
                "Отвечай от лица Доброй Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::Cappie => (
                "Кепочка — собеседница игрока.",
                "Отвечай от лица Кепочки. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::ShortHair => (
                "Коротковолосая Мита — собеседница игрока.",
                "Отвечай от лица Коротковолосой Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::Mila => (
                "Мила — собеседница игрока.",
                "Отвечай от лица Милы. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::Sleepy => (
                "Сонная Мита — собеседница игрока.",
                "Отвечай от лица Сонной Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::Ghost => (
                "Призрачная Мита — собеседница игрока.",
                "Отвечай от лица Призрачной Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::Creepy => (
                "Жуткая Мита — собеседница игрока.",
                "Отвечай от лица Жуткой Миты. Учитывай только предоставленные тебе сведения и историю.",
            ),
            CharacterId::GameMaster => (
                "Мастер игры — ведущий разговора.",
                "Отвечай от лица Мастера игры. Учитывай только предоставленные тебе сведения и историю.",
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
    /// Own active reminders of the selected branch; never another character's.
    pub reminders: Vec<crate::reminders::ReminderPrompt>,
    pub current_input: Arc<str>,
    pub focus: GenerationFocus,
    pub anchor: ContextAnchor,
    pub(crate) turn: TimelineTurn,
}

#[derive(Debug, Clone)]
pub struct CharacterView {
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
