//! Read-only projection of Core's speech inventory into the AI Hub view.

use neuromita::ai_runtime::speech_catalog::{SpeechInventory, SpeechModelOption};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;

use super::{
    runtime::Command,
    view::{AppWindow, HubModel},
};

pub fn bind(window: &AppWindow, send: Rc<dyn Fn(Command)>) {
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
    let (entries, counts) = project(inventory);
    window.set_ai_hub_entries(ModelRc::new(VecModel::from(entries)));
    window.set_ai_hub_category_counts(ModelRc::new(VecModel::from(counts.to_vec())));
    window.set_ai_hub_can_refresh(true);
}

pub fn stopped(window: &AppWindow) {
    window.set_ai_hub_can_refresh(false);
}

fn project(inventory: &SpeechInventory) -> (Vec<HubModel>, [i32; 5]) {
    let mut entries = Vec::with_capacity(inventory.tts.len() + inventory.asr.len());
    entries.extend(inventory.tts.iter().map(|option| model("tts", option)));
    entries.extend(inventory.asr.iter().map(|option| model("asr", option)));
    let counts = [
        i32::try_from(inventory.tts.len()).unwrap_or(i32::MAX),
        i32::try_from(inventory.asr.len()).unwrap_or(i32::MAX),
        0,
        0,
        0,
    ];
    (entries, counts)
}

fn model(category: &str, option: &SpeechModelOption) -> HubModel {
    let state = match (option.package_detected, option.verified_assets) {
        (true, true) => "package-assets",
        (true, false) => "package-only",
        (false, true) => "assets-only",
        (false, false) => "discovered",
    };
    HubModel {
        id: option.id.clone().into(),
        category: category.into(),
        title: option.name.clone().into(),
        description: option.installation_label().into(),
        backend: "Не определена".into(),
        language: "".into(),
        size: "".into(),
        state: state.into(),
        location: "".into(),
        setting_one: "".into(),
        setting_one_value: "".into(),
        setting_two: "".into(),
        setting_two_value: "".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: &str, package_detected: bool, verified_assets: bool) -> SpeechModelOption {
        SpeechModelOption {
            id: id.to_owned(),
            name: format!("{id} engine"),
            package_detected,
            verified_assets,
            installed_revisions: if verified_assets {
                vec!["rev-1".into()]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn counts_are_from_same_snapshot_as_rows_not_readiness() {
        let inventory = SpeechInventory {
            tts: vec![option("piper", false, false), option("xtts", true, true)],
            asr: vec![option("piper", true, false)],
            errors: vec!["invalid community plugin".into()],
        };
        let (rows, counts) = project(&inventory);
        assert_eq!(counts, [2, 1, 0, 0, 0]);
        assert_eq!(
            rows.iter()
                .map(|item| item.category.as_str())
                .collect::<Vec<_>>(),
            ["tts", "tts", "asr"]
        );
        assert_eq!(rows[0].state.as_str(), "discovered");
        assert_eq!(rows[1].state.as_str(), "package-assets");
        assert_eq!(rows[2].state.as_str(), "package-only");
        assert_ne!(rows[1].state.as_str(), "ready");
        assert_eq!(rows[0].backend.as_str(), "Не определена");
        assert!(rows.iter().all(|row| row.setting_one.is_empty()
            && row.setting_two.is_empty()
            && row.location.is_empty()));
    }

    #[test]
    fn fresh_snapshot_replaces_previous_rows_and_counts() {
        let (before, before_counts) = project(&SpeechInventory {
            tts: vec![option("one", false, false), option("two", false, false)],
            ..SpeechInventory::default()
        });
        let (after, after_counts) = project(&SpeechInventory {
            asr: vec![option("asr", false, true)],
            ..SpeechInventory::default()
        });
        assert_eq!(before.len(), 2);
        assert_eq!(before_counts, [2, 0, 0, 0, 0]);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].state.as_str(), "assets-only");
        assert_eq!(after_counts, [0, 1, 0, 0, 0]);
    }
}
