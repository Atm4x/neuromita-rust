//! Production AI installation: one pinned CPython environment, selected immutable layers,
//! model assets, and an explicit application activation boundary.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use ring::digest::{SHA256, digest};
use serde_json::{Value, json};
use thiserror::Error;

use super::{
    AiRuntimeError, AiService, ModelActivation, ProcessTopology, PythonHostConfig,
    application::{
        AiApplicationConfig, AiApplicationError, AiWorkerSelection, RunningAiApplication,
    },
    assets::{
        AssetInput, ModelAssetError, ModelAssetStore, ModelGenerationError, ModelGenerationRequest,
    },
    backend::{BackendError, BackendInstaller, BackendKind, LockedBackend},
    composition::{CompositionError, RuntimeComposition, ServiceSelection},
    fetch_install_plan_for,
    install::{
        AssetState, ModelAssetStatus, PluginInstallError, PluginInstallHandler, PluginInstallPlan,
        PluginInstallRequest,
    },
    install_operation::{InstallOperation, InstallOperationError, OperationStage, StageState},
    install_source::{InstallSourceError, InstallSourcePin},
    layer::valid_segment,
    managed::{ManagedRuntimeBootstrap, ManagedRuntimeError},
    overlay::OverlayInstaller,
    resolver::DependencyResolver,
    sdk::SdkPublisher,
    voice_profile::VoiceProfileStore,
};

#[derive(Clone)]
pub enum ModelAssets {
    None,
    Installed {
        revision: String,
    },
    Sources {
        revision: String,
        inputs: Vec<AssetInput>,
    },
    Catalog {
        revision: String,
        manifest: PathBuf,
    },
    Generated {
        revision: String,
        hook_id: String,
        source_plugin_id: String,
        source_revision: String,
        settings: Value,
        approved_hook_id: Option<String>,
    },
}

impl ModelAssets {
    fn revision(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Installed { revision }
            | Self::Sources { revision, .. }
            | Self::Catalog { revision, .. }
            | Self::Generated { revision, .. } => Some(revision),
        }
    }
}

pub struct SelectedModel {
    pub service: AiService,
    pub plugin_id: String,
    pub package_revision: String,
    pub backend: LockedBackend,
    pub extra_backends: Vec<LockedBackend>,
    pub settings: Value,
    pub assets: ModelAssets,
    pub approved_actions: HashSet<String>,
}

pub struct AiProductionInstaller {
    pub bootstrap: ManagedRuntimeBootstrap,
    pub sdk_root: PathBuf,
    pub plugins_directory: PathBuf,
    pub topology: ProcessTopology,
    pub timeout: Duration,
    pub max_asset_bytes: u64,
    pub voice_profiles: Option<Arc<VoiceProfileStore>>,
}

