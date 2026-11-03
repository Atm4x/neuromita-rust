//! Explicit application-owned activation of selected Python AI workers.

use std::{collections::HashSet, sync::Arc};

use tokio::{process::Command, time::timeout};

use thiserror::Error;

use crate::voice::VoiceProvider;

use super::{
    AiRuntimeError, AiService, AiSupervisor, ModelActivation, ProcessTopology, PythonHostConfig,
    PythonVoiceProvider, composition::RuntimeComposition, voice_profile::VoiceProfileStore,
};

pub struct AiWorkerSelection {
    pub service: AiService,
    pub composition: RuntimeComposition,
    pub models: Vec<ModelActivation>,
}

pub struct AiApplicationConfig {
    pub topology: ProcessTopology,
    pub host: PythonHostConfig,
    pub workers: Vec<AiWorkerSelection>,
    pub voice_profiles: Option<Arc<VoiceProfileStore>>,
}

#[derive(Debug, Error)]
pub enum AiApplicationError {
    #[error("invalid AI application selection: {0}")]
    Invalid(String),
    #[error("Python interpreter check failed: {0}")]
    Interpreter(String),
    #[error(transparent)]
    Runtime(#[from] AiRuntimeError),
}

pub struct RunningAiApplication {
    supervisor: Arc<AiSupervisor>,
    active: Vec<AiService>,
    voice_profiles: Option<Arc<VoiceProfileStore>>,
}

impl AiApplicationConfig {
    fn validate(&self) -> Result<(), AiApplicationError> {
        if self.workers.is_empty() {
            return Err(AiApplicationError::Invalid("no AI workers selected".into()));
        }
        let mut selected = HashSet::new();
        let mut tts = false;
        for worker in &self.workers {
            if !worker.composition.services().contains(&worker.service) {
                return Err(AiApplicationError::Invalid(
                    "worker service absent from composition".into(),
                ));
            }
            let models: HashSet<_> = worker.models.iter().map(|model| model.service).collect();
            let services: HashSet<_> = worker.composition.services().iter().copied().collect();
            if models != services || models.len() != worker.models.len() {
                return Err(AiApplicationError::Invalid(
                    "composition must select exactly one model per service".into(),
                ));
            }
            for service in services {
                if !selected.insert(service) {
                    return Err(AiApplicationError::Invalid(
                        "a service is selected by multiple workers".into(),
                    ));
                }
                tts |= service == AiService::Tts;
            }
            if self.topology == ProcessTopology::Split
                && worker.composition.services() != [worker.service]
            {
                return Err(AiApplicationError::Invalid(
                    "split workers must contain exactly one service".into(),
                ));
            }
        }
        if !tts {
            return Err(AiApplicationError::Invalid(
                "application TTS requires an explicit selected model".into(),
            ));
        }
        if self.topology == ProcessTopology::Shared && self.workers.len() != 1 {
            return Err(AiApplicationError::Invalid(
                "shared topology requires one composed worker".into(),
            ));
        }
        Ok(())
    }

    pub async fn activate(self) -> Result<RunningAiApplication, AiApplicationError> {
        self.validate()?;
        let output = timeout(
            self.host.request_timeout,
            Command::new(&self.host.python_executable)
                .arg("--version")
                .output(),
        )
        .await
        .map_err(|_| AiApplicationError::Interpreter("version probe timed out".into()))?
        .map_err(|error| AiApplicationError::Interpreter(error.to_string()))?;
        let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let reported = if reported.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        } else {
            reported
        };
        if !output.status.success()
            || !self
                .workers
                .iter()
                .all(|worker| reported == format!("Python {}", worker.composition.python_version()))
        {
            return Err(AiApplicationError::Interpreter(format!(
                "interpreter reports {reported:?}, which does not match selected locked Python"
            )));
        }
        let supervisor = Arc::new(AiSupervisor::new(self.topology, self.host));
        let mut active = Vec::new();
        for worker in self.workers {
            let activation = supervisor
                .activate_composed(worker.service, &worker.composition, &worker.models)
                .await;
            if let Err(error) = activation {
                for service in active.into_iter().rev() {
                    let _ = supervisor.stop(service).await;
                }
                return Err(error.into());
            }
            active.push(worker.service);
        }
        Ok(RunningAiApplication {
            supervisor,
            active,
            voice_profiles: self.voice_profiles,
        })
    }
}

impl RunningAiApplication {
    pub fn voice_provider(&self) -> Arc<dyn VoiceProvider> {
        let mut provider = PythonVoiceProvider::new(Arc::clone(&self.supervisor));
        if let Some(profiles) = &self.voice_profiles {
            provider = provider.with_profiles(Arc::clone(profiles));
        }
        Arc::new(provider)
    }

    pub async fn shutdown(self) -> Result<(), AiApplicationError> {
        let mut first_error = None;
        for service in self.active.into_iter().rev() {
            if let Err(error) = self.supervisor.stop(service).await {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
}
