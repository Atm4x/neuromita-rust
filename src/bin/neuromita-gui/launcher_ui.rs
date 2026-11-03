use std::path::{Path, PathBuf};

use neuromita::{
    launcher::{
        ConfigStore, LauncherApi, LauncherConfig, LauncherError, LauncherSession, OwnedProcessState,
    },
    localization::{LocalizedText, i18n},
};

pub struct LauncherUi {
    session: LauncherSession,
}

impl LauncherUi {
    pub fn new(config: LauncherConfig) -> Self {
        Self {
            session: LauncherApi::session(config),
        }
    }

    pub fn toggle_play(&mut self) -> LocalizedText {
        let result = match self.session.owned_state() {
            Ok(OwnedProcessState::Running { .. }) => self
                .session
                .stop()
                .map(|_| i18n("launcher.game.stopped", "Игра остановлена.")),
            Ok(OwnedProcessState::Idle | OwnedProcessState::Finished { .. }) => {
                self.session.start().map(|state| {
                    i18n("launcher.game.started", "Игра запущена (PID {pid}).")
                        .arg("pid", state.pid().unwrap_or_default().to_string())
                })
            }
            Ok(OwnedProcessState::StatusError { pid, message }) => {
                Err(LauncherError::Process { pid, message })
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(notice) => notice,
            Err(error) => i18n(
                "launcher.game.operation-error",
                "Не удалось запустить или остановить игру: {error}",
            )
            .arg("error", error.to_string()),
        }
    }

    pub fn open_location(&self) -> LocalizedText {
        match self.session.open_location() {
            Ok(()) => i18n("launcher.game.location-opened", "Каталог Unity открыт."),
            Err(error) => i18n(
                "launcher.game.location-error",
                "Не удалось открыть каталог Unity: {error}",
            )
            .arg("error", error.to_string()),
        }
    }

    pub fn owned_running(&mut self) -> bool {
        matches!(
            self.session.owned_state(),
            Ok(OwnedProcessState::Running { .. })
        )
    }
}

pub fn load_config(
    unity_directory: Option<&Path>,
    config_path: Option<&Path>,
) -> Result<LauncherConfig, Box<dyn std::error::Error>> {
    if unity_directory.is_some() && config_path.is_some() {
        return Err("--unity-dir and --launcher-config cannot be combined".into());
    }
    if let Some(path) = config_path {
        return Ok(ConfigStore::new(path).load()?);
    }
    let root = match unity_directory {
        Some(root) => root.to_path_buf(),
        None => default_unity_directory()?,
    };
    let config = LauncherConfig::new(root);
    config.validate(Path::new("--unity-dir"))?;
    Ok(config)
}

fn default_unity_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(root) = std::env::var_os("NEUROMITA_BASE_DIR") {
        if root.is_empty() {
            return Err("NEUROMITA_BASE_DIR must not be empty".into());
        }
        return Ok(PathBuf::from(root).join("NeuroMita-Unity"));
    }
    Ok(std::env::current_exe()?
        .parent()
        .ok_or("executable has no parent directory")?
        .join("NeuroMita-Unity"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_unity_directory_builds_a_launcher_config() {
        let root = Path::new("test-unity-install");
        let config = load_config(Some(root), None).unwrap();
        assert_eq!(config.install_root, root);
        assert!(config.candidate.is_none());
    }

    #[test]
    fn cannot_override_a_stored_launcher_config_silently() {
        assert!(load_config(Some(Path::new("unity")), Some(Path::new("launcher.json"))).is_err());
    }

    #[test]
    fn missing_explicit_config_is_an_error() {
        let path =
            std::env::temp_dir().join(format!("absent-launcher-{}.json", uuid::Uuid::new_v4()));
        assert!(matches!(load_config(None, Some(&path)), Err(_)));
    }
    #[cfg(unix)]
    #[test]
    fn play_button_starts_and_stops_the_owned_game() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("neuromita-launcher-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let executable = root.join("NeuroMita.exe");
        std::fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut launcher = LauncherUi::new(LauncherConfig::new(&root));
        assert!(!launcher.owned_running());
        let start = launcher.toggle_play();
        assert_eq!(start.id(), "launcher.game.started");
        assert!(launcher.owned_running());
        let stop = launcher.toggle_play();
        assert_eq!(stop.id(), "launcher.game.stopped");
        assert!(!launcher.owned_running());
        drop(launcher);
        std::fs::remove_dir_all(root).unwrap();
    }
}
