//! Shared stateless debug field projection for GUI and MCP.
use super::{CharacterDebugField, CharactersPresenter, SettingsPresenter};
use crate::{character::CharacterId, prompts::PromptCatalog};

/// Use the exact same selected Post DSL and current character state in either interface.
/// Returned values are user-authored content and must be protected by the transport.
pub async fn project_character_debug(
    characters: &CharactersPresenter,
    settings: &SettingsPresenter,
    catalog: Option<&PromptCatalog>,
    character: CharacterId,
) -> Result<Vec<CharacterDebugField>, String> {
    let post_dsl = if let Some(catalog) = catalog {
        let settings = settings.snapshot().map_err(|error| error.to_string())?;
        let selected = settings
            .values
            .characters
            .for_character(character)
            .prompt_set
            .as_deref();
        if selected.is_none()
            && catalog
                .list(character)
                .map_err(|error| error.to_string())?
                .is_empty()
        {
            None
        } else {
            let name = catalog
                .resolve_set(character, selected)
                .map_err(|error| error.to_string())?;
            catalog
                .load_post_rules(character, &name)
                .map_err(|error| error.to_string())?
        }
    } else {
        None
    };
    characters
        .debug_display(character, post_dsl.as_ref())
        .await
        .map_err(|error| error.to_string())
}
