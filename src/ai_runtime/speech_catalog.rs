//! Read-only inventory of packaged Python speech plugins and verified asset revisions.
//! Discovery never imports or executes a plugin, and it never installs anything.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::ai_runtime::{assets::ModelAssetStore, layer::valid_segment};
use serde::Deserialize;

include!(concat!(env!("OUT_DIR"), "/speech-plugin-manifests.rs"));

pub struct RemoteTtsTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub models: &'static [&'static str],
}

// Python parity: src/presets/remote_voice_templates.py.
pub const REMOTE_TTS_TEMPLATES: &[RemoteTtsTemplate] = &[RemoteTtsTemplate {
    id: "fish_audio",
    name: "Fish Audio",
    models: &["s2.1-pro-free", "s1", "s2-pro", "s2.1-pro"],
}];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechModelOption {
    pub id: String,
    pub name: String,
    pub package_detected: bool,
    pub verified_assets: bool,
    pub installed_revisions: Vec<String>,
}

impl SpeechModelOption {
    pub fn installation_label(&self) -> &'static str {
        match (self.package_detected, self.verified_assets) {
            (true, true) => "окружение и файлы подтверждены",
            (true, false) => "окружение обнаружено; файлы не подтверждены",
            (false, true) => "файлы подтверждены; окружение отсутствует",
            (false, false) => "не установлено",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SpeechInventory {
    pub tts: Vec<SpeechModelOption>,
    pub asr: Vec<SpeechModelOption>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SpeechModelCatalog {
    pub plugin_roots: Vec<PathBuf>,
    pub data_directory: PathBuf,
}

#[derive(Deserialize)]
struct PluginManifest {
    id: String,
    name: String,
    kind: String,
    api_version: u32,
    entrypoint: String,
    version: Option<String>,
    publisher: Option<String>,
    permissions: Option<Vec<String>>,
}

fn plugin_candidates(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten().take(256) {
        if !entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        {
            continue;
        }
        let path = entry.path();
        if entry.file_name() == "builtin" || entry.file_name() == "community" {
            if let Ok(children) = std::fs::read_dir(&path) {
                candidates.extend(children.flatten().take(256).filter_map(|child| {
                    child
                        .file_type()
                        .ok()
                        .filter(|kind| kind.is_dir() && !kind.is_symlink())
                        .map(|_| child.path())
                }));
            }
        } else {
            candidates.push(path);
        }
    }
    candidates
}

fn valid_plugin_version(version: &str) -> bool {
    let core = version
        .split_once(['-', '+'])
        .map_or(version, |(core, _)| core);
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
}

fn inspect_manifest(path: &Path, community: bool) -> Result<PluginManifest, String> {
    let manifest_path = path.join("plugin.json");
    let metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("manifest not readable: {error}"))?;
    if !metadata.is_file() || metadata.is_symlink() || metadata.len() > 64 * 1024 {
        return Err("manifest is not a bounded regular file".into());
    }
    let manifest: PluginManifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid manifest JSON: {error}"))?;
    if !valid_segment(&manifest.id)
        || path
            .file_name()
            .is_none_or(|name| name != manifest.id.as_str())
        || manifest.api_version != 1
        || manifest.entrypoint != "plugin.py"
        || !matches!(manifest.kind.as_str(), "tts" | "asr")
        || manifest.name.trim().is_empty()
        || manifest.name.len() > 128
        || manifest
            .version
            .as_ref()
            .is_some_and(|version| !valid_plugin_version(version))
        || manifest.publisher.as_ref().is_some_and(|publisher| {
            publisher.is_empty()
                || publisher.len() > 80
                || !publisher
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
        || manifest.permissions.as_ref().is_some_and(|permissions| {
            permissions.len()
                != permissions
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                || permissions.iter().any(|permission| {
                    !matches!(
                        permission.as_str(),
                        "network" | "filesystem_read" | "filesystem_write" | "subprocess"
                    )
                })
        })
        || (community
            && (manifest.version.is_none()
                || manifest.publisher.is_none()
                || manifest.permissions.is_none()))
    {
        return Err("invalid or unsupported speech plugin manifest".into());
    }
    let entrypoint = path.join("plugin.py");
    if !std::fs::symlink_metadata(entrypoint)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.is_symlink())
    {
        return Err("missing or unsafe plugin entrypoint".into());
    }
    if manifest
        .permissions
        .as_ref()
        .is_some_and(|permissions| !permissions.is_empty())
    {
        return Err("plugin requires explicit permission approval".into());
    }
    Ok(manifest)
}

impl SpeechModelCatalog {
    pub fn for_desktop(data_directory: PathBuf) -> Self {
        let roots = vec![
            data_directory.join("AIData/Scripts"),
            data_directory.join("AIData/plugins"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python/plugins/builtin"),
        ];
        Self {
            plugin_roots: roots,
            data_directory,
        }
    }

    pub async fn snapshot(&self) -> SpeechInventory {
        let mut inventory = SpeechInventory::default();
        let mut models = BTreeMap::<(String, String), SpeechModelOption>::new();
        for root in &self.plugin_roots {
            let candidates = plugin_candidates(root);
            let mut counts = HashMap::<String, usize>::new();
            for directory in &candidates {
                if let Some(id) = directory.file_name().and_then(|name| name.to_str()) {
                    *counts.entry(id.to_owned()).or_default() += 1;
                }
            }
            for directory in candidates {
                let Some(id) = directory.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if counts[id] != 1 {
                    inventory
                        .errors
                        .push(format!("ambiguous speech plugin id: {id}"));
                    continue;
                }
                if !directory.join("plugin.json").exists() {
                    continue;
                }
                let community = directory.parent().is_some_and(|parent| {
                    parent.file_name().is_some_and(|name| name == "community")
                });
                let manifest = match inspect_manifest(&directory, community) {
                    Ok(manifest) => manifest,
                    Err(reason) => {
                        inventory
                            .errors
                            .push(format!("speech plugin {id}: {reason}"));
                        continue;
                    }
                };
                let key = (manifest.kind, manifest.id.clone());
                models.entry(key).or_insert(SpeechModelOption {
                    id: manifest.id,
                    name: manifest.name,
                    package_detected: false,
                    verified_assets: false,
                    installed_revisions: Vec::new(),
                });
            }
        }
        for text in BUNDLED_SPEECH_MANIFESTS {
            let Ok(manifest) = serde_json::from_str::<PluginManifest>(text) else {
                continue;
            };
            if !valid_segment(&manifest.id)
                || !matches!(manifest.kind.as_str(), "tts" | "asr")
                || manifest.name.is_empty()
            {
                continue;
            }
            let key = (manifest.kind, manifest.id.clone());
            models.entry(key).or_insert(SpeechModelOption {
                id: manifest.id,
                name: manifest.name,
                package_detected: false,
                verified_assets: false,
                installed_revisions: Vec::new(),
            });
        }
        for ((kind, id), option) in &mut models {
            let overlays = self.data_directory.join("AIData/overlays").join(id);
            if let Ok(entries) = std::fs::read_dir(&overlays) {
                for entry in entries.flatten().take(128) {
                    if !entry
                        .file_type()
                        .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                    {
                        continue;
                    }
                    let path = entry.path().join(".neuromita-overlay.json");
                    if let Ok(meta) = std::fs::symlink_metadata(&path)
                        && meta.is_file()
                        && !meta.is_symlink()
                        && meta.len() < 262_144
                        && let Ok(payload) = std::fs::read(&path)
                        && let Ok(locked) = serde_json::from_slice::<serde_json::Value>(&payload)
                        && locked.get("plugin_id").and_then(|v| v.as_str()) == Some(id)
                        && locked.get("revision").and_then(|v| v.as_str())
                            == entry.file_name().to_str()
                    {
                        option.package_detected = true;
                    }
                }
            }
            let store = ModelAssetStore {
                root: self.data_directory.join("AIData/models"),
                max_asset_bytes: 4 * 1024 * 1024 * 1024,
                timeout: Duration::from_secs(30),
            };
            let versions = store.root.join(id);
            if let Ok(entries) = std::fs::read_dir(&versions) {
                for entry in entries.flatten().take(128) {
                    if !entry
                        .file_type()
                        .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                    {
                        continue;
                    }
                    let revision = entry.file_name().to_string_lossy().to_string();
                    if valid_segment(&revision)
                        && store.verified_revision(id, &revision).await.is_ok()
                    {
                        option.verified_assets = true;
                        option.installed_revisions.push(revision);
                    }
                }
            }
            let list = if kind == "tts" {
                &mut inventory.tts
            } else {
                &mut inventory.asr
            };
            list.push(option.clone());
        }
        inventory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_manifests_without_treating_bare_directories_as_installs() {
        let root = std::env::temp_dir().join(format!("nm-speech-catalog-{}", uuid::Uuid::new_v4()));
        let scripts = root.join("plugins");
        std::fs::create_dir_all(scripts.join("gigaam_onnx")).unwrap();
        std::fs::write(
            scripts.join("gigaam_onnx/plugin.json"),
            r#"{"id":"gigaam_onnx","name":"GigaAM","kind":"asr","api_version":1,"entrypoint":"plugin.py"}"#,
        )
        .unwrap();
        std::fs::write(scripts.join("gigaam_onnx/plugin.py"), "class Plugin: pass").unwrap();
        let snapshot = SpeechModelCatalog {
            plugin_roots: vec![scripts],
            data_directory: root.clone(),
        }
        .snapshot()
        .await;
        let model = snapshot
            .asr
            .iter()
            .find(|item| item.id == "gigaam_onnx")
            .unwrap();
        assert!(!model.package_detected);
        assert!(!model.verified_assets);
        assert!(snapshot.tts.iter().any(|item| item.id == "fish_speech"));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn fixture(root: &Path, source: &str, id: &str, kind: &str, permissions: &[&str]) {
        let dir = root.join(source).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            serde_json::json!({
                "id": id,
                "name": id,
                "kind": kind,
                "api_version": 1,
                "entrypoint": "plugin.py",
                "version": "1.0.0",
                "publisher": "test",
                "permissions": permissions,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.py"), "class Plugin: pass").unwrap();
    }

    #[tokio::test]
    async fn discovers_nested_builtin_and_community_speech_without_installs() {
        let root = std::env::temp_dir().join(format!("nm-nested-plugins-{}", uuid::Uuid::new_v4()));
        let plugins = root.join("AIData/plugins");
        fixture(&plugins, "builtin", "dynamic_builtin", "asr", &[]);
        fixture(&plugins, "community", "dynamic_community", "tts", &[]);
        fixture(&plugins, "", "dynamic_flat", "tts", &[]);
        let inventory = SpeechModelCatalog {
            plugin_roots: vec![plugins],
            data_directory: root.clone(),
        }
        .snapshot()
        .await;
        for (options, id) in [
            (&inventory.asr, "dynamic_builtin"),
            (&inventory.tts, "dynamic_community"),
            (&inventory.tts, "dynamic_flat"),
        ] {
            let option = options.iter().find(|item| item.id == id).unwrap();
            assert!(!option.package_detected);
            assert!(!option.verified_assets);
        }
        assert!(inventory.errors.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn duplicate_ids_and_permissioned_or_invalid_plugins_are_not_selectable() {
        let root =
            std::env::temp_dir().join(format!("nm-invalid-plugins-{}", uuid::Uuid::new_v4()));
        let plugins = root.join("AIData/plugins");
        fixture(&plugins, "builtin", "duplicate_fixture", "asr", &[]);
        fixture(&plugins, "community", "duplicate_fixture", "asr", &[]);
        fixture(
            &plugins,
            "community",
            "permissioned_fixture",
            "tts",
            &["network"],
        );
        fixture(&plugins, "community", "safe_fixture", "tts", &[]);
        std::fs::remove_file(plugins.join("community/safe_fixture/plugin.py")).unwrap();
        let inventory = SpeechModelCatalog {
            plugin_roots: vec![plugins],
            data_directory: root.clone(),
        }
        .snapshot()
        .await;
        assert!(
            !inventory
                .asr
                .iter()
                .any(|item| item.id == "duplicate_fixture")
        );
        assert!(
            !inventory
                .tts
                .iter()
                .any(|item| item.id == "permissioned_fixture")
        );
        assert!(!inventory.tts.iter().any(|item| item.id == "safe_fixture"));
        assert!(
            inventory
                .errors
                .iter()
                .any(|error| error.contains("ambiguous"))
        );
        assert!(
            inventory
                .errors
                .iter()
                .any(|error| error.contains("permission approval"))
        );
        assert!(
            inventory
                .errors
                .iter()
                .any(|error| error.contains("entrypoint"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
