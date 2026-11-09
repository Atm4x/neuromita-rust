//! Read-only projection of Core's speech inventory into the AI Hub view.

use neuromita::{
    ai_runtime::{
        native_model::NativePhase, native_trust::NativeTrustVerdict,
        native_workflow::NativeWorkflowSnapshot, runtime_status::RuntimeSnapshot,
        speech_catalog::SpeechInventory,
    },
    presentation::{
        ai_hub_catalog::{SpeechHubCatalog, SpeechHubEntry, SpeechHubLoadedModels},
        native_hub::NativeHubPresenter,
    },
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;

use super::{
    runtime::Command,
    view::{AppWindow, HubInstallOperation, HubInstallPreview, HubModel, NativeRevisionRow},
};

pub fn bind(window: &AppWindow, send: Rc<dyn Fn(Command)>) {
    window.set_native_download_configured(super::runtime::native_download_configured());
    let native_install = send.clone();
    window.on_native_download(move |model_id, digest, backend| {
        native_install(Command::InstallNativePackage {
            model_id: model_id.into(),
            manifest_sha256: digest.into(),
            backend: backend.into(),
        });
    });
    let native_cancel = send.clone();
    window.on_native_cancel_download(move || native_cancel(Command::CancelNativePackageInstall));
    let refresh = send.clone();
    window.on_native_refresh(move || refresh(Command::RefreshNativeModels));
    let select = send.clone();
    window.on_native_select(move |model_id, digest, backend| {
        select(Command::SelectNativeRevision {
            model_id: model_id.into(),
            manifest_sha256: digest.into(),
            backend: backend.into(),
        })
    });
    let activate = send.clone();
    window.on_native_activate(move |model_id| {
        activate(Command::ActivateNativeModel {
            model_id: model_id.into(),
        })
    });
    let stop = send.clone();
    window.on_native_stop(move || stop(Command::StopNativeModel));
    let weak = window.as_weak();
    window.on_ai_hub_find_selection_index(move |identity| {
        weak.upgrade().map_or(-1, |window| {
            let entries = window.get_ai_hub_entries();
            let mut matches = entries.iter().enumerate().filter(|(_, entry)| {
                entry.category == identity.category && entry.id == identity.id
            });
            match (matches.next(), matches.next()) {
                (Some((index, _)), None) => i32::try_from(index).unwrap_or(-1),
                _ => -1,
            }
        })
    });
    let weak = window.as_weak();
    let send_preview = send.clone();
    window.on_ai_hub_preview_install(move |identity, token| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let entries = window.get_ai_hub_entries();
        let matches = entries
            .iter()
            .filter(|entry| entry.category == identity.category && entry.id == identity.id)
            .count();
        if matches == 1
            && (identity.category.as_str() == "tts" || identity.category.as_str() == "asr")
        {
            send_preview(Command::PreviewAiHubInstall {
                category: identity.category.into(),
                id: identity.id.into(),
                token,
            });
        } else {
            window.set_ai_hub_install_preview(HubInstallPreview {
                category: identity.category,
                id: identity.id,
                token,
                summary: "".into(),
                error: "Модель отсутствует или неоднозначна".into(),
                stage: "failed".into(),
                can_install: false,
                operation_id: "".into(),
            });
        }
    });
    let weak = window.as_weak();
    let send_install = send.clone();
    window.on_ai_hub_confirm_install(move |identity, token| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let preview = window.get_ai_hub_install_preview();
        let count = window
            .get_ai_hub_entries()
            .iter()
            .filter(|entry| entry.category == identity.category && entry.id == identity.id)
            .count();
        if count == 1
            && preview.can_install
            && (preview.stage == "review" || preview.stage == "wheel-review")
            && preview.token == token
            && preview.category == identity.category
            && preview.id == identity.id
        {
            send_install(Command::ConfirmAiHubInstall {
                category: identity.category.into(),
                id: identity.id.into(),
                token,
            });
        }
    });
    let weak = window.as_weak();
    window.on_ai_hub_refresh(move || {
        if let Some(window) = weak.upgrade()
            && window.get_ai_hub_can_refresh()
        {
            send(Command::RefreshSpeechInventory);
        }
    });
}

pub fn inventory(window: &AppWindow, inventory: &SpeechInventory) {
    window.set_ai_hub_install_preview(HubInstallPreview::default());
    let catalog = SpeechHubCatalog::from_inventory(inventory);
    window.set_ai_hub_entries(ModelRc::new(VecModel::from(
        catalog.entries.into_iter().map(model).collect::<Vec<_>>(),
    )));
    window.set_ai_hub_category_counts(ModelRc::new(VecModel::from(
        catalog.category_counts.to_vec(),
    )));
    window.set_ai_hub_discovery_warning_count(
        i32::try_from(catalog.discovery_error_count).unwrap_or(i32::MAX),
    );
    window.set_ai_hub_can_refresh(true);
}

