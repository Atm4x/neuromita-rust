use super::{
    runtime::Command,
    view::{AiEngineUi, AiHardwareDevice, AppWindow},
};
use neuromita::{
    ai_runtime::{
        AiService,
        runtime_status::{RuntimePhase, RuntimeSnapshot, ServiceRuntimeStatus},
    },
    presentation::hardware::HardwareView,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;

pub fn bind(window: &AppWindow, send: Rc<dyn Fn(Command)>) {
    let weak = window.as_weak();
    window.global::<AiEngineUi>().on_refresh_hardware(move || {
        if let Some(window) = weak.upgrade() {
            window.global::<AiEngineUi>().set_detecting(true);
        }
        send(Command::RefreshHardware);
    });
}

pub fn render(window: &AppWindow, snapshot: HardwareView) {
    let ui = window.global::<AiEngineUi>();
    ui.set_devices(ModelRc::new(VecModel::from(
        snapshot
            .devices
            .into_iter()
            .map(|device| AiHardwareDevice {
                name: device.name.into(),
                vendor: device.vendor.into(),
                cuda: device.cuda.into(),
                directml: "".into(),
                capability: device.capability.into(),
                vram: device.memory.into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_hardware_error(snapshot.errors.join("\n").into());
    ui.set_detecting(false);
}

#[derive(Debug, PartialEq, Eq)]
struct WorkerLabel {
    description: String,
    failed: bool,
}

fn worker_label(status: Option<&ServiceRuntimeStatus>) -> WorkerLabel {
    match status {
        Some(status) => WorkerLabel {
            description: super::speech_status::describe(status),
            failed: status.phase == RuntimePhase::Failed,
        },
        None => WorkerLabel {
            description: "Не настроен".into(),
            failed: false,
        },
    }
}

pub fn runtime_status(window: &AppWindow, snapshot: &RuntimeSnapshot) {
    let ui = window.global::<AiEngineUi>();
    let embedding = worker_label(snapshot.services.get(&AiService::Embedding));
    let reranker = worker_label(snapshot.services.get(&AiService::Reranker));
    ui.set_embedding_status(embedding.description.into());
    ui.set_embedding_failed(embedding.failed);
    ui.set_reranker_status(reranker.description.into());
    ui.set_reranker_failed(reranker.failed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use neuromita::ai_runtime::runtime_status::LoadedModel;

    #[test]
    fn shows_original_failure_and_clears_it_only_after_new_model_ready_snapshot() {
        let absent = worker_label(None);
        assert_eq!(absent.description, "Не настроен");
        assert!(!absent.failed);
        let failed = worker_label(Some(&ServiceRuntimeStatus {
            phase: RuntimePhase::Failed,
            last_error: Some("Python worker timeout during infer".into()),
            ..Default::default()
        }));
        assert!(failed.failed);
        assert!(
            failed
                .description
                .contains("Python worker timeout during infer")
        );
        let ready = worker_label(Some(&ServiceRuntimeStatus {
            phase: RuntimePhase::ModelReady,
            active_model: Some(LoadedModel {
                plugin_id: "mock_embedding".into(),
                asset_revision: None,
            }),
            ..Default::default()
        }));
        assert!(!ready.failed);
        assert!(ready.description.contains("mock_embedding"));
        assert!(!ready.description.contains("timeout"));
    }
}
