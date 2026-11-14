//! Pure, replay-validated chess rules built on the MIT cozy-chess engine.
//! Serialization stores root FEN + accepted UCI moves, never a second board.
use cozy_chess::{
    Board, Color, GameStatus, Piece, Square,
    util::{display_san_move, display_uci_move, parse_san_move, parse_uci_move},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_MOVES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChessSide {
    White,
    Black,
}

impl From<Color> for ChessSide {
    fn from(value: Color) -> Self {
        match value {
            Color::White => Self::White,
            Color::Black => Self::Black,
        }
    }
}

impl ChessSide {
    pub fn opposite(self) -> Self {
        match self {
            Self::White => Self::Black,
            Self::Black => Self::White,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawReason {
    Stalemate,
    InsufficientMaterial,
    ThreefoldRepetition,
    FivefoldRepetition,
    FiftyMoves,
    SeventyFiveMoves,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChessResult {
    Ongoing,
    Checkmate { winner: ChessSide },
    Draw(DrawReason),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ChessError {
    #[error("invalid FEN")]
    InvalidFen,
    #[error("invalid or illegal chess move")]
    IllegalMove,
    #[error("chess game has finished")]
    Finished,
    #[error("too many moves")]
    MoveLimit,
    #[error("corrupt accepted chess history")]
    CorruptHistory,
}

/// The canonical, serializable rule state. No serialized copy of cozy Board
/// survives a restore; all moves must replay legally on any historical prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChessGame {
    root_fen: String,
    moves: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRecord {
    pub uci: String,
    pub san: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChessView {
    pub fen: String,
    pub turn: ChessSide,
    pub result: ChessResult,
    pub history: Vec<MoveRecord>,
    pub legal_uci: Vec<String>,
    pub board: Vec<String>,
}

struct Replayed {
    board: Board,
    positions: Vec<Board>,
    history: Vec<MoveRecord>,
    halfmoves: u32,
}

impl Default for ChessGame {
    fn default() -> Self {
        Self::new()
    }
}

impl ChessGame {
    pub fn new() -> Self {
        Self {
            root_fen: Board::startpos().to_string(),
            moves: Vec::new(),
        }
    }

    pub fn from_fen(fen: impl Into<String>) -> Result<Self, ChessError> {
        let fen = fen.into();
        let board: Board = fen.parse().map_err(|_| ChessError::InvalidFen)?;
        let game = Self {
            root_fen: board.to_string(),
            moves: Vec::new(),
        };
        game.validate()?;
        Ok(game)
    }

    pub fn root_fen(&self) -> &str {
        &self.root_fen
    }

    pub fn uci_history(&self) -> &[String] {
        &self.moves
    }

    pub fn revision(&self) -> u64 {
        self.moves.len() as u64
    }

    /// Validate after deserialization, before accepting this as Timeline state.
    pub fn validate(&self) -> Result<(), ChessError> {
        self.replay().map(|_| ())
    }

    pub fn view(&self) -> Result<ChessView, ChessError> {
        let replay = self.replay()?;
        let result = adjudicate(&replay);
        let mut legal_uci = Vec::new();
        if result == ChessResult::Ongoing {
            replay.board.generate_moves(|piece_moves| {
                for mv in piece_moves {
                    legal_uci.push(display_uci_move(&replay.board, mv).to_string());
                }
                false
            });
            legal_uci.sort();
        }
        Ok(ChessView {
            fen: replay.board.to_string(),
            turn: replay.board.side_to_move().into(),
            result,
            history: replay.history,
            legal_uci,
            board: board_text(&replay.board),
        })
    }

    /// Rejects illegal moves atomically, including underpromotion omissions and
    /// moves after a terminal position. The stored notation is canonical UCI.
    pub fn play_uci(&mut self, uci: &str) -> Result<MoveRecord, ChessError> {
        let replay = self.replay()?;
        if adjudicate(&replay) != ChessResult::Ongoing {
            return Err(ChessError::Finished);
        }
        let mv = parse_uci_move(&replay.board, uci).map_err(|_| ChessError::IllegalMove)?;
        self.commit_move(&replay.board, mv)
    }

    pub fn play_san(&mut self, san: &str) -> Result<MoveRecord, ChessError> {
        let replay = self.replay()?;
        if adjudicate(&replay) != ChessResult::Ongoing {
            return Err(ChessError::Finished);
        }
        let mv = parse_san_move(&replay.board, san).map_err(|_| ChessError::IllegalMove)?;
        self.commit_move(&replay.board, mv)
    }

    fn commit_move(
        &mut self,
        board: &Board,
        mv: cozy_chess::Move,
    ) -> Result<MoveRecord, ChessError> {
        if self.moves.len() >= MAX_MOVES {
            return Err(ChessError::MoveLimit);
        }
        let mut next = board.clone();
        next.try_play(mv).map_err(|_| ChessError::IllegalMove)?;
        let record = MoveRecord {
            uci: display_uci_move(board, mv).to_string(),
            san: display_san_move(board, mv).to_string(),
        };
        self.moves.push(record.uci.clone());
        Ok(record)
    }

    fn replay(&self) -> Result<Replayed, ChessError> {
        if self.moves.len() > MAX_MOVES {
            return Err(ChessError::MoveLimit);
        }
        let mut board: Board = self.root_fen.parse().map_err(|_| ChessError::InvalidFen)?;
        let mut positions = vec![board.clone()];
        let mut halfmoves = self
            .root_fen
            .split_ascii_whitespace()
            .nth(4)
            .ok_or(ChessError::InvalidFen)?
            .parse::<u32>()
            .map_err(|_| ChessError::InvalidFen)?;
        let mut history = Vec::with_capacity(self.moves.len());
        for uci in &self.moves {
            if adjudicate_partial(&board, &positions, halfmoves) != ChessResult::Ongoing {
                return Err(ChessError::CorruptHistory);
            }
            let mv = parse_uci_move(&board, uci).map_err(|_| ChessError::CorruptHistory)?;
            let canonical = display_uci_move(&board, mv).to_string();
            if &canonical != uci {
                return Err(ChessError::CorruptHistory);
            }
            let pawn = board.piece_on(mv.from) == Some(Piece::Pawn);
            let capture =
                board.piece_on(mv.to).is_some() || (pawn && mv.from.file() != mv.to.file());
            let san = display_san_move(&board, mv).to_string();
            board.try_play(mv).map_err(|_| ChessError::CorruptHistory)?;
            halfmoves = if pawn || capture {
                0
            } else {
                halfmoves.checked_add(1).ok_or(ChessError::CorruptHistory)?
            };
            positions.push(board.clone());
            history.push(MoveRecord {
                uci: canonical,
                san,
            });
        }
        Ok(Replayed {
            board,
            positions,
            history,
            halfmoves,
        })
    }
}

fn adjudicate(replay: &Replayed) -> ChessResult {
    adjudicate_partial(&replay.board, &replay.positions, replay.halfmoves)
}

fn adjudicate_partial(board: &Board, positions: &[Board], halfmoves: u32) -> ChessResult {
    // Mate outranks a 75-move draw on the mating move.
    if board.status() == GameStatus::Won {
        return ChessResult::Checkmate {
            winner: ChessSide::from(board.side_to_move()).opposite(),
        };
    }
    if !board.generate_moves(|_| true) {
        return ChessResult::Draw(DrawReason::Stalemate);
    }
    if insufficient_material(board) {
        return ChessResult::Draw(DrawReason::InsufficientMaterial);
    }
    let repeats = positions
        .iter()
        .filter(|past| past.same_position(board))
        .count();
    if repeats >= 5 {
        return ChessResult::Draw(DrawReason::FivefoldRepetition);
    }
    if halfmoves >= 150 {
        return ChessResult::Draw(DrawReason::SeventyFiveMoves);
    }
    // Python parity: claimable draws end the game immediately.
    if repeats >= 3 {
        return ChessResult::Draw(DrawReason::ThreefoldRepetition);
    }
    if halfmoves >= 100 {
        return ChessResult::Draw(DrawReason::FiftyMoves);
    }
    ChessResult::Ongoing
}

fn insufficient_material(board: &Board) -> bool {
    if [Piece::Pawn, Piece::Rook, Piece::Queen]
        .into_iter()
        .any(|piece| !board.pieces(piece).is_empty())
    {
        return false;
    }
    let bishops = board.pieces(Piece::Bishop);
    let knights = board.pieces(Piece::Knight);
    if bishops.len() + knights.len() <= 1 {
        return true;
    }
    // All bishops on one square color cannot produce checkmate.
    if knights.is_empty() {
        let mut square_colors = bishops
            .iter()
            .map(|square| ((square.file() as u8) + (square.rank() as u8)) % 2);
        if let Some(color) = square_colors.next() {
            return square_colors.all(|candidate| candidate == color);
        }
    }
    false
}

fn board_text(board: &Board) -> Vec<String> {
    (0..8)
        .rev()
        .map(|rank| {
            (0..8)
                .map(|file| {
                    let square = Square::ALL[rank * 8 + file];
                    match board.piece_on(square) {
                        None => '.',
                        Some(piece) => {
                            let symbol = piece.to_string().chars().next().unwrap_or('?');
                            if board.color_on(square) == Some(Color::White) {
                                symbol.to_ascii_uppercase()
                            } else {
                                symbol.to_ascii_lowercase()
                            }
                        }
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_fools_mate_and_san() {
        let mut game = ChessGame::new();
        for mv in ["f2f3", "e7e5", "g2g4", "d8h4"] {
            game.play_uci(mv).unwrap();
        }
        assert_eq!(
            game.view().unwrap().result,
            ChessResult::Checkmate {
                winner: ChessSide::Black
            }
        );
        assert_eq!(game.view().unwrap().history[3].san, "Qh4#");
        assert!(matches!(game.play_uci("a2a3"), Err(ChessError::Finished)));
    }

    #[test]
    fn validates_castling_and_en_passant() {
        let mut castle = ChessGame::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").unwrap();
        assert_eq!(castle.play_uci("e1g1").unwrap().san, "O-O");
        let mut en_passant = ChessGame::new();
        for mv in ["e2e4", "a7a6", "e4e5", "d7d5"] {
            en_passant.play_uci(mv).unwrap();
        }
        assert_eq!(en_passant.play_uci("e5d6").unwrap().san, "exd6");
    }

    #[test]
    fn promotion_is_explicit_and_includes_underpromotions() {
        let fen = "4k3/P7/8/8/8/8/8/4K3 w - - 0 1";
        let game = ChessGame::from_fen(fen).unwrap();
        assert!(game.view().unwrap().legal_uci.contains(&"a7a8n".into()));
        for suffix in ["q", "r", "b", "n"] {
            let mut candidate = game.clone();
            candidate.play_uci(&format!("a7a8{suffix}")).unwrap();
        }
        let mut candidate = game;
        assert!(candidate.play_uci("a7a8").is_err());
    }

    #[test]
    fn repetition_is_restored_from_history() {
        let mut game = ChessGame::new();
        for mv in [
            "g1f3", "g8f6", "f3g1", "f6g8", "g1f3", "g8f6", "f3g1", "f6g8",
        ] {
            game.play_uci(mv).unwrap();
        }
        assert_eq!(
            game.view().unwrap().result,
            ChessResult::Draw(DrawReason::ThreefoldRepetition)
        );
        let stored = serde_json::to_string(&game).unwrap();
        let restored: ChessGame = serde_json::from_str(&stored).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.view().unwrap(), game.view().unwrap());
    }

    #[test]
    fn fifty_move_and_material_draws() {
        let mut count = ChessGame::from_fen("4k3/8/8/8/8/8/4K2P/8 w - - 99 1").unwrap();
        count.play_uci("e2d2").unwrap();
        assert_eq!(
            count.view().unwrap().result,
            ChessResult::Draw(DrawReason::FiftyMoves)
        );
        let bare = ChessGame::from_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(
            bare.view().unwrap().result,
            ChessResult::Draw(DrawReason::InsufficientMaterial)
        );
    }

    #[test]
    fn illegal_move_never_mutates_game() {
        let mut game = ChessGame::new();
        let previous = game.clone();
        assert!(matches!(
            game.play_uci("e2e5"),
            Err(ChessError::IllegalMove)
        ));
        assert_eq!(game, previous);
        game.play_san("e4").unwrap();
        assert_eq!(game.uci_history(), ["e2e4"]);
    }

    #[test]
    fn corrupted_history_rejects_replay() {
        let mut game = ChessGame::new();
        game.moves.push("a1a8".into());
        assert!(matches!(game.validate(), Err(ChessError::CorruptHistory)));
        let malformed = serde_json::json!({
            "root_fen": Board::startpos().to_string(),
            "moves": [],
            "hidden_field": true
        });
        assert!(serde_json::from_value::<ChessGame>(malformed).is_err());
    }
}
