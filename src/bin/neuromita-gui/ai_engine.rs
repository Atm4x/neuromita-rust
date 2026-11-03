use neuromita::presentation::{AiEngineSettingsPresenter, AiHardwareSnapshot};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::view::{AiEngineUi, AiHardwareDevice, AppWindow};

pub fn bind(window: &AppWindow) {
    let presenter = AiEngineSettingsPresenter::new();
    let snapshot = presenter.snapshot();
    let ui = window.global::<AiEngineUi>();
    ui.set_process_mode(snapshot.process_mode.into());
    ui.set_process_status(snapshot.process_status.into());
    ui.set_mode_locked(!snapshot.process_control_available);
    ui.set_maintenance_enabled(snapshot.maintenance_available);
    ui.set_hub_available(snapshot.hub_available);
    ui.set_maintenance_status(snapshot.maintenance_status.into());
    ui.set_environment_path(snapshot.environment_path.unwrap_or_default().into());
    let weak = window.as_weak();
    ui.on_refresh_hardware(move || detect(weak.clone()));
    detect(window.as_weak());
}

fn detect(window: slint::Weak<AppWindow>) {
    if let Some(component) = window.upgrade() {
        component.global::<AiEngineUi>().set_detecting(true);
    }
    std::thread::spawn(move || {
        let snapshot = AiEngineSettingsPresenter::new().hardware();
        let _ = window.upgrade_in_event_loop(move |component| render(&component, snapshot));
    });
}

fn render(window: &AppWindow, hardware: AiHardwareSnapshot) {
    let ui = window.global::<AiEngineUi>();
    let devices = hardware
        .devices
        .into_iter()
        .map(|device| AiHardwareDevice {
            name: device.name.into(),
            vendor: device.vendor.into(),
            cuda: device.cuda.into(),
            directml: device.directml.into(),
            capability: device.capability.into(),
            vram: device.vram.into(),
        })
        .collect::<Vec<_>>();
    ui.set_devices(ModelRc::new(VecModel::from(devices)));
    ui.set_hardware_error(hardware.error.unwrap_or_default().into());
    ui.set_detecting(false);
}
