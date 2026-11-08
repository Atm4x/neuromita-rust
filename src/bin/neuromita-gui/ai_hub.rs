//! Read-only projection of Core's speech inventory into the AI Hub view.

use neuromita::{
    ai_runtime::{runtime_status::RuntimeSnapshot, speech_catalog::SpeechInventory},
    presentation::ai_hub_catalog::{SpeechHubCatalog, SpeechHubEntry, SpeechHubLoadedModels},
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;

use super::{
    runtime::Command,
    view::{AppWindow, HubModel},
};

pub fn bind(window: &AppWindow, send: Rc<dyn Fn(Command)>) {
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
    window.on_ai_hub_refresh(move || {
        if let Some(window) = weak.upgrade()
            && window.get_ai_hub_can_refresh()
        {
            send(Command::RefreshSpeechInventory);
        }
    });
}

pub fn inventory(window: &AppWindow, inventory: &SpeechInventory) {
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
