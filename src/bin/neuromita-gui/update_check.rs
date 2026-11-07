//! Non-blocking read-only release check for the installed launcher.
//!
//! Python and Unity ZIP releases are not eligible for the raw-executable
//! updater. This adapter deliberately never stages or activates them.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use neuromita::updater::{CheckedRelease, GithubUpdateCatalog};
use slint::ComponentHandle;

use super::view::{AppWindow, I18n};

const RELEASE_REPOSITORY: &str = "Atm4x/NeuroMita";

#[derive(Default)]
pub struct UpdateChecker {
    busy: Arc<AtomicBool>,
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
        if self.python_size.is_some() {
            components.push("Python ZIP");
        }
        if self.unity_size.is_some() {
            components.push("Unity ZIP");
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
    pub fn check(&self, window: &AppWindow) {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        window.set_update_check_loading(true);
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
        let spawned = thread::Builder::new()
            .name("neuromita-release-check".into())
            .spawn(move || {
                let outcome = (|| -> Result<PublishedArchives, String> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    let catalog = GithubUpdateCatalog::new(RELEASE_REPOSITORY)
                        .map_err(|error| error.to_string())?;
                    let release = runtime
                        .block_on(catalog.latest_release())
                        .map_err(|error| error.to_string())?;
                    Ok(PublishedArchives::inspect(&release))
                })();
                let failed = Arc::clone(&busy);
                let dispatched = weak.upgrade_in_event_loop(move |window| {
                    let russian = window.global::<I18n>().get_locale() == "ru";
                    let message = match outcome {
                        Ok(archives) => archives.summary(russian),
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
