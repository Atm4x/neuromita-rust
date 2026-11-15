//! Rust chess integrations. The UCI adapter is non-authoritative.
pub mod game;
pub mod uci;

pub use game::{ChessError, ChessGame, ChessResult, ChessSide, ChessView, DrawReason, MoveRecord};

use serde::{Deserialize, Serialize};

use crate::games::{GameKey, GameSessionId};

/// Stable difficulty selection from Python: Maia 1100/1500/1900.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChessDifficulty {
    Easy,
    Medium,
    Hard,
}

impl ChessDifficulty {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "easy" => Some(Self::Easy),
            "medium" => Some(Self::Medium),
            "hard" => Some(Self::Hard),
            _ => None,
        }
    }

    pub const fn maia_elo(self) -> u16 {
        match self {
            Self::Easy => 1100,
            Self::Medium => 1500,
            Self::Hard => 1900,
        }
    }
}

/// Typed commands for the single canonical GameSlot, not a second registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChessUserCommand {
    Start {
        player_white: bool,
        difficulty: ChessDifficulty,
        auto_engine: bool,
        auto_reactions: bool,
    },
    Move(String),
    EngineMove {
        expected: GameKey,
        uci: String,
    },
    SetDifficulty(ChessDifficulty),
    SetAutoEngine(bool),
    SetAutoReactions(bool),
    Close,
}

/// Owner-only read model. Do not publish FEN/history in global game facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChessProjection {
    pub session: GameSessionId,
    pub revision: u64,
    pub player_white: bool,
    pub difficulty: ChessDifficulty,
    pub auto_engine: bool,
    pub auto_reactions: bool,
    pub view: ChessView,
    pub root_fen: String,
    pub moves: Vec<String>,
}

impl ChessProjection {
    pub fn player_turn(&self) -> bool {
        self.view.turn
            == if self.player_white {
                ChessSide::White
            } else {
                ChessSide::Black
            }
    }

    pub fn status_line(&self) -> String {
        match self.view.result {
            ChessResult::Ongoing if self.player_turn() => "Ход игрока".into(),
            ChessResult::Ongoing => "Ход Миты".into(),
            ChessResult::Checkmate { winner } => format!("Мат. Победитель: {winner:?}"),
            ChessResult::Draw(reason) => format!("Ничья: {reason:?}"),
        }
    }
}

pub(crate) fn basic_reply(game: &ChessProjection) -> Option<&str> {
    game.view
        .legal_uci
        .iter()
        .rev()
        .max_by_key(|uci| {
            let destination = uci.as_bytes();
            let file = i32::from(destination[2] - b'a');
            let rank = i32::from(destination[3] - b'1');
            let captured = game.view.board[(7 - rank) as usize].as_bytes()[file as usize];
            let material = match captured.to_ascii_lowercase() {
                b'p' => 1,
                b'b' | b'n' => 3,
                b'r' => 5,
                b'q' => 9,
                _ => 0,
            };
            let center = 14 - (2 * file - 7).abs() - (2 * rank - 7).abs();
            let promotion = if destination.get(4) == Some(&b'q') {
                9
            } else {
                0
            };
            let weight = match game.difficulty {
                ChessDifficulty::Easy => 3,
                ChessDifficulty::Medium => 8,
                ChessDifficulty::Hard => 16,
            };
            center + weight * (material + promotion)
        })
        .map(String::as_str)
}
