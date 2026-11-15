use std::rc::Rc;

use neuromita::{
    games::chess::{ChessDifficulty, ChessProjection, ChessResult, ChessSide},
    presentation::ChessAction,
};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{
    runtime::Command,
    view::{AppWindow, ChessCell, ChessUi},
};

pub(super) fn bind(window: &AppWindow, dispatch: Rc<impl Fn(Command) + 'static>) {
    let ui = window.global::<ChessUi>();
    let sender = dispatch.clone();
    let weak = window.as_weak();
    ui.on_opened(move || request(&weak, &*sender, ChessAction::Inspect));
    let sender = dispatch.clone();
    let weak = window.as_weak();
    ui.on_start(move |player_white, difficulty| {
        let difficulty = match difficulty {
            0 => ChessDifficulty::Easy,
            1 => ChessDifficulty::Medium,
            2 => ChessDifficulty::Hard,
            _ => return,
        };
        request(
            &weak,
            &*sender,
            ChessAction::Start {
                player_white,
                difficulty,
            },
        );
    });
    let sender = dispatch.clone();
    let weak = window.as_weak();
    ui.on_played(move |from, to, promotion| {
        request(
            &weak,
            &*sender,
            ChessAction::Move {
                from: from.into(),
                to: to.into(),
                promotion: promotion.to_ascii_lowercase(),
            },
        );
    });
    let weak = window.as_weak();
    ui.on_close(move || request(&weak, &*dispatch, ChessAction::Close));
}

fn request(weak: &slint::Weak<AppWindow>, send: &impl Fn(Command), action: ChessAction) {
    if let Some(window) = weak.upgrade() {
        let ui = window.global::<ChessUi>();
        if !ui.get_busy() {
            ui.set_busy(true);
            ui.set_error("".into());
            send(Command::Chess(action));
        }
    }
}

pub(super) fn render(window: &AppWindow, result: Result<Option<Box<ChessProjection>>, String>) {
    let ui = window.global::<ChessUi>();
    ui.set_busy(false);
    let game = match result {
        Ok(game) => game,
        Err(error) => {
            ui.set_error(error.into());
            return;
        }
    };
    let Some(game) = game else {
        ui.set_board(ModelRc::new(VecModel::<ChessCell>::default()));
        ui.set_status(0);
        ui.set_history("".into());
        return;
    };
    let player_white = game.player_white;
    let player_side = if player_white {
        ChessSide::White
    } else {
        ChessSide::Black
    };
    ui.set_status(match game.view.result {
        ChessResult::Ongoing if game.player_turn() => 1,
        ChessResult::Ongoing => 2,
        ChessResult::Checkmate { winner } if winner == player_side => 3,
        ChessResult::Checkmate { .. } => 4,
        ChessResult::Draw(_) => 5,
    });
    let cells = game
        .view
        .board
        .iter()
        .enumerate()
        .flat_map(|(row, rank)| {
            rank.chars()
                .enumerate()
                .map(move |(file, piece)| ChessCell {
                    square: format!("{}{}", char::from(b'a' + file as u8), 8 - row).into(),
                    piece: glyph(piece).into(),
                    dark: (row + file) % 2 == 1,
                    selectable: piece != '.' && piece.is_ascii_uppercase() == player_white,
                })
        })
        .collect::<Vec<_>>();
    let cells = if player_white {
        cells
    } else {
        cells.into_iter().rev().collect()
    };
    ui.set_board(ModelRc::new(VecModel::from(cells)));
    let history = &game.view.history;
    ui.set_history(
        history[history.len().saturating_sub(16)..]
            .iter()
            .map(|turn| turn.san.as_str())
            .collect::<Vec<_>>()
            .join("  ")
            .into(),
    );
}

fn glyph(piece: char) -> &'static str {
    match piece {
        'K' => "♔",
        'Q' => "♕",
        'R' => "♖",
        'B' => "♗",
        'N' => "♘",
        'P' => "♙",
        'k' => "♚",
        'q' => "♛",
        'r' => "♜",
        'b' => "♝",
        'n' => "♞",
        'p' => "♟",
        _ => "",
    }
}
