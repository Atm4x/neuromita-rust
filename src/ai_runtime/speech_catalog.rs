//! Read-only inventory of packaged Python speech plugins and verified asset revisions.
//! Discovery never imports or executes a plugin, and it never installs anything.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::ai_runtime::{
    AiService,
    assets::ModelAssetStore,
    layer::valid_segment,
    module_contract::{ModelModuleDeclaration, PythonPluginManifest},
};

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

#[derive(Debug, Clone, Default)]
pub struct ModuleDiscovery {
    pub modules: Vec<ModelModuleDeclaration>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SpeechModelCatalog {
    pub plugin_roots: Vec<PathBuf>,
    pub data_directory: PathBuf,
}

type PluginManifest = PythonPluginManifest;

pub(super) fn executable_plugin_root(data_directory: &Path) -> PathBuf {
    for path in ["AIData/plugins", "AIData/Scripts"] {
        let candidate = data_directory.join(path);
        if std::fs::symlink_metadata(&candidate).is_ok() {
            return candidate;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python/plugins/builtin")
}

fn plugin_candidates(root: &Path) -> Vec<PathBuf> {
    if !std::fs::symlink_metadata(root)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.is_symlink())
    {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten().take(256) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() && !kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() && (entry.file_name() == "builtin" || entry.file_name() == "community") {
            if let Ok(children) = std::fs::read_dir(&path) {
                candidates.extend(children.flatten().take(256).filter_map(|child| {
                    child
                        .file_type()
                        .ok()
                        .filter(|kind| kind.is_dir() || kind.is_symlink())
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
    if !std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.is_symlink())
    {
        return Err("plugin directory must be a real directory".into());
    }
    let manifest_path = path.join("plugin.json");
    let metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("manifest not readable: {error}"))?;
    if !metadata.is_file() || metadata.is_symlink() || metadata.len() > 64 * 1024 {
        return Err("manifest is not a bounded regular file".into());
    }
    let source = std::fs::read(&manifest_path).map_err(|error| error.to_string())?;
    let manifest: PluginManifest = serde_json::from_slice(&source)
        .map_err(|error| format!("invalid manifest JSON: {error}"))?;
    ModelModuleDeclaration::from_python_manifest(&manifest, &source)
        .map_err(|error| format!("invalid module declaration: {error}"))?;
    if !valid_segment(&manifest.id)
        || path
            .file_name()
            .is_none_or(|name| name != manifest.id.as_str())
        || manifest.api_version != 1
        || manifest.entrypoint != "plugin.py"
        || !matches!(
            manifest.kind.as_str(),
            "tts" | "asr" | "embedding" | "reranker" | "beats"
        )
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

pub(super) fn verify_executable_plugin(
    root: &Path,
    plugin_id: &str,
    service: &str,
) -> Result<(), String> {
    if !valid_segment(plugin_id) || !matches!(service, "tts" | "asr") {
        return Err("unsupported selected speech plugin".into());
    }
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("plugin root unavailable: {error}"))?;
    if !metadata.is_dir() || metadata.is_symlink() {
        return Err("plugin root must be a real directory".into());
    }
    let matches = plugin_candidates(root)
        .into_iter()
        .filter(|directory| directory.file_name().is_some_and(|name| name == plugin_id))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "selected plugin {plugin_id} is missing or ambiguous in executable plugin root"
        ));
    }
    let directory = &matches[0];
    let community = directory
        .parent()
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "community"));
    let manifest = inspect_manifest(directory, community)?;
    if manifest.kind != service {
        return Err(format!(
            "selected plugin {plugin_id} does not provide {service}"
        ));
    }
    Ok(())
}

impl SpeechModelCatalog {
    pub fn for_desktop(data_directory: PathBuf) -> Self {
        let plugin_roots = vec![executable_plugin_root(&data_directory)];
        Self {
            plugin_roots,
            data_directory,
        }
    }

