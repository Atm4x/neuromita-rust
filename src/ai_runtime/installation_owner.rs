//! Shared application-owned admission, cancellation, and durable operation journal
//! for installers. Component executors retain their implementation-specific work.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use ring::digest::{SHA256, digest};
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    AiService,
    install_operation::{
        InstallOperation, InstallOperationError, OperationFailure, OperationStage,
    },
    layer::valid_segment,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallComponentKind {
    Host,
    Package,
    Bundle,
    RuntimeLibrary,
    Resource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationTarget {
    pub kind: InstallComponentKind,
    pub service: Option<AiService>,
    pub module_id: String,
    pub revision: String,
    pub component: String,
}

impl InstallationTarget {
    fn validate(&self) -> Result<(), InstallationError> {
        if !valid_segment(&self.module_id)
            || self.revision.is_empty()
            || self.revision.len() > 128
            || !self.revision.bytes().all(|b| b.is_ascii_hexdigit())
            || !valid_segment(&self.component)
        {
            return Err(InstallationError::InvalidTarget);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationPhase {
    Executing,
    CancellationRequested,
    Cancelled,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationStatus {
    pub operation: Uuid,
    pub target: InstallationTarget,
    pub phase: InstallationPhase,
}

#[derive(Debug, Error)]
pub enum InstallationError {
    #[error("an installation is already active")]
    Busy,
    #[error("application is shutting down")]
    Stopping,
    #[error("invalid installation target")]
    InvalidTarget,
    #[error(transparent)]
    Journal(#[from] InstallOperationError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

struct Active {
    operation: Uuid,
    cancellation: CancellationToken,
}

struct State {
    active: Option<Active>,
    stopped: bool,
}

pub struct InstallationOwner {
    ai_data: PathBuf,
    state: Mutex<State>,
    updates: watch::Sender<Option<InstallationStatus>>,
}

pub struct InstallationPermit {
    owner: Arc<InstallationOwner>,
    target: InstallationTarget,
    operation: Uuid,
    cancellation: CancellationToken,
    journal: Option<InstallOperation>,
}

impl InstallationOwner {
    pub fn new(ai_data: PathBuf) -> Arc<Self> {
        let (updates, _) = watch::channel(None);
        Arc::new(Self {
            ai_data,
            state: Mutex::new(State {
                active: None,
                stopped: false,
            }),
            updates,
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<InstallationStatus>> {
        self.updates.subscribe()
    }

    pub fn current(&self) -> Option<InstallationStatus> {
        self.updates.borrow().clone()
    }

    pub fn begin(
        self: &Arc<Self>,
        target: InstallationTarget,
    ) -> Result<InstallationPermit, InstallationError> {
        target.validate()?;
        let mut state = self.state.lock().expect("installation owner lock poisoned");
        if state.stopped {
            return Err(InstallationError::Stopping);
        }
        if state.active.is_some() {
            return Err(InstallationError::Busy);
        }
        if !self.ai_data.is_absolute() {
            return Err(InstallationError::InvalidTarget);
        }
        match fs::symlink_metadata(&self.ai_data) {
            Ok(metadata) if !metadata.is_dir() || metadata.is_symlink() => {
                return Err(InstallationError::InvalidTarget);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&self.ai_data)?;
            }
            Err(error) => return Err(error.into()),
            _ => {}
        }
        let nonce = Uuid::new_v4();
        let fingerprint = format!(
            "{:?}|{:?}|{}|{}|{}|{}",
            target.kind, target.service, target.module_id, target.revision, target.component, nonce
        );
        let key = digest(&SHA256, fingerprint.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut journal = InstallOperation::open(
            &self.ai_data,
            &key,
            vec![OperationStage {
                id: format!("component:{}", target.component),
                repeatable: false,
            }],
        )?;
        journal.start(0)?;
        let operation = journal.operation_id();
        let cancellation = CancellationToken::new();
        state.active = Some(Active {
            operation,
            cancellation: cancellation.clone(),
        });
        self.updates.send_replace(Some(InstallationStatus {
            operation,
            target: target.clone(),
            phase: InstallationPhase::Executing,
        }));
        Ok(InstallationPermit {
            owner: Arc::clone(self),
            target,
            operation,
            cancellation,
            journal: Some(journal),
        })
    }

    pub fn cancel(&self, operation: Uuid) -> bool {
        let state = self.state.lock().expect("installation owner lock poisoned");
        let Some(active) = state
            .active
            .as_ref()
            .filter(|active| active.operation == operation)
        else {
            return false;
        };
        active.cancellation.cancel();
        if let Some(mut status) = self.current() {
            status.phase = InstallationPhase::CancellationRequested;
            self.updates.send_replace(Some(status));
        }
        true
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock().expect("installation owner lock poisoned");
        state.stopped = true;
        if let Some(active) = &state.active {
            active.cancellation.cancel();
            if let Some(mut status) = self.current() {
                status.phase = InstallationPhase::CancellationRequested;
                self.updates.send_replace(Some(status));
            }
        }
    }
}

impl InstallationPermit {
    pub fn operation(&self) -> Uuid {
        self.operation
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// The admission lock linearizes success against cancellation and shutdown.
    /// Only an authoritative executor receipt may call this method.
    pub fn complete(mut self) -> Result<(), InstallationError> {
        let mut state = self
            .owner
            .state
            .lock()
            .expect("installation owner lock poisoned");
        if state.stopped
            || self.cancellation.is_cancelled()
            || state
                .active
                .as_ref()
                .is_none_or(|active| active.operation != self.operation)
        {
            return Err(InstallationError::Stopping);
        }
        let journal = self.journal.as_mut().expect("installation journal present");
        journal.verified(0)?;
        journal.finish()?;
        self.journal.take();
        state.active = None;
        self.owner.updates.send_replace(Some(InstallationStatus {
            operation: self.operation,
            target: self.target.clone(),
            phase: InstallationPhase::Completed,
        }));
        Ok(())
    }

    pub fn fail(mut self) -> Result<(), InstallationError> {
        {
            let mut state = self
                .owner
                .state
                .lock()
                .expect("installation owner lock poisoned");
            self.journal
                .as_mut()
                .expect("installation journal present")
                .record_failure(OperationFailure::Other)?;
            self.journal.take();
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.operation == self.operation)
            {
                state.active = None;
                self.owner.updates.send_replace(Some(InstallationStatus {
                    operation: self.operation,
                    target: self.target.clone(),
                    phase: if self.cancellation.is_cancelled() {
                        InstallationPhase::Cancelled
                    } else {
                        InstallationPhase::Failed
                    },
                }));
            }
        }
        Ok(())
    }

    fn release(&mut self, phase: InstallationPhase) {
        if self.journal.take().is_none() {
            return;
        }
        let mut state = self
            .owner
            .state
            .lock()
            .expect("installation owner lock poisoned");
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.operation == self.operation)
        {
            state.active = None;
            self.owner.updates.send_replace(Some(InstallationStatus {
                operation: self.operation,
                target: self.target.clone(),
                phase,
            }));
        }
    }
}

impl Drop for InstallationPermit {
    fn drop(&mut self) {
        if self.journal.is_some() {
            let phase = if self.cancellation.is_cancelled() {
                InstallationPhase::Cancelled
            } else {
                InstallationPhase::Interrupted
            };
            self.release(phase);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: InstallComponentKind) -> InstallationTarget {
        InstallationTarget {
            kind,
            service: Some(AiService::Tts),
            module_id: "voice_module".into(),
            revision: "ab".repeat(32),
            component: "worker".into(),
        }
    }

    fn fixture() -> (PathBuf, Arc<InstallationOwner>) {
        let root = std::env::temp_dir().join(format!("install-owner-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let owner = InstallationOwner::new(root.join("AIData"));
        (root, owner)
    }

    #[test]
    fn native_and_python_share_one_admission_and_journal() {
        let (root, owner) = fixture();
        let first = owner.begin(target(InstallComponentKind::Package)).unwrap();
        assert!(matches!(
            owner.begin(target(InstallComponentKind::Bundle)),
            Err(InstallationError::Busy)
        ));
        let operation = first.operation();
        assert!(!owner.cancel(Uuid::new_v4()));
        first.complete().unwrap();
        let record = owner.current().unwrap();
        assert_eq!(record.operation, operation);
        assert_eq!(record.phase, InstallationPhase::Completed);
        let second = owner.begin(target(InstallComponentKind::Bundle)).unwrap();
        second.complete().unwrap();
        assert_eq!(
            InstallOperation::inspect_all(&root.join("AIData"))
                .unwrap()
                .len(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_is_observed_and_does_not_claim_success() {
        let (root, owner) = fixture();
        let permit = owner.begin(target(InstallComponentKind::Package)).unwrap();
        let op = permit.operation();
        let mut observer = owner.subscribe();
        assert!(owner.cancel(op));
        assert!(permit.cancellation().is_cancelled());
        assert_eq!(
            observer.borrow_and_update().as_ref().unwrap().phase,
            InstallationPhase::CancellationRequested
        );
        assert!(matches!(
            permit.complete(),
            Err(InstallationError::Stopping)
        ));
        assert_eq!(owner.current().unwrap().phase, InstallationPhase::Cancelled);
        owner.shutdown();
        assert!(matches!(
            owner.begin(target(InstallComponentKind::Bundle)),
            Err(InstallationError::Stopping)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancellation_keeps_admission_until_executor_exits() {
        let (root, owner) = fixture();
        let permit = owner.begin(target(InstallComponentKind::Package)).unwrap();
        let operation = permit.operation();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (leave, release) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = entered.send(());
            permit.cancellation().cancelled().await;
            let _ = release.await;
            drop(permit);
        });
        ready.await.unwrap();
        assert!(owner.cancel(operation));
        assert!(matches!(
            owner.begin(target(InstallComponentKind::Bundle)),
            Err(InstallationError::Busy)
        ));
        leave.send(()).unwrap();
        task.await.unwrap();
        assert_eq!(owner.current().unwrap().phase, InstallationPhase::Cancelled);
        owner
            .begin(target(InstallComponentKind::Bundle))
            .unwrap()
            .complete()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_components_are_rejected_without_creating_journals() {
        let (root, owner) = fixture();
        let mut selection = target(InstallComponentKind::Bundle);
        selection.revision = "not-a-revision".into();
        assert!(matches!(
            owner.begin(selection),
            Err(InstallationError::InvalidTarget)
        ));
        assert!(!root.join("AIData").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_or_dropped_operation_does_not_publish_ready() {
        let (root, owner) = fixture();
        owner
            .begin(target(InstallComponentKind::Package))
            .unwrap()
            .fail()
            .unwrap();
        assert_eq!(owner.current().unwrap().phase, InstallationPhase::Failed);
        {
            let _lease = owner.begin(target(InstallComponentKind::Bundle)).unwrap();
        }
        assert_eq!(
            owner.current().unwrap().phase,
            InstallationPhase::Interrupted
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
