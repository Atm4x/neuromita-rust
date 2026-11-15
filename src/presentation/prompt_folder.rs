//! Host-side prompt-folder operation shared by GUI and MCP.
use super::SettingsPresenter;
use crate::{character::CharacterId, prompts::PromptCatalog};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct PromptFolderPresenter {
    catalog: Option<Arc<PromptCatalog>>,
    settings: Arc<SettingsPresenter>,
}
impl PromptFolderPresenter {
    pub fn new(catalog: Option<Arc<PromptCatalog>>, settings: Arc<SettingsPresenter>) -> Self {
        Self { catalog, settings }
    }
    pub fn locate(&self, character: CharacterId) -> Result<PathBuf, String> {
        let catalog = self
            .catalog
            .as_ref()
            .ok_or("prompt catalog is unavailable")?;
        let snapshot = self.settings.snapshot().map_err(|e| e.to_string())?;
        let selected = snapshot
            .values
            .characters
            .for_character(character)
            .prompt_set
            .as_deref();
        let character_root = catalog.root().join(character.to_string());
        let folder = match selected {
            Some(name) => {
                let name = catalog
                    .resolve_set(character, Some(name))
                    .map_err(|e| e.to_string())?;
                character_root.join(name)
            }
            None => character_root,
        };
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let real = folder.canonicalize().map_err(|e| e.to_string())?;
        let root = catalog.root().canonicalize().map_err(|e| e.to_string())?;
        if !real.starts_with(root) {
            return Err("resolved prompt path escapes catalog root".into());
        }
        Ok(real)
    }
    pub fn open(&self, character: CharacterId) -> Result<(), String> {
        let path = self.locate(character)?;
        let bin = if cfg!(windows) {
            "explorer.exe"
        } else {
            "xdg-open"
        };
        std::process::Command::new(bin)
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
