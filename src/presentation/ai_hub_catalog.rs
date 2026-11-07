//! Read-only speech catalog projection shared by GUI adapters and executable tests.

use crate::ai_runtime::speech_catalog::{SpeechInventory, SpeechModelOption};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechHubCatalog {
    pub entries: Vec<SpeechHubEntry>,
    pub category_counts: [i32; 5],
    pub discovery_error_count: usize,
}

impl SpeechHubCatalog {
    pub fn from_inventory(inventory: &SpeechInventory) -> Self {
        let mut entries = Vec::with_capacity(inventory.tts.len() + inventory.asr.len());
        entries.extend(
            inventory
                .tts
                .iter()
                .map(|option| entry(SpeechHubCategory::Tts, option)),
        );
        entries.extend(
            inventory
                .asr
                .iter()
                .map(|option| entry(SpeechHubCategory::Asr, option)),
        );
        Self {
            entries,
            category_counts: [
                i32::try_from(inventory.tts.len()).unwrap_or(i32::MAX),
                i32::try_from(inventory.asr.len()).unwrap_or(i32::MAX),
                0,
                0,
                0,
            ],
            discovery_error_count: inventory.errors.len(),
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
