//! Read-only native application update check, independent of AI Core lifecycle.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use slint::ComponentHandle;
use tokio_util::sync::CancellationToken;

use neuromita::{
    localization::{LocalizedText, i18n},
    portable::PortableDataRoot,
    updater::{
        check::{
            NativeDownloadResult, NativeUpdateResult, check_native_update, download_native_update,
        },
        install::{NativeInstallOutcome, prepare_latest_native_update},
    },
};

use super::view::AppWindow;

pub struct NativeUpdateChecker {
    window: slint::Weak<AppWindow>,
    pending: Arc<AtomicBool>,
    completed: Arc<Mutex<Option<NativeUpdateCompletion>>>,
    cancellation: CancellationToken,
}

impl NativeUpdateChecker {
    pub fn new(window: &AppWindow) -> Self {
        Self {
            window: window.as_weak(),
            pending: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(Mutex::new(None)),
            cancellation: CancellationToken::new(),
        }
    }

    pub fn check(&self) -> Result<(), CheckStartError> {
        self.start(NativeUpdateTask::Check)
    }

    pub fn download(&self, data_directory: PathBuf) -> Result<(), CheckStartError> {
        self.start(NativeUpdateTask::Download(data_directory))
    }

    pub fn install(&self, data_directory: PathBuf) -> Result<(), CheckStartError> {
        self.start(NativeUpdateTask::Install(data_directory))
    }

