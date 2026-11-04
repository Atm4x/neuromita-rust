use std::{sync::mpsc, thread, time::Duration};

use super::{LauncherApi, LauncherConfig, OwnedProcessState};

#[derive(Debug, Clone, Copy)]
pub enum LauncherCommand {
    Toggle,
    OpenLocation,
}

#[derive(Debug, Clone)]
pub struct LauncherUpdate {
    pub state: OwnedProcessState,
    pub error: Option<String>,
}

pub struct LauncherController {
    sender: Option<mpsc::SyncSender<LauncherCommand>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LauncherController {
    pub fn start(
        config: LauncherConfig,
        publish: impl Fn(LauncherUpdate) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receive) = mpsc::sync_channel(16);
        let worker = thread::Builder::new()
            .name("launcher-owner".into())
            .spawn(move || {
                let mut session = LauncherApi::session(config);
                let mut previous = None;
                loop {
                    let result = match receive.recv_timeout(Duration::from_millis(250)) {
                        Ok(LauncherCommand::Toggle) => match session.owned_state() {
                            Ok(OwnedProcessState::Running { .. }) => session.stop().map(|_| ()),
                            Ok(_) => session.start().map(|_| ()),
                            Err(error) => Err(error),
                        },
                        Ok(LauncherCommand::OpenLocation) => session.open_location(),
                        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let error = result.err().map(|error| error.to_string());
                    match session.owned_state() {
                        Ok(state) => {
                            if previous.as_ref() != Some(&state) || error.is_some() {
                                previous = Some(state.clone());
                                publish(LauncherUpdate { state, error });
                            }
                        }
                        Err(error) => publish(LauncherUpdate {
                            state: previous.clone().unwrap_or(OwnedProcessState::Idle),
                            error: Some(error.to_string()),
                        }),
                    }
                }
                if let Err(error) = session.shutdown() {
                    publish(LauncherUpdate {
                        state: OwnedProcessState::Idle,
                        error: Some(error.to_string()),
                    });
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    pub fn submit(&self, command: LauncherCommand) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or("launcher is stopped")?
            .try_send(command)
            .map_err(|error| error.to_string())
    }

    /// Disconnects admission and waits for owned process cleanup, after the UI loop exits.
    pub fn shutdown(&mut self) -> Result<(), String> {
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
        self.sender.take();
    }
}
