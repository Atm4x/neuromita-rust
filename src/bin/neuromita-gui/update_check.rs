//! Read-only native application update check, independent of AI Core lifecycle.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use slint::ComponentHandle;

use neuromita::{
    localization::{LocalizedText, i18n},
    updater::check::{NativeUpdateResult, check_native_update},
};

use super::view::AppWindow;

pub struct NativeUpdateChecker {
    window: slint::Weak<AppWindow>,
    pending: Arc<AtomicBool>,
    completed: Arc<Mutex<Option<Result<NativeUpdateResult, String>>>>,
}

impl NativeUpdateChecker {
    pub fn new(window: &AppWindow) -> Self {
        Self {
            window: window.as_weak(),
            pending: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(Mutex::new(None)),
        }
    }

    pub fn check(&self) -> Result<(), CheckStartError> {
        if self.pending.swap(true, Ordering::AcqRel) {
            return Err(CheckStartError::Busy);
        }
        let window = self.window.clone();
        let completed = Arc::clone(&self.completed);
        let spawn = thread::Builder::new()
            .name("neuromita-native-release-check".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| {
                        runtime.block_on(async {
                            match tokio::time::timeout(
                                Duration::from_secs(18),
                                check_native_update(),
                            )
                            .await
                            {
                                Ok(Ok(result)) => Ok(result),
                                Ok(Err(error)) => Err(error.to_string()),
                                Err(_) => Err("Network request timed out".to_owned()),
                            }
                        })
                    });
                *completed.lock().unwrap_or_else(|error| error.into_inner()) = Some(result);
                let _ = window.upgrade_in_event_loop(|window| {
                    window.invoke_native_update_finished();
                });
            });
        if let Err(error) = spawn {
            self.pending.store(false, Ordering::Release);
            return Err(CheckStartError::Spawn(error.to_string()));
        }
        Ok(())
    }

    pub fn take_result(&self) -> Option<Result<NativeUpdateResult, String>> {
        let result = self
            .completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if result.is_some() {
            self.pending.store(false, Ordering::Release);
        }
        result
    }
}

pub enum CheckStartError {
    Busy,
    Spawn(String),
}

pub fn started_notice() -> LocalizedText {
    i18n(
        "launcher.native-update.checking",
        "Проверяется релиз Rust-приложения…",
    )
}

pub fn start_error_notice(error: CheckStartError) -> LocalizedText {
    match error {
        CheckStartError::Busy => i18n(
            "launcher.native-update.busy",
            "Проверка релиза Rust-приложения уже выполняется.",
        ),
        CheckStartError::Spawn(error) => i18n(
            "launcher.native-update.failed",
            "Не удалось проверить релиз Rust-приложения: {error}",
        )
        .arg("error", error),
    }
}

pub fn finished_notice(result: Result<NativeUpdateResult, String>) -> LocalizedText {
    match result {
        Ok(NativeUpdateResult::NotPublished) => i18n(
            "launcher.native-update.not-published",
            "Нативный релиз Rust-приложения пока не опубликован. Установить обновление нельзя.",
        ),
        Ok(NativeUpdateResult::UnsupportedPlatform) => i18n(
            "launcher.native-update.unsupported-platform",
            "Для этой платформы проверяемые нативные обновления пока не поддерживаются.",
        ),
        Ok(NativeUpdateResult::NoCompatibleArtifact { tag }) => i18n(
            "launcher.native-update.missing-asset",
            "Релиз Rust-приложения {tag} найден, но в нём нет файла с SHA-256 издателя для этой платформы. Установка недоступна.",
        )
        .arg("tag", tag),
        Ok(NativeUpdateResult::UncomparableVersion { tag }) => i18n(
            "launcher.native-update.unknown-version",
            "Релиз Rust-приложения {tag} найден, но его версию нельзя безопасно сравнить с установленной.",
        )
        .arg("tag", tag),
        Ok(NativeUpdateResult::Current { tag }) => i18n(
            "launcher.native-update.current",
            "Установленная версия Rust-приложения актуальна относительно релиза {tag}.",
        )
        .arg("tag", tag),
        Ok(NativeUpdateResult::OlderPublished { tag }) => i18n(
            "launcher.native-update.older-release",
            "Опубликованный релиз Rust-приложения {tag} старее установленной версии.",
        )
        .arg("tag", tag),
        Ok(NativeUpdateResult::Available { tag, asset_name }) => i18n(
            "launcher.native-update.available",
            "Релиз {tag} содержит файл {asset} с SHA-256 издателя. Файл не скачан и не проверен; автоматическая установка пока не подключена.",
        )
        .arg("asset", asset_name)
        .arg("tag", tag),
        Err(error) => i18n(
            "launcher.native-update.failed",
            "Не удалось проверить релиз Rust-приложения: {error}",
        )
        .arg("error", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_notice_does_not_confuse_published_digest_with_verified_download() {
        let available = finished_notice(Ok(NativeUpdateResult::Available {
            tag: "v0.2.0".into(),
            asset_name: "neuromita-gui-windows-x86_64.exe".into(),
        }));
        assert!(available.source().contains("не скачан и не проверен"));
        assert!(
            finished_notice(Ok(NativeUpdateResult::NotPublished))
                .source()
                .contains("не опубликован")
        );
    }
}
