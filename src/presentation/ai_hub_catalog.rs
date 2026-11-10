//! Read-only speech catalog projection shared by GUI adapters and executable tests.

use std::collections::HashMap;

use crate::ai_runtime::{
    AiService,
    runtime_status::{RuntimePhase, RuntimeSnapshot},
    speech_catalog::{SpeechInventory, SpeechModelOption},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpeechHubCategory {
    Tts,
    Asr,
}

impl SpeechHubCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tts => "tts",
            Self::Asr => "asr",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechHubResourceState {
    Discovered,
    PackageOnly,
    AssetsOnly,
    PackageAndAssets,
}

impl SpeechHubResourceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::PackageOnly => "package-only",
            Self::AssetsOnly => "assets-only",
            Self::PackageAndAssets => "package-assets",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechHubEntry {
    pub id: String,
    pub category: SpeechHubCategory,
    pub title: String,
    pub description: &'static str,
    pub state: SpeechHubResourceState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeechHubLoadedModels {
    pub tts: Option<String>,
    pub asr: Option<String>,
}

impl SpeechHubLoadedModels {
    pub fn from_runtime(snapshot: &RuntimeSnapshot) -> Self {
        fn active_id(snapshot: &RuntimeSnapshot, service: AiService) -> Option<String> {
            let status = snapshot.service(service);
            if status.phase != RuntimePhase::ModelReady {
                return None;
            }
            status.active_model.map(|model| model.plugin_id)
        }
        Self {
            tts: active_id(snapshot, AiService::Tts),
            asr: active_id(snapshot, AiService::Asr),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechHubCatalog {
    pub entries: Vec<SpeechHubEntry>,
    pub category_counts: [i32; 5],
    pub discovery_error_count: usize,
}

impl SpeechHubCatalog {
    pub fn from_inventory(inventory: &SpeechInventory) -> Self {
        let mut entries = Vec::with_capacity(inventory.tts.len() + inventory.asr.len());
        let mut identity_occurrences = HashMap::new();
        for (category, options) in [
            (SpeechHubCategory::Tts, &inventory.tts),
            (SpeechHubCategory::Asr, &inventory.asr),
        ] {
            for option in options {
                *identity_occurrences
                    .entry((category, option.id.as_str()))
                    .or_insert(0usize) += 1;
            }
        }
        let mut rejected_duplicates = 0usize;
        for (category, options) in [
            (SpeechHubCategory::Tts, &inventory.tts),
            (SpeechHubCategory::Asr, &inventory.asr),
        ] {
            for option in options {
                if identity_occurrences[&(category, option.id.as_str())] == 1 {
                    entries.push(entry(category, option));
                } else {
                    rejected_duplicates += 1;
                }
            }
        }
        Self {
            category_counts: [
                i32::try_from(
                    entries
                        .iter()
                        .filter(|e| e.category == SpeechHubCategory::Tts)
                        .count(),
                )
                .unwrap_or(i32::MAX),
                i32::try_from(
                    entries
                        .iter()
                        .filter(|e| e.category == SpeechHubCategory::Asr)
                        .count(),
                )
                .unwrap_or(i32::MAX),
                0,
                0,
                0,
            ],
            entries,
            discovery_error_count: inventory.errors.len().saturating_add(rejected_duplicates),
        }
    }
}

fn entry(category: SpeechHubCategory, option: &SpeechModelOption) -> SpeechHubEntry {
    let state = match (option.package_detected, option.verified_assets) {
        (true, true) => SpeechHubResourceState::PackageAndAssets,
        (true, false) => SpeechHubResourceState::PackageOnly,
        (false, true) => SpeechHubResourceState::AssetsOnly,
        (false, false) => SpeechHubResourceState::Discovered,
    };
    SpeechHubEntry {
        id: option.id.clone(),
        category,
        title: option.name.clone(),
        description: option.installation_label(),
        state,
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
    fn duplicate_identity_is_not_ambiguous_for_details_or_category_counts() {
        let inventory = SpeechInventory {
            tts: vec![option("shared", false, false), option("shared", true, true)],
            asr: vec![option("shared", false, true), option("asr", false, false)],
            errors: vec!["corrupted plugin".into()],
        };
        let projected = SpeechHubCatalog::from_inventory(&inventory);
        assert_eq!(projected.category_counts, [0, 2, 0, 0, 0]);
        assert_eq!(projected.discovery_error_count, 3);
        assert_eq!(projected.entries.len(), 2);
        assert_eq!(projected.entries[0].category, SpeechHubCategory::Asr);
        assert_eq!(projected.entries[1].id, "asr");
    }

    #[test]
    fn loaded_model_requires_matching_service_and_confirmed_model_ready_phase() {
        use crate::ai_runtime::runtime_status::{LoadedModel, ServiceRuntimeStatus};

        let mut snapshot = RuntimeSnapshot::default();
        snapshot.services.insert(
            AiService::Tts,
            ServiceRuntimeStatus {
                phase: RuntimePhase::WorkerRunning,
                active_model: Some(LoadedModel {
                    plugin_id: "shared".into(),
                    asset_revision: None,
                }),
                ..Default::default()
            },
        );
        snapshot.services.insert(
            AiService::Asr,
            ServiceRuntimeStatus {
                phase: RuntimePhase::ModelReady,
                active_model: Some(LoadedModel {
                    plugin_id: "shared".into(),
                    asset_revision: Some("rev-2".into()),
                }),
                ..Default::default()
            },
        );
        let projected = SpeechHubLoadedModels::from_runtime(&snapshot);
        assert_eq!(projected.tts, None);
        assert_eq!(projected.asr.as_deref(), Some("shared"));

        snapshot.services.get_mut(&AiService::Tts).unwrap().phase = RuntimePhase::ModelReady;
        let projected = SpeechHubLoadedModels::from_runtime(&snapshot);
        assert_eq!(projected.tts.as_deref(), Some("shared"));
        assert_eq!(projected.asr.as_deref(), Some("shared"));

        snapshot.services.get_mut(&AiService::Tts).unwrap().phase = RuntimePhase::Failed;
        snapshot.services.get_mut(&AiService::Asr).unwrap().phase = RuntimePhase::Stopped;
        assert_eq!(
            SpeechHubLoadedModels::from_runtime(&snapshot),
            SpeechHubLoadedModels::default()
        );
    }

    #[test]
    fn model_ready_without_confirmed_active_identity_is_not_exposed() {
        use crate::ai_runtime::runtime_status::ServiceRuntimeStatus;
        let mut snapshot = RuntimeSnapshot::default();
        snapshot.services.insert(
            AiService::Tts,
            ServiceRuntimeStatus {
                phase: RuntimePhase::ModelReady,
                active_model: None,
                pending_model: Some("tts_next".into()),
                ..Default::default()
            },
        );
        assert_eq!(SpeechHubLoadedModels::from_runtime(&snapshot).tts, None);
    }

    #[test]
    fn count_and_rows_are_one_snapshot_and_never_claim_inference_ready() {
        let inventory = SpeechInventory {
            tts: vec![option("piper", false, false), option("xtts", true, true)],
            asr: vec![option("piper", true, false)],
            errors: vec!["invalid community plugin".into()],
        };
        let projected = SpeechHubCatalog::from_inventory(&inventory);
        assert_eq!(projected.category_counts, [2, 1, 0, 0, 0]);
        assert_eq!(projected.discovery_error_count, 1);
        assert_eq!(projected.entries.len(), 3);
        assert_eq!(projected.entries[0].category, SpeechHubCategory::Tts);
        assert_eq!(projected.entries[2].category, SpeechHubCategory::Asr);
        assert_eq!(
            projected.entries[0].state,
            SpeechHubResourceState::Discovered
        );
        assert_eq!(
            projected.entries[1].state,
            SpeechHubResourceState::PackageAndAssets
        );
        assert_eq!(
            projected.entries[2].state,
            SpeechHubResourceState::PackageOnly
        );
        assert!(
            projected
                .entries
                .iter()
                .all(|row| row.state.as_str() != "ready")
        );
    }

    #[test]
    fn all_resource_combinations_are_distinct() {
        let inventory = SpeechInventory {
            tts: vec![
                option("missing", false, false),
                option("package", true, false),
                option("assets", false, true),
                option("both", true, true),
            ],
            ..SpeechInventory::default()
        };
        let catalog = SpeechHubCatalog::from_inventory(&inventory);
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|entry| entry.state)
                .collect::<Vec<_>>(),
            [
                SpeechHubResourceState::Discovered,
                SpeechHubResourceState::PackageOnly,
                SpeechHubResourceState::AssetsOnly,
                SpeechHubResourceState::PackageAndAssets,
            ]
        );
    }

    #[test]
    fn new_snapshot_replaces_previous_catalog_including_category_counts() {
        let before = SpeechHubCatalog::from_inventory(&SpeechInventory {
            tts: vec![option("one", false, false), option("two", false, false)],
            ..SpeechInventory::default()
        });
        let after = SpeechHubCatalog::from_inventory(&SpeechInventory {
            asr: vec![option("asr", false, true)],
            ..SpeechInventory::default()
        });
        assert_eq!(before.category_counts, [2, 0, 0, 0, 0]);
        assert_eq!(after.category_counts, [0, 1, 0, 0, 0]);
        assert_eq!(after.entries.len(), 1);
        assert_eq!(after.entries[0].state, SpeechHubResourceState::AssetsOnly);
    }

    #[tokio::test]
    async fn real_discovery_reloads_valid_plugins_without_hiding_catalog_errors() {
        use crate::ai_runtime::speech_catalog::SpeechModelCatalog;

        let root = std::env::temp_dir().join(format!("nm-ai-hub-catalog-{}", uuid::Uuid::new_v4()));
        let plugins = root.join("AIData/plugins/community");
        let valid = plugins.join("c4_valid_tts");
        let broken = plugins.join("c4_broken_asr");
        std::fs::create_dir_all(&valid).unwrap();
        std::fs::create_dir_all(&broken).unwrap();
        let manifest = |id: &str, kind: &str| {
            serde_json::json!({
                "id": id,
                "name": id,
                "kind": kind,
                "api_version": 1,
                "entrypoint": "plugin.py",
                "version": "1.0.0",
                "publisher": "test",
                "permissions": [],
            })
            .to_string()
        };
        std::fs::write(valid.join("plugin.json"), manifest("c4_valid_tts", "tts")).unwrap();
        std::fs::write(valid.join("plugin.py"), "class Plugin: pass").unwrap();
        std::fs::write(broken.join("plugin.json"), "{invalid").unwrap();
        std::fs::write(broken.join("plugin.py"), "class Plugin: pass").unwrap();

        let catalog = SpeechModelCatalog::for_desktop(root.clone());
        let before = SpeechHubCatalog::from_inventory(&catalog.snapshot().await);
        assert_eq!(before.discovery_error_count, 1);
        assert!(before.entries.iter().any(|item| item.id == "c4_valid_tts"));
        assert!(!before.entries.iter().any(|item| item.id == "c4_broken_asr"));
        assert_eq!(
            before.category_counts[0] as usize,
            before
                .entries
                .iter()
                .filter(|item| item.category == SpeechHubCategory::Tts)
                .count()
        );

        std::fs::remove_dir_all(&valid).unwrap();
        std::fs::write(broken.join("plugin.json"), manifest("c4_broken_asr", "asr")).unwrap();
        let after = SpeechHubCatalog::from_inventory(&catalog.snapshot().await);
        assert_eq!(after.discovery_error_count, 0);
        assert!(!after.entries.iter().any(|item| item.id == "c4_valid_tts"));
        assert!(
            after
                .entries
                .iter()
                .any(|item| item.id == "c4_broken_asr" && item.category == SpeechHubCategory::Asr)
        );
        assert_eq!(
            after.category_counts[1] as usize,
            after
                .entries
                .iter()
                .filter(|item| item.category == SpeechHubCategory::Asr)
                .count()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn category_scopes_identical_ids_without_deduplication() {
        let inventory = SpeechInventory {
            tts: vec![option("shared", false, true)],
            asr: vec![option("shared", true, false)],
            ..SpeechInventory::default()
        };
        let catalog = SpeechHubCatalog::from_inventory(&inventory);
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(catalog.entries[0].id, catalog.entries[1].id);
        assert_ne!(catalog.entries[0].category, catalog.entries[1].category);
        assert_ne!(catalog.entries[0].state, catalog.entries[1].state);
    }
}
