use serde::{Deserialize, Serialize};

use crate::{
    character::{CharacterError, CharacterId},
    games::{
        GameError, UserGameCommand,
        chess::{ChessDifficulty, ChessProjection, ChessResult, ChessUserCommand, basic_reply},
    },
};

use super::CharactersPresenter;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChessAction {
    Inspect,
    Start {
        player_white: bool,
        difficulty: ChessDifficulty,
    },
    Move {
        from: String,
        to: String,
        promotion: String,
    },
    Close,
}

impl CharactersPresenter {
    pub async fn chess_action(
        &self,
        id: CharacterId,
        action: ChessAction,
    ) -> Result<Option<ChessProjection>, CharacterError> {
        let character = self.get(id)?;
        let command = match action {
            ChessAction::Inspect => return Ok(character.game_view().await?.chess),
            ChessAction::Start {
                player_white,
                difficulty,
            } => ChessUserCommand::Start {
                player_white,
                difficulty,
                auto_engine: true,
                auto_reactions: false,
            },
            ChessAction::Move {
                from,
                to,
                promotion,
            } => {
                if from.len() != 2
                    || to.len() != 2
                    || !matches!(promotion.as_str(), "" | "q" | "r" | "b" | "n")
                {
                    return Err(CharacterError::InvalidGame(GameError::Invalid(
                        "invalid chess move coordinates",
                    )));
                }
                let receipt = character.game_view().await?;
                let view = receipt
                    .chess
                    .ok_or(CharacterError::InvalidGame(GameError::NoActiveGame))?;
                let prefix = format!("{from}{to}");
                let with_promotion = format!("{prefix}{promotion}");
                let legal = view
                    .view
                    .legal_uci
                    .iter()
                    .find(|uci| **uci == prefix || **uci == with_promotion)
                    .ok_or(CharacterError::InvalidGame(GameError::Invalid(
                        "illegal chess move",
                    )))?;
                ChessUserCommand::Move(legal.clone())
            }
            ChessAction::Close => ChessUserCommand::Close,
        };
        let receipt = character
            .game(UserGameCommand::Chess(command))
            .await?
            .receipt;
        if let (Some(game), Some(expected)) = (&receipt.chess, receipt.key)
            && game.auto_engine
            && !game.player_turn()
            && game.view.result == ChessResult::Ongoing
            && let Some(uci) = basic_reply(game)
        {
            return Ok(character
                .game(UserGameCommand::Chess(ChessUserCommand::EngineMove {
                    expected,
                    uci: uci.to_owned(),
                }))
                .await?
                .receipt
                .chess);
        }
        Ok(receipt.chess)
    }
}