    fn start(&self, task: NativeUpdateTask) -> Result<(), CheckStartError> {
        if self.pending.swap(true, Ordering::AcqRel) {
            return Err(CheckStartError::Busy);
        }
        let window = self.window.clone();
        let completed = Arc::clone(&self.completed);
        let cancel = self.cancellation.child_token();
        let spawn = thread::Builder::new()
            .name("neuromita-native-release-check".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| {
                        runtime.block_on(async {
                            match task {
                                NativeUpdateTask::Check => {
                                    match tokio::time::timeout(
                                        Duration::from_secs(18),
                                        check_native_update(),
                                    )
                                    .await
                                    {
                                        Ok(result) => Ok(NativeUpdateCompletion::Checked(
                                            result.map_err(|error| error.to_string()),
                                        )),
                                        Err(_) => Err("Release check timed out".to_owned()),
                                    }
                                }
                                NativeUpdateTask::Download(directory) => {
                                    let root = PortableDataRoot::resolve(Some(&directory))
                                        .map_err(|error| error.to_string())?;
                                    match tokio::time::timeout(
                                        Duration::from_secs(180),
                                        download_native_update(&root, &cancel),
                                    )
                                    .await
                                    {
                                        Ok(result) => Ok(NativeUpdateCompletion::Downloaded(
                                            result.map_err(|error| error.to_string()),
                                        )),
                                        Err(_) => Err("Download timed out".to_owned()),
                                    }
                                }
                                NativeUpdateTask::Install(directory) => {
                                    let root = PortableDataRoot::resolve(Some(&directory))
                                        .map_err(|error| error.to_string())?;
                                    match tokio::time::timeout(
                                        Duration::from_secs(180),
                                        prepare_latest_native_update(&root, &cancel),
                                    )
                                    .await
                                    {
                                        Ok(result) => Ok(NativeUpdateCompletion::InstallPrepared(
                                            result.map_err(|error| error.to_string()),
                                        )),
                                        Err(_) => {
                                            Err("Installation preparation timed out".to_owned())
                                        }
                                    }
                                }
                            }
                        })
                    });
                let completion = match result {
                    Ok(completion) => completion,
                    Err(error) => NativeUpdateCompletion::Failed(error),
                };
                *completed.lock().unwrap_or_else(|error| error.into_inner()) = Some(completion);
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

    pub fn take_result(&self) -> Option<NativeUpdateCompletion> {
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

impl Drop for NativeUpdateChecker {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

enum NativeUpdateTask {
    Check,
    Download(PathBuf),
    Install(PathBuf),
}

pub enum NativeUpdateCompletion {
    Checked(Result<NativeUpdateResult, String>),
    Downloaded(Result<NativeDownloadResult, String>),
    InstallPrepared(Result<NativeInstallOutcome, String>),
    Failed(String),
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
            "Операция с обновлением Rust-приложения уже выполняется.",
        ),
        CheckStartError::Spawn(error) => i18n(
            "launcher.native-update.failed",
            "Не удалось проверить релиз Rust-приложения: {error}",
        )
        .arg("error", error),
    }
}

pub fn download_started_notice() -> LocalizedText {
    i18n(
        "launcher.native-update.downloading",
        "Получение метаданных и проверенное скачивание Rust-приложения… Установка не выполняется.",
    )
}

pub fn completion_notice(completion: NativeUpdateCompletion) -> LocalizedText {
    match completion {
        NativeUpdateCompletion::Checked(result) => finished_notice(result),
        NativeUpdateCompletion::Downloaded(result) => download_finished_notice(result),
        NativeUpdateCompletion::InstallPrepared(Ok(NativeInstallOutcome::NotEligible(reason))) => {
            finished_notice(Ok(reason))
        }
        NativeUpdateCompletion::InstallPrepared(Ok(NativeInstallOutcome::Prepared { .. })) => i18n(
            "launcher.native-update.prepared",
            "Обновление подготовлено к применению после закрытия приложения.",
        ),
        NativeUpdateCompletion::InstallPrepared(Err(error)) => install_error_notice(error),
        NativeUpdateCompletion::Failed(error) => finished_notice(Err(error)),
    }
}

pub fn install_started_notice() -> LocalizedText {
    i18n(
        "launcher.native-update.preparing",
        "Проверяется пакет и готовится установка нативного обновления с перезапуском…",
    )
}

pub fn install_error_notice(error: impl std::fmt::Display) -> LocalizedText {
    i18n(
        "launcher.native-update.install-failed",
        "Применение обновления не начато: {error}",
    )
    .arg("error", error.to_string())
}

pub fn handoff_notice() -> LocalizedText {
    i18n(
        "launcher.native-update.handoff",
        "Обновление проверено. Приложение закрывается; отдельный updater применит его после завершения процесса и попробует перезапустить GUI.",
    )
}

pub fn handoff_cancelled_notice(shutdown: impl std::fmt::Display) -> LocalizedText {
    i18n(
        "launcher.native-update.shutdown-cancelled",
        "Не удалось закрыть приложение ({shutdown}). Обновление безопасно отменено; текущая версия продолжает работу.",
    )
    .arg("shutdown", shutdown.to_string())
}

pub fn handoff_pending_notice(
    shutdown: impl std::fmt::Display,
    cancellation: impl std::fmt::Display,
) -> LocalizedText {
    i18n(
        "launcher.native-update.shutdown-pending",
        "Не удалось закрыть приложение ({shutdown}) и завершить отмену обновления ({cancellation}). Состояние установки требует проверки; не запускайте повторную установку до восстановления.",
    )
    .arg("shutdown", shutdown.to_string())
    .arg("cancellation", cancellation.to_string())
}

pub fn download_finished_notice(result: Result<NativeDownloadResult, String>) -> LocalizedText {
    match result {
        Ok(NativeDownloadResult::NotEligible(reason)) => finished_notice(Ok(reason)),
        Ok(NativeDownloadResult::Verified {
            tag,
            asset_name,
            sha256,
            size,
            reused,
        }) => {
            let notice = if reused {
                i18n(
                    "launcher.native-update.verified-reused",
                    "Релиз {tag}: файл {asset} повторно проверен (SHA-256: {hash}, {bytes} байт). Установка не выполнена.",
                )
            } else {
                i18n(
                    "launcher.native-update.verified-new",
                    "Релиз {tag}: файл {asset} скачан и проверен (SHA-256: {hash}, {bytes} байт). Установка не выполнена.",
                )
            };
            notice
                .arg("tag", tag)
                .arg("asset", asset_name)
                .arg("hash", sha256)
                .arg("bytes", size.to_string())
        }
        Err(error) => i18n(
            "launcher.native-update.download-failed",
            "Не удалось скачать и проверить обновление: {error}. Установка не выполнялась.",
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