pub fn runtime_status(window: &AppWindow, snapshot: &RuntimeSnapshot) {
    let loaded = SpeechHubLoadedModels::from_runtime(snapshot);
    window.set_ai_hub_loaded_tts_id(loaded.tts.unwrap_or_default().into());
    window.set_ai_hub_loaded_asr_id(loaded.asr.unwrap_or_default().into());
}

pub fn stopped(window: &AppWindow) {
    let current = window.get_ai_hub_install_operation();
    if current.stage == "installing" {
        window.set_ai_hub_install_operation(HubInstallOperation {
            stage: "unknown".into(),
            summary: "Core остановлен: результат публикации overlay не подтверждён. Проверьте установку после повторного запуска.".into(),
            ..current
        });
    }
    window.set_ai_hub_install_preview(HubInstallPreview::default());
    window.set_ai_hub_can_refresh(false);
    window.set_ai_hub_loaded_tts_id("".into());
    window.set_ai_hub_loaded_asr_id("".into());
}

fn model(entry: SpeechHubEntry) -> HubModel {
    HubModel {
        id: entry.id.into(),
        category: entry.category.as_str().into(),
        title: entry.title.into(),
        description: entry.description.into(),
        backend: "Не определена".into(),
        language: "".into(),
        size: "".into(),
        state: entry.state.as_str().into(),
        location: "".into(),
        setting_one: "".into(),
        setting_one_value: "".into(),
        setting_two: "".into(),
        setting_two_value: "".into(),
    }
}

pub fn install_preview(
    window: &AppWindow,
    category: String,
    id: String,
    token: i32,
    summary: String,
    error: String,
    stage: String,
    can_install: bool,
    operation_id: String,
) {
    window.set_ai_hub_install_preview(HubInstallPreview {
        category: category.into(),
        id: id.into(),
        token,
        summary: summary.into(),
        error: error.into(),
        stage: stage.into(),
        can_install,
        operation_id: operation_id.into(),
    });
}

pub fn install_operation(
    window: &AppWindow,
    operation_id: String,
    category: String,
    id: String,
    stage: String,
    summary: String,
) {
    window.set_ai_hub_install_operation(HubInstallOperation {
        operation_id: operation_id.into(),
        category: category.into(),
        id: id.into(),
        stage: stage.into(),
        summary: summary.into(),
    });
}

/// Read-only inventory; an explicit choice is not model readiness.
pub fn native_models(window: &AppWindow, snapshot: NativeWorkflowSnapshot) {
    let view = NativeHubPresenter::project(snapshot);
    let rows = view
        .revisions
        .into_iter()
        .map(|item| {
            let observed_phase = match item.observed_phase {
                Some(NativePhase::Ready) => "Ready",
                Some(NativePhase::Warm) => "Warm",
                Some(NativePhase::Loaded) => "Loaded",
                Some(NativePhase::Started) => "Started",
                Some(NativePhase::Failed) => "Failed",
                Some(NativePhase::Stopped) => "Stopped",
                None => "Не наблюдался",
            };
            NativeRevisionRow {
                trusted: matches!(item.trust, NativeTrustVerdict::Authorized { .. }),
                trust_status: match &item.trust {
                    NativeTrustVerdict::Authorized { publisher_id } => {
                        format!("Подпись издателя {publisher_id} подтверждена")
                    }
                    NativeTrustVerdict::Missing => "Нет подтверждённой подписи издателя".into(),
                    NativeTrustVerdict::Rejected => "Подпись или политика доверия отклонена".into(),
                }
                .into(),
                model_id: item.model_id.into(),
                display_name: item.display_name.into(),
                package_version: item.package_version.into(),
                manifest_sha256: item.manifest_sha256.into(),
                backend: item.backend.as_str().into(),
                selected: item.selected,
                running: item.active,
                observed_phase: observed_phase.into(),
            }
        })
        .collect::<Vec<_>>();
    window.set_native_revisions(ModelRc::new(VecModel::from(rows)));
    window.set_native_has_active(view.has_active_worker);
    window.set_native_status(if view.has_active_worker {
        "Запущенный EXE сохраняет закреплённую ревизию до явной остановки".into()
    } else if view.invalid_selection {
        "Выбранная ревизия отсутствует или backend более не объявлен".into()
    } else {
        "Для запуска необходимы независимый доверенный ключ и подписанный approval; manifest не доказывает готовность".into()
    });
    window.set_native_error("".into());
}

pub fn native_error(window: &AppWindow, error: String) {
    window.set_native_error(error.into());
}

/// Transport progress is not verified installation or a live worker status.
pub fn native_download_status(window: &AppWindow, summary: String, percent: i32, active: bool) {
    window.set_native_download_summary(summary.into());
    window.set_native_download_progress(percent);
    window.set_native_download_active(active);
}