    pub fn module_declarations(&self) -> ModuleDiscovery {
        let mut discovery = ModuleDiscovery::default();
        let mut models = BTreeMap::<(String, String), ModelModuleDeclaration>::new();
        let mut blocked = std::collections::HashSet::<String>::new();
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
                    discovery.errors.push(format!("ambiguous module id: {id}"));
                    blocked.insert(id.to_owned());
                    continue;
                }
                let symlinked = std::fs::symlink_metadata(&directory)
                    .is_ok_and(|metadata| metadata.is_symlink());
                if !symlinked && !directory.join("plugin.json").exists() {
                    continue;
                }
                let community = directory.parent().is_some_and(|parent| {
                    parent.file_name().is_some_and(|name| name == "community")
                });
                let manifest = match inspect_manifest(&directory, community) {
                    Ok(manifest) => manifest,
                    Err(reason) => {
                        discovery.errors.push(format!("module {id}: {reason}"));
                        blocked.insert(id.to_owned());
                        continue;
                    }
                };
                let Ok(source) = std::fs::read(directory.join("plugin.json")) else {
                    discovery
                        .errors
                        .push(format!("module {id}: manifest disappeared"));
                    continue;
                };
                match ModelModuleDeclaration::from_python_manifest(&manifest, &source) {
                    Ok(module) => {
                        let key = (manifest.kind, manifest.id);
                        if models.contains_key(&key) {
                            discovery.errors.push(format!("ambiguous module id: {id}"));
                            blocked.insert(id.to_owned());
                        } else {
                            models.insert(key, module);
                        }
                    }
                    Err(error) => {
                        discovery.errors.push(format!("module {id}: {error}"));
                        blocked.insert(id.to_owned());
                    }
                }
            }
        }
        for text in BUNDLED_SPEECH_MANIFESTS {
            let Ok(manifest) = serde_json::from_str::<PluginManifest>(text) else {
                continue;
            };
            if let Ok(module) =
                ModelModuleDeclaration::from_python_manifest(&manifest, text.as_bytes())
            {
                models.entry((manifest.kind, manifest.id)).or_insert(module);
            }
        }
        discovery.modules = models
            .into_iter()
            .filter(|((_, id), _)| !blocked.contains(id))
            .map(|(_, module)| module)
            .collect();
        discovery
    }

    pub async fn snapshot(&self) -> SpeechInventory {
        let discovered = self.module_declarations();
        let mut inventory = SpeechInventory {
            errors: discovered.errors,
            ..SpeechInventory::default()
        };
        let mut models = BTreeMap::<(String, String), SpeechModelOption>::new();
        for module in discovered.modules {
            let kind = match module.service {
                AiService::Tts => "tts",
                AiService::Asr => "asr",
                _ => continue,
            };
            let id = module.id.as_str().to_owned();
            models.insert(
                (kind.to_owned(), id.clone()),
                SpeechModelOption {
                    id,
                    name: module.name,
                    package_detected: false,
                    verified_assets: false,
                    installed_revisions: Vec::new(),
                },
            );
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

    #[test]
    fn clean_profile_lists_module_models_without_python_or_inference_components() {
        let root = std::env::temp_dir().join(format!("nm-module-clean-{}", uuid::Uuid::new_v4()));
        let modules = SpeechModelCatalog::for_desktop(root).module_declarations();
        let speech = modules
            .modules
            .iter()
            .find(|module| module.id.as_str() == "mock_tts")
            .unwrap();
        assert_eq!(speech.models.len(), 1);
        assert_eq!(speech.models[0].id.as_str(), "mock_tts");
        assert_eq!(
            speech.bootstrap,
            crate::ai_runtime::module_contract::ModuleBootstrap::python_sdk_312()
        );
        assert!(
            modules
                .modules
                .iter()
                .any(|module| module.id.as_str() == "mock_embedding")
        );
    }

    #[test]
    fn invalid_module_in_executable_root_cannot_fallback_to_embedded_copy() {
        let root = std::env::temp_dir().join(format!("nm-module-invalid-{}", uuid::Uuid::new_v4()));
        let plugins = root.join("AIData/plugins");
        std::fs::create_dir_all(plugins.join("mock_tts")).unwrap();
        std::fs::write(plugins.join("mock_tts/plugin.json"),
            r#"{"id":"mock_tts","name":"Fake","kind":"tts","entrypoint":"plugin.py","api_version":1,"bootstrap":{"kind":"python_sdk","python_minor":"3.13","sdk_api_version":1}}"#).unwrap();
        std::fs::write(plugins.join("mock_tts/plugin.py"), "class Plugin: pass").unwrap();
        let modules = SpeechModelCatalog::for_desktop(root.clone()).module_declarations();
        assert!(!modules.errors.is_empty());
        assert!(
            !modules
                .modules
                .iter()
                .any(|module| module.id.as_str() == "mock_tts")
        );
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

    #[test]
    fn executable_root_and_selected_plugin_use_the_same_resolver() {
        let root = std::env::temp_dir().join(format!("nm-plugin-root-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("AIData/Scripts");
        let plugins = root.join("AIData/plugins");
        fixture(&legacy, "", "legacy_tts", "tts", &[]);
        assert_eq!(executable_plugin_root(&root), legacy);
        fixture(&plugins, "community", "active_asr", "asr", &[]);
        assert_eq!(executable_plugin_root(&root), plugins);
        assert!(verify_executable_plugin(&plugins, "active_asr", "asr").is_ok());
        assert!(verify_executable_plugin(&plugins, "legacy_tts", "tts").is_err());
        assert!(verify_executable_plugin(&plugins, "active_asr", "tts").is_err());
        fixture(&plugins, "builtin", "active_asr", "asr", &[]);
        assert!(
            verify_executable_plugin(&plugins, "active_asr", "asr")
                .unwrap_err()
                .contains("ambiguous")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_refuses_permissioned_plugins_and_symlinked_roots() {
        let root = std::env::temp_dir().join(format!("nm-plugin-perms-{}", uuid::Uuid::new_v4()));
        let plugins = root.join("AIData/plugins");
        fixture(&plugins, "community", "denied_tts", "tts", &["network"]);
        assert!(
            verify_executable_plugin(&plugins, "denied_tts", "tts")
                .unwrap_err()
                .contains("permission approval")
        );
        #[cfg(unix)]
        {
            let link = root.join("linked_plugins");
            std::os::unix::fs::symlink(&plugins, &link).unwrap();
            assert!(
                verify_executable_plugin(&link, "denied_tts", "tts")
                    .unwrap_err()
                    .contains("real directory")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_plugin_cannot_hide_an_ambiguous_id_or_appear_selectable() {
        let root = std::env::temp_dir().join(format!("nm-plugin-link-{}", uuid::Uuid::new_v4()));
        let plugins = root.join("AIData/plugins");
        fixture(&plugins, "builtin", "valid_tts", "tts", &[]);
        std::fs::create_dir_all(plugins.join("community")).unwrap();
        let link = plugins.join("community/valid_tts");
        std::os::unix::fs::symlink(plugins.join("builtin/valid_tts"), &link).unwrap();
        let catalog = SpeechModelCatalog {
            plugin_roots: vec![plugins.clone()],
            data_directory: root.clone(),
        };
        let ambiguous = catalog.snapshot().await;
        assert!(!ambiguous.tts.iter().any(|model| model.id == "valid_tts"));
        assert!(
            ambiguous
                .errors
                .iter()
                .any(|error| error.contains("ambiguous"))
        );
        assert!(
            verify_executable_plugin(&plugins, "valid_tts", "tts")
                .unwrap_err()
                .contains("ambiguous")
        );

        std::fs::remove_dir_all(plugins.join("builtin/valid_tts")).unwrap();
        let linked_only = catalog.snapshot().await;
        assert!(!linked_only.tts.iter().any(|model| model.id == "valid_tts"));
        assert!(
            linked_only
                .errors
                .iter()
                .any(|error| error.contains("real directory"))
        );
        assert!(verify_executable_plugin(&plugins, "valid_tts", "tts").is_err());
        std::fs::remove_dir_all(root).unwrap();
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
