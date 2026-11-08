use std::{sync::mpsc, thread};

use neuromita::{
    localization::{LocalizedText, i18n},
    updater::native::{NativeUpdateStatus, check_native_update},
};

pub struct NativeUpdates {
    checking: bool,
    sender: mpsc::Sender<Result<NativeUpdateStatus, String>>,
    receiver: mpsc::Receiver<Result<NativeUpdateStatus, String>>,
}

impl NativeUpdates {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            checking: false,
            sender,
            receiver,
        }
    }

    pub fn start(&mut self) -> LocalizedText {
        if self.checking {
            return i18n(
                "updater.native.busy",
                "Проверка обновлений Rust-приложения уже выполняется.",
            );
        }
        self.checking = true;
        let sender = self.sender.clone();
        match thread::Builder::new()
            .name("native-update-check".into())
            .spawn(move || {
                let outcome = check_on_worker();
                let _ = sender.send(outcome);
            }) {
            Ok(_) => i18n(
                "updater.native.checking",
                "Проверяем обновления Rust-приложения…",
            ),
            Err(error) => {
                self.checking = false;
                check_error(error.to_string())
            }
        }
    }

    pub fn poll(&mut self) -> Option<LocalizedText> {
        match self.receiver.try_recv() {
            Ok(outcome) => {
                self.checking = false;
                Some(match outcome {
                    Ok(NativeUpdateStatus::NoPublishedRelease { current }) => i18n(
                        "updater.native.no-release",
                        "Для Rust-приложения пока нет опубликованных стабильных релизов. Установлена версия {current}.",
                    )
                    .arg("current", current),
                    Ok(NativeUpdateStatus::Current { current, latest }) => i18n(
                        "updater.native.current",
                        "Версия Rust-приложения {current} не старее последнего опубликованного релиза {latest}.",
                    )
                    .arg("current", current)
                    .arg("latest", latest),
                    Ok(NativeUpdateStatus::Available {
                        current,
                        latest,
                        asset,
                    }) => i18n(
                        "updater.native.available",
                        "Доступна версия Rust-приложения {latest} (установлена {current}), проверяемый пакет {asset}. Установка пока не подключена.",
                    )
                    .arg("current", current)
                    .arg("latest", latest)
                    .arg("asset", asset.name()),
                    Ok(NativeUpdateStatus::MissingExecutableAsset {
                        current,
                        latest,
                        executable,
                    }) => i18n(
                        "updater.native.no-asset",
                        "Найден релиз Rust-приложения {latest} (установлена {current}), но отсутствует исполняемый файл {executable}.",
                    )
                    .arg("current", current)
                    .arg("latest", latest)
                    .arg("executable", executable),
                    Err(error) => check_error(error),
                })
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) if self.checking => {
                self.checking = false;
                Some(check_error(
                    "поток проверки завершился без результата".into(),
                ))
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }
}

fn check_on_worker() -> Result<NativeUpdateStatus, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "не удалось определить имя исполняемого файла".to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(check_native_update(env!("CARGO_PKG_VERSION"), name))
        .map_err(|error| error.to_string())
}

fn check_error(error: String) -> LocalizedText {
    i18n(
        "updater.native.error",
        "Не удалось проверить обновления Rust-приложения: {error}",
    )
    .arg("error", error)
}
