//! Non-blocking release inspection and verified ZIP download for the launcher.
//! Archive staging never installs Python/Unity packages or invokes the exe helper.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use neuromita::{
    portable::PortableDataRoot,
    updater::{
        CheckedRelease, GithubUpdateCatalog, UpdateStager,
        package::{ArchiveComponent, published_archive},
    },
};
use slint::ComponentHandle;
use tokio_util::sync::CancellationToken;

use super::view::{AppWindow, I18n};

const RELEASE_REPOSITORY: &str = "Atm4x/NeuroMita";

pub struct UpdateChecker {
    busy: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<CheckedRelease>>>,
    cancellation: Arc<Mutex<Option<CancellationToken>>>,
    data_directory: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct PublishedArchives {
    tag: String,
    python_size: Option<u64>,
    unity_size: Option<u64>,
}

impl PublishedArchives {
    fn inspect(release: &CheckedRelease) -> Self {
        let python_name = format!("PythonBuild-{}.zip", release.tag);
        let unity_name = format!("UnityBuild-{}.zip", release.tag);
        Self {
            tag: release.tag.clone(),
            python_size: release
                .assets
                .iter()
                .find(|asset| asset.name() == python_name)
                .map(|asset| asset.size()),
            unity_size: release
                .assets
                .iter()
                .find(|asset| asset.name() == unity_name)
                .map(|asset| asset.size()),
        }
    }

    fn summary(&self, russian: bool) -> String {
        let mut components = Vec::new();
        if let Some(size) = self.python_size {
            components.push(format!("Python ZIP ({} MB)", size / 1_000_000));
        }
        if let Some(size) = self.unity_size {
            components.push(format!("Unity ZIP ({} MB)", size / 1_000_000));
        }
        if components.is_empty() {
            return if russian {
                format!(
                    "Релиз {} найден, но поддерживаемые архивы Python/Unity отсутствуют. Установка недоступна.",
                    self.tag
                )
            } else {
                format!(
                    "Release {} found, but supported Python/Unity archives are missing. Installation unavailable.",
                    self.tag
                )
            };
        }
        let names = components.join(" + ");
        if russian {
            format!(
                "Опубликован релиз {} ({names}). Это ZIP-архивы Python/Unity, а не Rust exe. Автоустановка пока недоступна.",
                self.tag
            )
        } else {
            format!(
                "Published release {} ({names}). These are Python/Unity ZIP archives, not the Rust executable. Automatic installation is not available yet.",
                self.tag
            )
        }
    }
}

impl UpdateChecker {
    pub fn new(data_directory: PathBuf) -> Self {
        Self {
            busy: Arc::new(AtomicBool::new(false)),
            latest: Arc::new(Mutex::new(None)),
            cancellation: Arc::new(Mutex::new(None)),
            data_directory,
        }
    }

    pub fn check(&self, window: &AppWindow) {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        window.set_update_check_loading(true);
        window.set_update_can_stage_python(false);
        window.set_update_can_stage_unity(false);
        *self.latest.lock().expect("update state poisoned") = None;
        let russian = window.global::<I18n>().get_locale() == "ru";
        window.set_update_check_status(
            if russian {
                "Проверяем опубликованные релизы…"
            } else {
                "Checking published releases…"
            }
            .into(),
        );
        let weak = window.as_weak();
        let busy = Arc::clone(&self.busy);
        let latest = Arc::clone(&self.latest);
        let spawned = thread::Builder::new()
            .name("neuromita-release-check".into())
            .spawn(move || {
                let outcome = (|| -> Result<CheckedRelease, String> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    let catalog = GithubUpdateCatalog::new(RELEASE_REPOSITORY)
                        .map_err(|error| error.to_string())?;
                    let release = runtime
                        .block_on(catalog.latest_release())
                        .map_err(|error| error.to_string())?;
                    Ok(release)
                })();
                let failed = Arc::clone(&busy);
                let dispatched = weak.upgrade_in_event_loop(move |window| {
                    let russian = window.global::<I18n>().get_locale() == "ru";
                    let message = match outcome {
                        Ok(release) => {
                            let archives = PublishedArchives::inspect(&release);
                            window.set_update_can_stage_python(archives.python_size.is_some());
                            window.set_update_can_stage_unity(archives.unity_size.is_some());
                            *latest.lock().expect("update state poisoned") = Some(release);
                            archives.summary(russian)
                        }
                        Err(error) if russian => format!("Не удалось проверить релизы: {error}"),
                        Err(error) => format!("Release check failed: {error}"),
                    };
                    window.set_update_check_status(message.into());
                    window.set_update_check_loading(false);
                    busy.store(false, Ordering::Release);
                });
                if dispatched.is_err() {
                    failed.store(false, Ordering::Release);
                }
            });
        if let Err(error) = spawned {
            self.busy.store(false, Ordering::Release);
            window.set_update_check_loading(false);
            window
                .set_update_check_status(format!("Release check could not start: {error}").into());
        }
    }

    pub fn cancel_stage(&self) {
        if let Some(token) = self
            .cancellation
            .lock()
            .expect("cancel state poisoned")
            .as_ref()
        {
            token.cancel();
        }
    }

