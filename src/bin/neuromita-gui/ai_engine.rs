use super::{
    runtime::Command,
    view::{
        AiEngineUi, AiHardwareDevice, AiHubCatalogState, AiHubComponent, AiHubLoadPhase,
        AiHubTruth, AppWindow,
    },
};
use neuromita::{
    ai_runtime::{
        AiService,
        runtime_status::{RuntimePhase, RuntimeSnapshot, ServiceRuntimeStatus},
        speech_catalog::{SpeechInventory, SpeechModelOption},
    },
    presentation::hardware::HardwareView,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;

pub fn bind(window: &AppWindow, send: Rc<dyn Fn(Command)>) {
    // The shell is available; only read-only Core speech inventory is connected.
    // Installation, schema persistence and model readiness remain unavailable.
    window.global::<AiEngineUi>().set_hub_available(true);
    let hub_window = window.as_weak();
    window.global::<AiEngineUi>().on_open_hub(move || {
        if let Some(window) = hub_window.upgrade() {
            window.set_ai_hub_session_epoch(window.get_ai_hub_session_epoch().wrapping_add(1));
            window.set_ai_hub_open(true);
        }
    });
    let refresh_window = window.as_weak();
    let refresh_send = send.clone();
    window.on_ai_hub_refresh_requested(move |_, _| {
        let Some(window) = refresh_window.upgrade() else {
            return;
        };
        if !window.get_core_connected() {
            return;
        }
        // No optimistic spinner: the dispatch channel has no success acknowledgement.
        // A new Core inventory snapshot is the only confirmation of Refresh.
        refresh_send(Command::RefreshSpeechInventory);
    });
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

// The only production AI Hub data source here is Core-owned SpeechInventory.
// Overlay detection is NOT a worker or inference readiness proof, and a
// partially installed environment must not be labelled Installed.
fn installation_truth(model: &SpeechModelOption) -> AiHubTruth {
    match (model.package_detected, model.verified_assets) {
        (true, true) => AiHubTruth::Yes,
        (false, false) => AiHubTruth::No,
        _ => AiHubTruth::Unknown,
    }
}

fn project_models(
    category: &str,
    models: &[SpeechModelOption],
    revision: i32,
    token: &SharedString,
) -> Vec<AiHubComponent> {
    models
        .iter()
        .map(|model| AiHubComponent {
            component_id: model.id.clone().into(),
            category: category.into(),
            title: model.name.clone().into(),
            description: "".into(),
            tags: ModelRc::new(VecModel::from(
                model
                    .installed_revisions
                    .iter()
                    .cloned()
                    .map(Into::into)
                    .collect::<Vec<SharedString>>(),
            )),
            backend: "".into(),
            compatibility: model.installation_label().into(),
            // Inventory has no component-level action compatibility result.
            supported: false,
            unsupported_reason: "Installation actions require a verified Core AI Hub adapter"
                .into(),
            discovered: AiHubTruth::Yes,
            installed: installation_truth(model),
            environment_ready: AiHubTruth::Unknown,
            worker_ready: AiHubTruth::Unknown,
            model_ready: AiHubTruth::Unknown,
            phase: AiHubLoadPhase::Idle,
            error: "".into(),
            revision,
            token: token.clone(),
            can_install: false,
            can_uninstall: false,
            can_open_settings: false,
        })
        .collect()
}

pub fn inventory(window: &AppWindow, inventory: &SpeechInventory) {
    let revision = window.get_ai_hub_catalog().revision.saturating_add(1);
    let token: SharedString = format!("speech-inventory:{revision}").into();
    let mut components = project_models("tts", &inventory.tts, revision, &token);
    components.extend(project_models("asr", &inventory.asr, revision, &token));
    window.set_ai_hub_components(ModelRc::new(VecModel::from(components)));
    window.set_ai_hub_catalog(AiHubCatalogState {
        available: true,
        phase: AiHubLoadPhase::Loaded,
        error: inventory.errors.join("\n").into(),
        revision,
        token,
    });
}

// Component-level readiness must be correlated with the exact Core-issued
// active plugin, never inferred from any other model's worker lifecycle.
fn status_gates<'a>(
    status: Option<&'a ServiceRuntimeStatus>,
    component_id: &str,
) -> (AiHubTruth, AiHubTruth, AiHubLoadPhase, Option<&'a str>) {
    let Some(status) = status else {
        return (
            AiHubTruth::Unknown,
            AiHubTruth::Unknown,
            AiHubLoadPhase::Idle,
            None,
        );
    };
    let worker = match status.phase {
        RuntimePhase::WorkerRunning | RuntimePhase::ModelReady if status.generation.is_some() => {
            AiHubTruth::Yes
        }
        RuntimePhase::Stopped | RuntimePhase::Failed => AiHubTruth::No,
        _ => AiHubTruth::Unknown,
    };
    let model = match status.phase {
        RuntimePhase::ModelReady
            if status.generation.is_some()
                && status
                    .active_model
                    .as_ref()
                    .is_some_and(|model| model.plugin_id == component_id) =>
        {
            AiHubTruth::Yes
        }
        RuntimePhase::Stopped | RuntimePhase::Failed => AiHubTruth::No,
        _ => AiHubTruth::Unknown,
    };
    let phase = match status.phase {
        RuntimePhase::Starting => AiHubLoadPhase::Checking,
        RuntimePhase::Failed => AiHubLoadPhase::Failed,
        _ => AiHubLoadPhase::Loaded,
    };
    // A service-wide failure is not necessarily an individual model failure.
    // Only label the exact pending or active model with its backend error.
    let associated = status.pending_model.as_deref() == Some(component_id)
        || status
            .active_model
            .as_ref()
            .is_some_and(|model| model.plugin_id == component_id);
    let error = if associated {
        status.last_error.as_deref()
    } else {
        None
    };
    (worker, model, phase, error)
}

pub fn runtime_status(window: &AppWindow, snapshot: &RuntimeSnapshot) {
    if !window.get_ai_hub_catalog().available {
        // Do not materialize catalog rows from worker/status data alone.
        return;
    }
    let rows = window.get_ai_hub_components();
    let updated = rows
        .iter()
        .map(|mut row| {
            let service = match row.category.as_str() {
                "tts" => Some(AiService::Tts),
                "asr" => Some(AiService::Asr),
                _ => None,
            };
            if let Some(service) = service {
                let (worker, model, phase, error) =
                    status_gates(snapshot.services.get(&service), row.component_id.as_str());
                row.worker_ready = worker;
                row.model_ready = model;
                row.phase = phase;
                row.error = error.unwrap_or_default().into();
            }
            row
        })
        .collect::<Vec<_>>();
    window.set_ai_hub_components(ModelRc::new(VecModel::from(updated)));
}

pub fn disconnected(window: &AppWindow) {
    let revision = window.get_ai_hub_catalog().revision.saturating_add(1);
    window.set_ai_hub_components(ModelRc::new(VecModel::<AiHubComponent>::default()));
    window.set_ai_hub_catalog(AiHubCatalogState {
        available: false,
        phase: AiHubLoadPhase::Unavailable,
        error: "".into(),
        revision,
        token: "".into(),
    });
}

#[cfg(test)]
mod hub_inventory_tests {
    use super::*;
    #[test]
    fn partial_receipts_never_claim_ready() {
        let models = [
            SpeechModelOption {
                id: "partial".into(),
                name: "Partial".into(),
                package_detected: true,
                verified_assets: false,
                installed_revisions: vec![],
            },
            SpeechModelOption {
                id: "verified".into(),
                name: "Verified".into(),
                package_detected: true,
                verified_assets: true,
                installed_revisions: vec!["rev1".into()],
            },
        ];
        let rows = project_models("tts", &models, 7, &"epoch7".into());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].installed, AiHubTruth::Unknown);
        assert_eq!(rows[1].installed, AiHubTruth::Yes);
        for row in rows {
            assert_eq!(row.discovered, AiHubTruth::Yes);
            assert_eq!(row.environment_ready, AiHubTruth::Unknown);
            assert_eq!(row.worker_ready, AiHubTruth::Unknown);
            assert_eq!(row.model_ready, AiHubTruth::Unknown);
            assert!(!row.can_install && !row.can_uninstall && !row.can_open_settings);
            assert_eq!(row.revision, 7);
        }
    }

    #[test]
    fn explicit_generation_and_matching_plugin_prove_model_ready() {
        let generation = uuid::Uuid::new_v4();
        let status = ServiceRuntimeStatus {
            phase: RuntimePhase::ModelReady,
            generation: Some(generation),
            active_model: Some(neuromita::ai_runtime::runtime_status::LoadedModel {
                plugin_id: "tts-one".into(),
                asset_revision: Some("rev1".into()),
            }),
            pending_model: None,
            last_error: None,
        };
        let (worker, model, _, _) = status_gates(Some(&status), "tts-one");
        assert_eq!(worker, AiHubTruth::Yes);
        assert_eq!(model, AiHubTruth::Yes);
        let (_, other, _, _) = status_gates(Some(&status), "tts-two");
        assert_eq!(other, AiHubTruth::Unknown);
        let mut stale = status.clone();
        stale.generation = None;
        let (worker, model, _, _) = status_gates(Some(&stale), "tts-one");
        assert_eq!(worker, AiHubTruth::Unknown);
        assert_eq!(model, AiHubTruth::Unknown);
    }

    #[test]
    fn absent_status_and_failed_worker_do_not_claim_ready() {
        let (worker, model, _, _) = status_gates(None, "asr-a");
        assert_eq!(worker, AiHubTruth::Unknown);
        assert_eq!(model, AiHubTruth::Unknown);
        let failed = ServiceRuntimeStatus {
            phase: RuntimePhase::Failed,
            generation: None,
            active_model: None,
            pending_model: Some("asr-a".into()),
            last_error: Some("worker unavailable".into()),
        };
        let (worker, model, phase, error) = status_gates(Some(&failed), "asr-a");
        assert_eq!(worker, AiHubTruth::No);
        assert_eq!(model, AiHubTruth::No);
        assert_eq!(phase, AiHubLoadPhase::Failed);
        assert_eq!(error, Some("worker unavailable"));
        let (_, _, _, other_error) = status_gates(Some(&failed), "asr-b");
        assert_eq!(other_error, None);
    }

    #[test]
    fn empty_inventory_does_not_create_fictitious_components() {
        let rows = project_models("asr", &[], 1, &"empty".into());
        assert!(rows.is_empty());
    }
}
