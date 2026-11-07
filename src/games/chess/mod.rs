//! Rust chess integrations. The UCI adapter is non-authoritative.
pub mod game;
pub mod uci;

pub use game::{ChessError, ChessGame, ChessResult, ChessSide, ChessView, DrawReason, MoveRecord};