    pub fn stage(&self, window: &AppWindow, identifier: &str) {
        let Some(component) = ArchiveComponent::from_ui_id(identifier) else {
            window.set_update_check_status("Unsupported archive component".into());
            return;
        };
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let candidate = self
            .latest
            .lock()
            .expect("update state poisoned")
            .as_ref()
            .and_then(|release| published_archive(release, component))
            .cloned();
        let Some(asset) = candidate else {
            self.busy.store(false, Ordering::Release);
            window.set_update_check_status("No verified release archive selected".into());
            return;
        };
        let cancellation = CancellationToken::new();
        *self.cancellation.lock().expect("cancel state poisoned") = Some(cancellation.clone());
        window.set_update_stage_loading(true);
        let russian = window.global::<I18n>().get_locale() == "ru";
        let initial_message = if russian {
            format!(
                "Загрузка {} ({} МБ)… Только проверка SHA-256, без установки.",
                component.label(),
                asset.size() / 1_000_000
            )
        } else {
            format!(
                "Downloading {} ({} MB)… SHA-256 verification only; no installation.",
                component.label(),
                asset.size() / 1_000_000
            )
        };
        window.set_update_check_status(initial_message.into());
        let weak = window.as_weak();
        let busy = Arc::clone(&self.busy);
        let cancellation_slot = Arc::clone(&self.cancellation);
        let data_directory = self.data_directory.clone();
        let spawned = thread::Builder::new()
            .name("neuromita-archive-stage".into())
            .spawn(move || {
                let result = (|| -> Result<bool, String> {
                    let root = PortableDataRoot::resolve(Some(&data_directory))
                        .map_err(|error| error.to_string())?;
                    let stager = UpdateStager::new(&root).map_err(|error| error.to_string())?;
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    runtime
                        .block_on(stager.stage(&asset, &cancellation))
                        .map(|artifact| artifact.reused())
                        .map_err(|error| error.to_string())
                })();
                let failed = Arc::clone(&busy);
                let fallback_cancel = Arc::clone(&cancellation_slot);
                let dispatch = weak.upgrade_in_event_loop(move |window| {
                    let russian = window.global::<I18n>().get_locale() == "ru";
                    let status = match result {
                        Ok(reused) if russian => format!(
                            "{}: проверенный ZIP {}. Установка пока не поддерживается.",
                            component.label(),
                            if reused {
                                "уже загружен"
                            } else {
                                "загружен"
                            }
                        ),
                        Ok(reused) => format!(
                            "{}: verified ZIP {}. Installation is not supported yet.",
                            component.label(),
                            if reused { "already staged" } else { "staged" }
                        ),
                        Err(error) if russian => {
                            format!("Не удалось проверить/загрузить ZIP: {error}")
                        }
                        Err(error) => format!("Unable to stage/verify ZIP: {error}"),
                    };
                    window.set_update_check_status(status.into());
                    window.set_update_stage_loading(false);
                    *cancellation_slot.lock().expect("cancel state poisoned") = None;
                    busy.store(false, Ordering::Release);
                });
                if dispatch.is_err() {
                    *fallback_cancel.lock().expect("cancel state poisoned") = None;
                    failed.store(false, Ordering::Release);
                }
            });
        if let Err(error) = spawned {
            *self.cancellation.lock().expect("cancel state poisoned") = None;
            self.busy.store(false, Ordering::Release);
            window.set_update_stage_loading(false);
            window.set_update_check_status(
                format!("Archive download could not start: {error}").into(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neuromita::updater::UpdateAsset;

    fn asset(name: &str) -> UpdateAsset {
        UpdateAsset::new(
            name,
            "https://github.com/Atm4x/NeuroMita/releases/download/sample/asset",
            100,
            &format!("sha256:{}", "0".repeat(64)),
        )
        .unwrap()
    }

    #[test]
    fn actual_archive_names_are_reported_without_claiming_installability() {
        let release = CheckedRelease {
            tag: "v2026.11.01".into(),
            assets: vec![
                asset("PythonBuild-v2026.11.01.zip"),
                asset("UnityBuild-v2026.11.01.zip"),
                asset("releases-manifest.json"),
            ],
        };
        let result = PublishedArchives::inspect(&release);
        assert_eq!(result.python_size, Some(100));
        assert_eq!(result.unity_size, Some(100));
        for locale in [true, false] {
            let summary = result.summary(locale);
            assert!(summary.contains("v2026.11.01"));
            assert!(summary.contains("Python ZIP"));
            assert!(summary.contains("Unity ZIP"));
            assert!(!summary.contains("installed"));
        }
    }

    #[test]
    fn unexpected_names_and_rust_executables_do_not_become_archive_updates() {
        let release = CheckedRelease {
            tag: "v2026.11.01".into(),
            assets: vec![
                asset("PythonBuild-v2025.01.01.zip"),
                asset("UnityBuild-v2026.11.01.7z"),
                asset("neuromita-gui.exe"),
            ],
        };
        let inspection = PublishedArchives::inspect(&release);
        assert_eq!(inspection.python_size, None);
        assert_eq!(inspection.unity_size, None);
        assert!(inspection.summary(true).contains("Установка недоступна"));
    }

    #[test]
    fn missing_one_component_is_not_reported_as_both_components() {
        let release = CheckedRelease {
            tag: "v2026.11.01".into(),
            assets: vec![asset("PythonBuild-v2026.11.01.zip")],
        };
        let inspection = PublishedArchives::inspect(&release);
        assert_eq!(inspection.python_size, Some(100));
        assert_eq!(inspection.unity_size, None);
        assert!(!inspection.summary(true).contains("Unity ZIP"));
    }
}