#[derive(Debug, Error)]
pub enum AiProductionError {
    #[error("invalid production AI selection: {0}")]
    Invalid(String),
    #[error("selected model assets are not ready: {0}")]
    Unready(String),
    #[error(transparent)]
    Bootstrap(#[from] ManagedRuntimeError),
    #[error(transparent)]
    Operation(#[from] InstallOperationError),
    #[error(transparent)]
    Source(#[from] InstallSourceError),
    #[error("AI installation task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error(transparent)]
    InstallPlan(#[from] AiRuntimeError),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error(transparent)]
    Plugin(#[from] PluginInstallError),
    #[error(transparent)]
    Assets(#[from] ModelAssetError),
    #[error(transparent)]
    Generated(#[from] ModelGenerationError),
    #[error(transparent)]
    Composition(#[from] CompositionError),
    #[error(transparent)]
    Activation(#[from] AiApplicationError),
}

struct PreflightedModel<'a> {
    selected: &'a SelectedModel,
    plan: PluginInstallPlan,
    assets: ModelAssets,
}

fn operation_fingerprint(
    preflighted: &[PreflightedModel<'_>],
    python_version: &str,
    python_executable: &std::path::Path,
    uv_executable: &std::path::Path,
    sdk_root: &std::path::Path,
    source_digest: &str,
) -> Result<String, AiProductionError> {
    let selected: Vec<Value> = preflighted
        .iter()
        .map(|model| {
            let selection = model.selected;
            let asset = match &model.assets {
                ModelAssets::None => json!({"kind": "none"}),
                ModelAssets::Installed { revision } => json!({"kind": "installed", "revision": revision}),
                ModelAssets::Sources { revision, inputs } => json!({
                    "kind": "sources", "revision": revision,
                    "inputs": inputs.iter().map(|input| json!({
                        "role": input.role,
                        "path": input.relative_path,
                        "sha256": input.sha256,
                        "source": match &input.source {
                            super::assets::AssetSource::LocalFile(path) => format!("local:{}", path.display()),
                            super::assets::AssetSource::Https(url) => format!("https:{url}"),
                        },
                    })).collect::<Vec<_>>(),
                }),
                ModelAssets::Catalog { .. } => unreachable!("resolved by whole-selection preflight"),
                ModelAssets::Generated {
                    revision, hook_id, source_plugin_id, source_revision,
                    settings, approved_hook_id,
                } => json!({
                    "kind": "generated", "revision": revision, "hook": hook_id,
                    "source_plugin": source_plugin_id, "source_revision": source_revision,
                    "settings": settings, "approved": approved_hook_id,
                }),
            };
            let mut approved: Vec<_> = selection.approved_actions.iter().collect();
            approved.sort_unstable();
            json!({
                "service": selection.service,
                "plugin_id": selection.plugin_id,
                "package_revision": selection.package_revision,
                "backend": selection.backend,
                "extra_backends": selection.extra_backends,
                "settings": selection.settings,
                "approved_actions": approved,
                "plan": model.plan,
                "assets": asset,
            })
        })
        .collect();
    let pinned = json!({
        "version": 1,
        "python_version": python_version,
        "python_executable": python_executable,
        "uv_executable": uv_executable,
        "sdk_root": sdk_root,
        "source_digest": source_digest,
        "selected": selected,
    });
    let encoded = serde_json::to_vec(&pinned).map_err(|_| {
        AiProductionError::Invalid("cannot fingerprint installation selection".into())
    })?;
    Ok(digest(&SHA256, &encoded)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn operation_stages(preflighted: &[PreflightedModel<'_>]) -> Vec<OperationStage> {
    let mut stages = Vec::new();
    for (index, model) in preflighted.iter().enumerate() {
        stages.push(OperationStage {
            id: format!(
                "{index}:primary-backend:{}",
                model.selected.backend.kind.as_str()
            ),
            repeatable: true,
        });
        for extra in &model.selected.extra_backends {
            stages.push(OperationStage {
                id: format!("{index}:extra-backend:{}", extra.kind.as_str()),
                repeatable: true,
            });
        }
        stages.push(OperationStage {
            id: format!("{index}:overlay:{}", model.selected.plugin_id),
            repeatable: true,
        });
        stages.push(OperationStage {
            id: format!("{index}:assets:{}", model.selected.plugin_id),
            repeatable: !matches!(model.assets, ModelAssets::Generated { .. }),
        });
    }
    stages
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn backend_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::TorchCpu => "torch-cpu",
        BackendKind::TorchCuda => "torch-cuda",
        BackendKind::OnnxCpu => "onnx-cpu",
        BackendKind::OnnxCuda => "onnx-cuda",
        BackendKind::OnnxDirectml => "onnx-directml",
        BackendKind::Ctranslate2Cpu => "ctranslate2-cpu",
        BackendKind::Ctranslate2Cuda => "ctranslate2-cuda",
        BackendKind::RemoteApi => "remote-api",
    }
}

struct AssetPreparation {
    store: ModelAssetStore,
    installer: PluginInstallHandler,
    host: PythonHostConfig,
    plugin_id: String,
    plan: PluginInstallPlan,
    selected: ModelAssets,
    default_assets: Vec<ModelAssetStatus>,
    dependency_layers: Vec<PathBuf>,
    python_version: String,
}

impl AssetPreparation {
    async fn run(self) -> Result<Option<String>, AiProductionError> {
        let Self {
            store,
            installer,
            host,
            plugin_id,
            plan,
            selected,
            default_assets,
            dependency_layers,
            python_version,
        } = self;
        if matches!(selected, ModelAssets::None) && plan.assets.iter().any(|asset| asset.required) {
            return Err(AiProductionError::Unready(format!(
                "{plugin_id} requires an explicitly selected model asset revision"
            )));
        }
        let revision = match selected {
            ModelAssets::None => None,
            ModelAssets::Installed { revision } => {
                store.verified_revision(&plugin_id, &revision).await?;
                Some(revision)
            }
            ModelAssets::Sources { revision, inputs } => {
                store.publish(&plugin_id, &revision, &plan, &inputs).await?;
                store.verified_revision(&plugin_id, &revision).await?;
                Some(revision)
            }
            ModelAssets::Catalog { revision, manifest } => {
                let sources = super::assets::catalog::ModelSourceCatalog::load(&manifest)
                    .await
                    .map_err(|error| AiProductionError::Invalid(error.to_string()))?
                    .resolve(&plugin_id, &revision, &plan)
                    .map_err(|error| AiProductionError::Invalid(error.to_string()))?;
                store
                    .publish(&plugin_id, &revision, &plan, &sources)
                    .await?;
                store.verified_revision(&plugin_id, &revision).await?;
                Some(revision)
            }
            ModelAssets::Generated {
                revision,
                hook_id,
                source_plugin_id,
                source_revision,
                settings,
                approved_hook_id,
            } => {
                store
                    .publish_generated(ModelGenerationRequest {
                        plugin_id: &plugin_id,
                        revision: &revision,
                        plan: &plan,
                        python_version: &python_version,
                        hook_id: &hook_id,
                        source_plugin_id: &source_plugin_id,
                        source_revision: &source_revision,
                        settings: &settings,
                        python_executable: &host.python_executable,
                        sdk_root: &host.sdk_root,
                        plugins_directory: &host.plugins_directory,
                        dependency_layers: &dependency_layers,
                        approved_hook_id: approved_hook_id.as_deref(),
                    })
                    .await?;
                store.verified_revision(&plugin_id, &revision).await?;
                Some(revision)
            }
        };
        let assets = match &revision {
            Some(revision) => {
                installer
                    .inspect_revision_assets(&plugin_id, revision, &plan)
                    .await?
            }
            None => default_assets,
        };
        if let Some(missing) = assets.iter().find(|asset| {
            asset.required && !matches!(asset.state, AssetState::Present | AssetState::Verified)
        }) {
            return Err(AiProductionError::Unready(format!(
                "{plugin_id} requires {} ({:?})",
                missing.relative_path, missing.state
            )));
        }
        Ok(revision)
    }
}

impl AiProductionInstaller {
    fn validate(&self, selections: &[SelectedModel]) -> Result<(), AiProductionError> {
        if selections.is_empty()
            || selections.len() > 5
            || !self.sdk_root.is_absolute()
            || !self.plugins_directory.is_absolute()
            || !self.plugins_directory.is_dir()
            || self.timeout.is_zero()
            || self.max_asset_bytes == 0
        {
            return Err(AiProductionError::Invalid(
                "selected services, SDK, plugins, and positive limits are required".into(),
            ));
        }
        let mut services = HashSet::new();
        let mut plugins = HashSet::new();
        for model in selections {
            if !services.insert(model.service)
                || !plugins.insert(model.plugin_id.as_str())
                || !valid_segment(&model.plugin_id)
                || !valid_segment(&model.package_revision)
                || !model.settings.is_object()
                || model
                    .assets
                    .revision()
                    .is_some_and(|revision| !valid_segment(revision))
                || model.backend.python_version != self.bootstrap.python_version
                || model.extra_backends.len() > 4
                || model.extra_backends.iter().any(|backend| {
                    backend.python_version != self.bootstrap.python_version
                        || backend.kind == model.backend.kind
                })
                || model
                    .extra_backends
                    .iter()
                    .map(|backend| backend.kind)
                    .collect::<HashSet<_>>()
                    .len()
                    != model.extra_backends.len()
            {
                return Err(AiProductionError::Invalid(format!(
                    "invalid or conflicting service selection: {}",
                    model.plugin_id
                )));
            }
        }
        Ok(())
    }

    async fn preflight_model<'a>(
        &self,
        selection: &'a SelectedModel,
        plan: PluginInstallPlan,
        store: &ModelAssetStore,
    ) -> Result<PreflightedModel<'a>, AiProductionError> {
        if !plan.build_stages.is_empty() {
            return Err(AiProductionError::Invalid(
                "build stages have no verified execution pipeline".into(),
            ));
        }
        if plan.backend.is_none() && plan.supported_backends.is_empty() {
            return Err(AiProductionError::Invalid(format!(
                "plugin {} does not declare a required compute or remote backend",
                selection.plugin_id
            )));
        }
        selection.backend.validate()?;
        for backend in &selection.extra_backends {
            backend.validate()?;
        }
        let backend_id = backend_name(selection.backend.kind);
        let selected_extra: HashSet<_> = selection
            .extra_backends
            .iter()
            .map(|entry| backend_name(entry.kind))
            .collect();
        let required_extra: HashSet<_> =
            plan.required_backends.iter().map(String::as_str).collect();
        if selected_extra != required_extra {
            return Err(AiProductionError::Invalid(format!(
                "plugin {} requires supplemental backends: {}",
                selection.plugin_id,
                plan.required_backends.join(", ")
            )));
        }
        if plan
            .backend
            .as_deref()
            .is_some_and(|kind| kind != backend_id)
            || !plan.supported_backends.is_empty()
                && !plan
                    .supported_backends
                    .iter()
                    .any(|kind| kind == backend_id)
        {
            return Err(AiProductionError::Invalid(format!(
                "plugin {} does not support selected backend {backend_id}",
                selection.plugin_id
            )));
        }
        if plan
            .actions
            .iter()
            .any(|action| !selection.approved_actions.contains(&action.id))
        {
            return Err(AiProductionError::Invalid(format!(
                "plugin {} has unapproved installation actions",
                selection.plugin_id
            )));
        }
        if matches!(selection.assets, ModelAssets::None)
            && plan.assets.iter().any(|asset| asset.required)
        {
            return Err(AiProductionError::Unready(format!(
                "{} requires an explicitly selected model asset revision",
                selection.plugin_id
            )));
        }
        let assets =
            match &selection.assets {
                ModelAssets::Installed { revision } => {
                    store
                        .verified_revision(&selection.plugin_id, revision)
                        .await?;
                    selection.assets.clone()
                }
                ModelAssets::Catalog { revision, manifest } => {
                    let sources = super::assets::catalog::ModelSourceCatalog::load(manifest)
                        .await
                        .map_err(|error| AiProductionError::Invalid(error.to_string()))?
                        .resolve(&selection.plugin_id, revision, &plan)
                        .map_err(|error| AiProductionError::Invalid(error.to_string()))?;
                    store
                        .preflight_sources(&selection.plugin_id, revision, &plan, &sources)
                        .await?;
                    ModelAssets::Sources {
                        revision: revision.clone(),
                        inputs: sources,
                    }
                }
                ModelAssets::Generated {
                    hook_id,
                    source_plugin_id,
                    approved_hook_id,
                    ..
                } => {
                    if approved_hook_id.as_deref() != Some(hook_id.as_str()) {
                        return Err(AiProductionError::Generated(
                            ModelGenerationError::NotApproved,
                        ));
                    }
                    if !plan.asset_hooks.iter().any(|hook| {
                        &hook.id == hook_id && &hook.source_plugin_id == source_plugin_id
                    }) {
                        return Err(AiProductionError::Generated(ModelGenerationError::Invalid(
                            "undeclared asset hook or mismatched source plugin".into(),
                        )));
                    }
                    selection.assets.clone()
                }
                ModelAssets::Sources { revision, inputs } => {
                    store
                        .preflight_sources(&selection.plugin_id, revision, &plan, inputs)
                        .await?;
                    selection.assets.clone()
                }
                _ => selection.assets.clone(),
            };
        Ok(PreflightedModel {
            selected: selection,
            plan,
            assets,
        })
    }

    /// Prepare selected Python models without starting any inference worker.
    /// A failure never activates a partially installed set of services.
    pub async fn prepare(
        &self,
        selections: &[SelectedModel],
    ) -> Result<AiApplicationConfig, AiProductionError> {
        self.validate(selections)?;
        let bootstrap = self.bootstrap.clone();
        let task = tokio::spawn(async move { bootstrap.prepare().await });
        let cancel = task.abort_handle();
        let _abort_on_drop = AbortOnDrop(cancel);
        let managed = task.await??;
        let data = &self.bootstrap.uv.data_directory;
        let publisher = SdkPublisher::new(data.clone());
        let verified_sdk = publisher.resolve_active().map_err(|error| {
            AiProductionError::Invalid(format!("verified SDK is not ready: {error}"))
        })?;
        if self.sdk_root != publisher.sdk_directory() && self.sdk_root != verified_sdk {
            return Err(AiProductionError::Invalid(
                "configured SDK bypasses verified installation".into(),
            ));
        }
        let cache = data.join("cache/uv");
        let backends = BackendInstaller {
            uv_executable: managed.uv_executable.clone(),
            python_executable: managed.python_executable.clone(),
            backends_directory: data.join("backends"),
            cache_directory: cache.clone(),
            timeout: self.timeout,
        };
        let overlays = OverlayInstaller {
            uv_executable: managed.uv_executable.clone(),
            python_executable: managed.python_executable.clone(),
            overlays_directory: data.join("overlays"),
            cache_directory: cache.clone(),
            timeout: self.timeout,
        };
        let installer = PluginInstallHandler {
            resolver: DependencyResolver {
                uv_executable: managed.uv_executable.clone(),
                python_executable: managed.python_executable.clone(),
                cache_directory: cache,
                timeout: self.timeout,
            },
            overlays,
            assets_directory: data.join("models"),
        };
        let store = ModelAssetStore {
            root: data.join("models"),
            max_asset_bytes: self.max_asset_bytes,
            timeout: self.timeout,
        };
        let host = PythonHostConfig {
            python_executable: managed.python_executable,
            sdk_root: verified_sdk,
            plugins_directory: self.plugins_directory.clone(),
            models_directory: Some(store.root.clone()),
            request_timeout: self.timeout,
        };
        let source_pin = InstallSourcePin::capture(
            &self.plugins_directory,
            selections.iter().map(|model| model.plugin_id.clone()),
            &host.python_executable,
            &managed.uv_executable,
        )?;
        let mut preflighted = Vec::with_capacity(selections.len());
        for selection in selections {
            let plan =
                fetch_install_plan_for(&host, &selection.plugin_id, selection.settings.clone())
                    .await?;
            plan.validate(&self.bootstrap.python_version)?;
            preflighted.push(self.preflight_model(selection, plan, &store).await?);
            source_pin.verify()?;
        }
        let fingerprint = operation_fingerprint(
            &preflighted,
            &self.bootstrap.python_version,
            &host.python_executable,
            &backends.uv_executable,
            &host.sdk_root,
            source_pin.digest(),
        )?;
        let stages = operation_stages(&preflighted);
        let mut operation = InstallOperation::open(data, &fingerprint, stages)?;
        let mut next_stage = 0;
        let mut layers = Vec::with_capacity(selections.len());
        let mut activation = Vec::with_capacity(selections.len());
        for preflighted_model in preflighted {
            let PreflightedModel {
                selected: selection,
                plan,
                assets,
            } = preflighted_model;
            let mut backend_layers = Vec::new();
            let mut dependencies = Vec::new();
            let backend = &selection.backend;
            let backend_id = backend_name(backend.kind);
            source_pin.verify()?;
            operation.start(next_stage)?;
            let task_backend_installer = backends.clone();
            let task_backend = backend.clone();
            let task =
                tokio::spawn(async move { task_backend_installer.install(&task_backend).await });
            let _abort_on_drop = AbortOnDrop(task.abort_handle());
            let path = task.await??;
            source_pin.verify()?;
            if !backends.installed(backend).await? {
                return Err(AiProductionError::Invalid(
                    "backend receipt missing after installation".into(),
                ));
            }
            operation.verified(next_stage)?;
            next_stage += 1;
            dependencies.push(path);
            backend_layers.push(backend.clone());
            let mut backend_packages = backend.packages.clone();
            for extra in &selection.extra_backends {
                source_pin.verify()?;
                operation.start(next_stage)?;
                let worker = backends.clone();
                let plan = extra.clone();
                let task = tokio::spawn(async move { worker.install(&plan).await });
                let _abort_on_drop = AbortOnDrop(task.abort_handle());
                let extra_path = task.await??;
                source_pin.verify()?;
                dependencies.push(extra_path);
                if !backends.installed(extra).await? {
                    return Err(AiProductionError::Invalid(
                        "supplemental backend receipt missing".into(),
                    ));
                }
                operation.verified(next_stage)?;
                next_stage += 1;
                backend_layers.push(extra.clone());
                backend_packages.extend(extra.packages.iter().cloned());
            }
            source_pin.verify()?;
            operation.start(next_stage)?;
            let task_installer = installer.clone();
            let task_plugin = selection.plugin_id.clone();
            let task_revision = selection.package_revision.clone();
            let task_python = self.bootstrap.python_version.clone();
            let task_backend = backend_id.to_owned();
            let task_backend_packages = backend_packages;
            let task_plan = plan.clone();
            let task_approvals = selection.approved_actions.clone();
            let task = tokio::spawn(async move {
                task_installer
                    .install_with_approval(
                        PluginInstallRequest {
                            plugin_id: &task_plugin,
                            revision: &task_revision,
                            python_version: &task_python,
                            selected_backend: Some(&task_backend),
                            plan: &task_plan,
                            backend_packages: &task_backend_packages,
                        },
                        &task_approvals,
                    )
                    .await
            });
            let _abort_on_drop = AbortOnDrop(task.abort_handle());
            let installed = task.await??;
            source_pin.verify()?;
            if !installer.overlays.installed(&installed.overlay).await? {
                return Err(AiProductionError::Invalid(
                    "plugin overlay receipt missing".into(),
                ));
            }
            operation.verified(next_stage)?;
            next_stage += 1;
            dependencies.push(installed.overlay_path);
            // Generated hooks may write outside the immutable asset layer. Never
            // automatically re-execute an interrupted or already verified hook.
            let recovered_generated = if let ModelAssets::Generated { revision, .. } = &assets {
                match operation.stage_state(next_stage)? {
                    StageState::Pending => {
                        source_pin.verify()?;
                        operation.start(next_stage)?;
                        None
                    }
                    StageState::Started | StageState::Verified => {
                        store
                            .verified_revision(&selection.plugin_id, revision)
                            .await
                            .map_err(|_| {
                                InstallOperationError::Invalid(
                                    "interrupted model generation has no verified asset receipt",
                                )
                            })?;
                        operation.verified(next_stage)?;
                        Some(revision.clone())
                    }
                }
            } else {
                source_pin.verify()?;
                operation.start(next_stage)?;
                None
            };
            let task_store = store.clone();
            let task_installer = installer.clone();
            let task_host = host.clone();
            let task_plugin = selection.plugin_id.clone();
            let task_assets = assets;
            let task_python = self.bootstrap.python_version.clone();
            let asset_revision = if let Some(revision) = recovered_generated {
                Some(revision)
            } else {
                let task = tokio::spawn(async move {
                    AssetPreparation {
                        store: task_store,
                        installer: task_installer,
                        host: task_host,
                        plugin_id: task_plugin,
                        plan,
                        selected: task_assets,
                        default_assets: installed.assets,
                        dependency_layers: dependencies,
                        python_version: task_python,
                    }
                    .run()
                    .await
                });
                let _abort_on_drop = AbortOnDrop(task.abort_handle());
                task.await??
            };
            source_pin.verify()?;
            if let Some(revision) = &asset_revision {
                store
                    .verified_revision(&selection.plugin_id, revision)
                    .await?;
            }
            operation.verified(next_stage)?;
            next_stage += 1;
            layers.push(ServiceSelection {
                service: selection.service,
                backends: backend_layers,
                overlays: vec![installed.overlay],
            });
            activation.push(ModelActivation {
                service: selection.service,
                plugin_id: selection.plugin_id.clone(),
                settings: selection.settings.clone(),
                asset_revision,
            });
        }
        let mut workers = Vec::new();
        match self.topology {
            ProcessTopology::Shared => workers.push(AiWorkerSelection {
                service: activation[0].service,
                composition: RuntimeComposition::resolve(
                    self.topology,
                    activation[0].service,
                    &layers,
                    &backends,
                    &installer.overlays,
                )
                .await?,
                models: activation,
            }),
            ProcessTopology::Split => {
                for model in activation {
                    workers.push(AiWorkerSelection {
                        service: model.service,
                        composition: RuntimeComposition::resolve(
                            self.topology,
                            model.service,
                            &layers,
                            &backends,
                            &installer.overlays,
                        )
                        .await?,
                        models: vec![model],
                    });
                }
            }
        }
        source_pin.verify()?;
        operation.finish()?;
        Ok(AiApplicationConfig {
            topology: self.topology,
            host,
            workers,
            voice_profiles: self.voice_profiles.clone(),
        })
    }

    pub async fn prepare_and_activate(
        &self,
        selections: &[SelectedModel],
    ) -> Result<RunningAiApplication, AiProductionError> {
        Ok(self.prepare(selections).await?.activate().await?)
    }
}
