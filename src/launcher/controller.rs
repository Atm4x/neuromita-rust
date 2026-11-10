use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use super::{LauncherApi, LauncherConfig, OwnedProcessState, SessionStatus};

#[derive(Debug, Clone, Copy)]
pub enum LauncherCommand {
    Toggle,
    OpenLocation,
}

#[derive(Debug, Clone)]
pub struct LauncherUpdate {
    pub state: OwnedProcessState,
    pub installed: bool,
    pub external_running: bool,
    pub error: Option<String>,
    pub busy: bool,
}

fn has_external_game(status: &SessionStatus) -> bool {
    if matches!(status.owned, OwnedProcessState::Running { .. }) {
        return false;
    }
    let owned_pid = status.owned.pid();
    status
        .processes
        .matching
        .iter()
        .any(|identity| Some(identity.pid) != owned_pid)
}

pub struct LauncherController {
    sender: Option<mpsc::SyncSender<LauncherCommand>>,
    stopped: Arc<AtomicBool>,
    toggle_pending: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LauncherController {
    pub fn start(
        config: LauncherConfig,
        publish: impl Fn(LauncherUpdate) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receive) = mpsc::sync_channel(16);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = stopped.clone();
        let toggle_pending = Arc::new(AtomicBool::new(false));
        let worker_toggle_pending = toggle_pending.clone();
        let worker = thread::Builder::new()
            .name("launcher-owner".into())
            .spawn(move || {
                let mut session = LauncherApi::session(config);
                let mut previous = None;
                let mut previous_installation = None;
                let mut previous_external = None;
                let mut external_running = false;
                let mut last_scan = Instant::now() - Duration::from_secs(2);
                while !worker_stop.load(Ordering::Acquire) {
                    let command = receive.recv_timeout(Duration::from_millis(250));
                    let force_scan = matches!(command, Ok(LauncherCommand::Toggle));
                    if force_scan {
                        publish(LauncherUpdate {
                            state: session.owned_state().unwrap_or(OwnedProcessState::Idle),
                            installed: session.service().install().is_ok(),
                            external_running,
                            error: None,
                            busy: true,
                        });
                    }
                    let result = match command {
                        Ok(LauncherCommand::Toggle) => match session.owned_state() {
                            Ok(OwnedProcessState::Running { .. }) => session.stop().map(|_| ()),
                            Ok(_) => session.start().map(|_| ()),
                            Err(error) => Err(error),
                        },
                        Ok(LauncherCommand::OpenLocation) => session.open_location(),
                        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let mut error = result.err().map(|error| error.to_string());
                    let installed = session.service().install().is_ok();
                    if !installed {
                        external_running = false;
                    } else if force_scan || last_scan.elapsed() >= Duration::from_secs(1) {
                        last_scan = Instant::now();
                        match session.status() {
                            Ok(status) => external_running = has_external_game(&status),
                            Err(scan_error) => {
                                external_running = false;
                                if error.is_none() {
                                    error = Some(scan_error.to_string());
                                }
                            }
                        }
                    }
                    match session.owned_state() {
                        Ok(state) => {
                            if previous.as_ref() != Some(&state)
                                || error.is_some()
                                || previous_installation != Some(installed)
                                || previous_external != Some(external_running)
                                || force_scan
                            {
                                previous = Some(state.clone());
                                previous_installation = Some(installed);
                                previous_external = Some(external_running);
                                publish(LauncherUpdate {
                                    state,
                                    installed,
                                    external_running,
                                    error,
                                    busy: false,
                                });
                            }
                        }
                        Err(error) => publish(LauncherUpdate {
                            state: previous.clone().unwrap_or(OwnedProcessState::Idle),
                            installed,
                            external_running,
                            error: Some(error.to_string()),
                            busy: false,
                        }),
                    }
                    if force_scan {
                        worker_toggle_pending.store(false, Ordering::Release);
                    }
                }
                if let Err(error) = session.shutdown() {
                    publish(LauncherUpdate {
                        state: OwnedProcessState::Idle,
                        installed: session.service().install().is_ok(),
                        external_running: false,
                        error: Some(error.to_string()),
                        busy: false,
                    });
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            stopped,
            toggle_pending,
            worker: Some(worker),
        })
    }

    pub fn submit(&self, command: LauncherCommand) -> Result<(), String> {
        let sender = self.sender.as_ref().ok_or("launcher is stopped")?;
        let toggle = matches!(command, LauncherCommand::Toggle);
        if toggle && self.toggle_pending.swap(true, Ordering::AcqRel) {
            return Err("launcher operation is already in progress".into());
        }
        if let Err(error) = sender.try_send(command) {
            if toggle {
                self.toggle_pending.store(false, Ordering::Release);
            }
            return Err(error.to_string());
        }
        Ok(())
    }

    /// Disconnects admission and waits for owned process cleanup, after the UI loop exits.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.stopped.store(true, Ordering::Release);
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "launcher owner panicked".to_string())?;
        }
        Ok(())
    }
}

impl Drop for LauncherController {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.sender.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_toggle_is_rejected_during_inflight_operation() {
        let root = std::env::temp_dir().join(format!("absent-launcher-{}", uuid::Uuid::new_v4()));
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (resume_sender, resume_receiver) = mpsc::channel();
        let (updates_sender, updates_receiver) = mpsc::channel();
        let mut owner = LauncherController::start(LauncherConfig::new(root), move |update| {
            if update.busy {
                let _ = entered_sender.send(());
                let _ = resume_receiver.recv_timeout(Duration::from_secs(5));
            }
            let _ = updates_sender.send(update);
        })
        .unwrap();

        owner.submit(LauncherCommand::Toggle).unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            owner.submit(LauncherCommand::Toggle).unwrap_err(),
            "launcher operation is already in progress"
        );
        resume_sender.send(()).unwrap();
        assert!(
            updates_receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .busy
        );
        let finished = updates_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(!finished.busy);
        assert!(finished.error.is_some());
        owner.shutdown().unwrap();
    }

    #[test]
    fn background_errors_are_observed_and_stopped_owner_rejects_commands() {
        let root = std::env::temp_dir().join(format!("absent-launcher-{}", uuid::Uuid::new_v4()));
        let (send, receive) = mpsc::channel();
        let mut owner = LauncherController::start(LauncherConfig::new(root), move |update| {
            let _ = send.send(update);
        })
        .unwrap();
        owner.submit(LauncherCommand::Toggle).unwrap();
        let update = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(update.error.is_some());
        assert_eq!(update.state, OwnedProcessState::Idle);
        owner.shutdown().unwrap();
        assert!(owner.submit(LauncherCommand::Toggle).is_err());
    }
}
