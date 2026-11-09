//! Out-of-process activation handoff for a preverified raw application executable.
//!
//! The helper has a fixed on-disk identity and accepts only a parent process
//! identity. It never accepts an arbitrary target path, package path or URL.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    process::{self, Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::activation::{
    ActivationError, RecoveryOutcome, SingleFileActivator, prepared_target_name,
};
use super::reboot_guard;
use crate::portable::PortableDataRoot;

pub const HELPER_COMMAND: &str = "--internal-apply-update";
pub const RECOVER_COMMAND: &str = "--recover-interrupted-update";
const AUTO_RECOVER_COMMAND: &str = "--auto-recover-interrupted-update";
const RESULT_NAME: &str = "last-activation.json";
const HANDOFF_NAME: &str = "handoff.json";
const READY_NAME: &str = "readiness.json";
pub const READY_NONCE_ENV: &str = "NEUROMITA_UPDATE_READY_NONCE";
const READY_WAIT: Duration = Duration::from_secs(30);

#[cfg(windows)]
const HELPER_NAME: &str = "neuromita-updater.exe";
#[cfg(not(windows))]
const HELPER_NAME: &str = "neuromita-updater";

#[derive(Debug, Error)]
pub enum HandoffError {
    #[error("the updater helper must be a regular file beside this installation")]
    UnsafeHelper,
    #[error("only an installed application process may initiate its own update")]
    WrongCaller,
    #[error("the updater has no prepared transaction")]
    NotPrepared,
    #[error("the installed Windows recovery startup guard is not registered")]
    MissingRecoveryGuard,
    #[error("updater handoff failed ({handoff}); transaction cleanup also failed ({cleanup})")]
    HandoffCleanupFailed {
        handoff: Box<HandoffError>,
        cleanup: Box<HandoffError>,
    },
    #[error("the helper supports only the default installation-owned UserData root")]
    NonPortableDataRoot,
    #[error("invalid updater helper process arguments")]
    InvalidInvocation,
    #[error("native GUI startup readiness was not acknowledged by the expected process")]
    NotReady,
    #[error("native GUI startup timed out while awaiting explicit readiness")]
    ReadinessTimeout,
    #[error("native GUI update failed and the previous executable could not be restored: {0}")]
    RollbackFailed(ActivationError),
    #[error("previous executable was restored but relaunch failed: {0}")]
    RollbackRestartFailed(io::Error),
    #[error("parent process identity cannot be verified")]
    ParentIdentity,
    #[error("this platform has no supported parent identity waiting mechanism")]
    UnsupportedPlatform,
    #[error("recovery refused because an installed GUI or another updater helper is still running")]
    RecoveryBusy,
    #[error("recovery cannot prove that all installation processes are stopped: {0}")]
    RecoveryInspection(String),
    #[error("activation failed: {0}")]
    Activation(#[from] ActivationError),
    #[error("updater handoff I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("unable to write updater status: {0}")]
    Report(#[from] serde_json::Error),
}

#[derive(Serialize)]
struct ActivationReport<'a> {
    state: &'a str,
    target: &'a str,
    details: &'a str,
}

#[derive(Serialize, Deserialize)]
struct HandoffIntent {
    parent_pid: u32,
    parent_identity: u64,
    nonce: Uuid,
    target: String,
}

#[derive(Serialize, Deserialize)]
struct ReadyReceipt {
    nonce: Uuid,
    pid: u32,
    process_identity: u64,
}

/// Read-only preflight, invoked before staging and again when scheduling the helper.
/// Refuses development binaries, custom user-data directories and missing helpers.
pub fn validate_handoff_environment(activator: &SingleFileActivator) -> Result<(), HandoffError> {
    if fs::canonicalize(std::env::current_exe()?)? != activator.target_path() {
        return Err(HandoffError::WrongCaller);
    }
    let default_data = activator.install_root().join("UserData");
    let default_metadata = fs::symlink_metadata(&default_data)?;
    if !default_metadata.is_dir()
        || default_metadata.file_type().is_symlink()
        || fs::canonicalize(activator.data_root())? != fs::canonicalize(default_data)?
    {
        return Err(HandoffError::NonPortableDataRoot);
    }
    let helper = activator.install_root().join(HELPER_NAME);
    require_regular_file(&helper)?;
    if fs::canonicalize(&helper)? == activator.target_path() {
        return Err(HandoffError::UnsafeHelper);
    }
    Ok(())
}

/// Launches a separate installed helper and returns after the child has started.
/// The caller must subsequently perform its normal graceful shutdown; the
/// helper independently waits for *this exact process identity* to exit.
/// The transaction must have been prepared by `SingleFileActivator::prepare`.
pub fn schedule_after_shutdown(activator: &SingleFileActivator) -> Result<u32, HandoffError> {
    validate_handoff_environment(activator)?;
    let expected = if cfg!(windows) {
        "neuromita-gui.exe"
    } else {
        "neuromita-gui"
    };
    if activator.target_path().file_name() != Some(OsStr::new(expected)) {
        return Err(HandoffError::InvalidInvocation);
    }
    if activator.recover()? != RecoveryOutcome::Prepared {
        return Err(HandoffError::NotPrepared);
    }
    let helper = activator.install_root().join(HELPER_NAME);
    let identity = current_process_identity()?;
    let intent = HandoffIntent {
        parent_pid: process::id(),
        parent_identity: identity,
        nonce: Uuid::new_v4(),
        target: activator
            .target_path()
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or(HandoffError::UnsafeHelper)?
            .to_owned(),
    };
    let intent_path = activator
        .install_root()
        .join(".neuromita-update")
        .join(HANDOFF_NAME);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&intent_path)?;
    let record = (|| -> Result<(), HandoffError> {
        file.write_all(&serde_json::to_vec(&intent)?)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);
    if let Err(error) = record {
        return Err(abort_failed_schedule(
            activator,
            &helper,
            &intent_path,
            false,
            error,
        ));
    }
    let registered = match reboot_guard::arm(&helper) {
        Ok(registered) => registered,
        Err(error) => {
            return Err(abort_failed_schedule(
                activator,
                &helper,
                &intent_path,
                false,
                error.into(),
            ));
        }
    };
    if let Err(error) = activator.arm_health_check(intent.nonce) {
        return Err(abort_failed_schedule(
            activator,
            &helper,
            &intent_path,
            registered,
            error.into(),
        ));
    }
    let child = Command::new(&helper)
        .arg(HELPER_COMMAND)
        .arg(process::id().to_string())
        .arg(identity.to_string())
        .arg(intent.nonce.to_string())
        .current_dir(activator.install_root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(child) => Ok(child.id()),
        Err(error) => Err(abort_failed_schedule(
            activator,
            &helper,
            &intent_path,
            registered,
            error.into(),
        )),
    }
}

fn abort_failed_schedule(
    activator: &SingleFileActivator,
    helper: &Path,
    intent_path: &Path,
    guard_armed: bool,
    handoff: HandoffError,
) -> HandoffError {
    let cleanup = (|| -> Result<(), HandoffError> {
        match fs::symlink_metadata(intent_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                fs::remove_file(intent_path)?;
            }
            Ok(_) => return Err(HandoffError::UnsafeHelper),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        activator.discard_prepared()?;
        if guard_armed {
            reboot_guard::disarm(helper)?;
        }
        Ok(())
    })();
    match cleanup {
        Ok(()) => handoff,
        Err(cleanup) => HandoffError::HandoffCleanupFailed {
            handoff: Box::new(handoff),
            cleanup: Box::new(cleanup),
        },
    }
}

/// Entrypoint of the separately installed `neuromita-updater` binary.
/// The installation root is derived exclusively from the helper's own path.
pub fn run_helper(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<(), HandoffError> {
    let arguments = args
        .into_iter()
        .map(|part| part.as_ref().to_owned())
        .collect::<Vec<_>>();
    if arguments.as_slice() == [OsStr::new(RECOVER_COMMAND)] {
        return recover_stopped_installation();
    }
    if arguments.as_slice() == [OsStr::new(AUTO_RECOVER_COMMAND)] {
        return auto_recover_on_login();
    }
    let [command, pid, identity, nonce] = arguments.as_slice() else {
        return Err(HandoffError::InvalidInvocation);
    };
    if command != HELPER_COMMAND {
        return Err(HandoffError::InvalidInvocation);
    }
    let pid = pid
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 1 && *value != process::id())
        .ok_or(HandoffError::InvalidInvocation)?;
    let identity = identity
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0)
        .ok_or(HandoffError::InvalidInvocation)?;
    let nonce = nonce
        .to_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(HandoffError::InvalidInvocation)?;
    let executable = std::env::current_exe()?;
    if executable.file_name() != Some(OsStr::new(HELPER_NAME)) {
        return Err(HandoffError::UnsafeHelper);
    }
    require_regular_file(&executable)?;
    if !reboot_guard::is_armed(&executable)? {
        return Err(HandoffError::MissingRecoveryGuard);
    }
    let install_root = executable.parent().ok_or(HandoffError::UnsafeHelper)?;
    let target_name = prepared_target_name(install_root)?;
    let intent_path = install_root.join(".neuromita-update").join(HANDOFF_NAME);
    require_regular_file(&intent_path)?;
    let intent_bytes = fs::read(&intent_path)?;
    if intent_bytes.len() > 1024 {
        return Err(HandoffError::InvalidInvocation);
    }
    let intent: HandoffIntent = serde_json::from_slice(&intent_bytes)?;
    if intent.parent_pid != pid
        || intent.parent_identity != identity
        || intent.nonce != nonce
        || intent.target != target_name
    {
        return Err(HandoffError::InvalidInvocation);
    }
    wait_until_parent_exits(pid, identity)?;
    fs::remove_file(&intent_path)?;
    let data_directory = install_root.join("UserData");
    let metadata = fs::symlink_metadata(&data_directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(HandoffError::NonPortableDataRoot);
    }
    let data_root = PortableDataRoot::from_executable(&executable, Some(&data_directory), None)?;
    let activator = SingleFileActivator::new(install_root, &data_root, &target_name)?;
    let result = (|| -> Result<&'static str, HandoffError> {
        activator.verify_handoff_nonce(nonce)?;
        if !target_name.starts_with("neuromita-gui") {
            return Err(HandoffError::InvalidInvocation);
        }
        let resolved = activator.apply_or_resume()?;
        if resolved != RecoveryOutcome::AwaitingHealth {
            return Err(HandoffError::NotPrepared);
        }
        match restart_and_verify(install_root, &activator, nonce) {
            Ok(()) => Ok("updated-and-restarted"),
            Err(error) => {
                activator
                    .rollback_health_check(nonce)
                    .map_err(HandoffError::RollbackFailed)?;
                Command::new(activator.target_path())
                    .current_dir(install_root)
                    .env_remove(READY_NONCE_ENV)
                    .env_remove("NEUROMITA_DATA_DIR")
                    .spawn()
                    .map_err(HandoffError::RollbackRestartFailed)?;
                Err(error)
            }
        }
    })();
    let details = result.as_ref().err().map(ToString::to_string);
    let outcome = match &result {
        Ok(state) => *state,
        Err(HandoffError::ReadinessTimeout) => "rolled-back-after-readiness-timeout",
        Err(HandoffError::NotReady) => "rolled-back-after-startup-failure",
        Err(HandoffError::RollbackFailed(_)) => "rollback-failed",
        Err(HandoffError::RollbackRestartFailed(_)) => "rollback-restored-relaunch-failed",
        Err(_) => "failed",
    };
    write_report(
        &data_root,
        &target_name,
        outcome,
        details.as_deref().unwrap_or(""),
    )?;
    if matches!(activator.recover(), Ok(RecoveryOutcome::NoTransaction)) {
        reboot_guard::disarm(&executable)?;
    }
    result.map(|_| ())
}

/// Windows-only per-user login recovery for a previously consented native update.
/// It never installs, restarts a GUI or modifies any transaction lacking its
/// registered, installation-scoped startup command.
fn auto_recover_on_login() -> Result<(), HandoffError> {
    #[cfg(not(windows))]
    {
        return Err(HandoffError::UnsupportedPlatform);
    }
    #[cfg(windows)]
    {
        let helper = std::env::current_exe()?;
        if helper.file_name() != Some(OsStr::new(HELPER_NAME)) {
            return Err(HandoffError::UnsafeHelper);
        }
        require_regular_file(&helper)?;
        if !reboot_guard::is_armed(&helper)? {
            return Err(HandoffError::MissingRecoveryGuard);
        }
        let install = helper.parent().ok_or(HandoffError::UnsafeHelper)?;
        match fs::symlink_metadata(install.join(".neuromita-update")) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // The normal helper may already have finished successfully.
                reboot_guard::disarm(&helper)?;
                Ok(())
            }
            Err(error) => Err(error.into()),
            Ok(_) => recover_stopped_installation(),
        }
    }
}

/// Explicit offline recovery after a helper crash or reboot. The installation
/// root and target are derived from the real helper executable, never CLI paths.
/// This command NEVER resumes installation, launches a GUI or accepts a nonce.
fn recover_stopped_installation() -> Result<(), HandoffError> {
    let executable = std::env::current_exe()?;
    if executable.file_name() != Some(OsStr::new(HELPER_NAME)) {
        return Err(HandoffError::UnsafeHelper);
    }
    require_regular_file(&executable)?;
    let install = executable.parent().ok_or(HandoffError::UnsafeHelper)?;
    let target_name = prepared_target_name(install)?;
    let required_name = if cfg!(windows) {
        "neuromita-gui.exe"
    } else {
        "neuromita-gui"
    };
    if target_name != required_name {
        return Err(HandoffError::InvalidInvocation);
    }
    let data_path = install.join("UserData");
    let metadata = fs::symlink_metadata(&data_path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(HandoffError::NonPortableDataRoot);
    }
    let data_root = PortableDataRoot::from_executable(&executable, Some(&data_path), None)?;
    let activator = SingleFileActivator::new(install, &data_root, required_name)?;
    ensure_no_active_installation_processes(&activator, &executable)?;
    let recovered = activator.recover_interrupted_health_check()?;
    if recovered != RecoveryOutcome::PreviousRestored {
        return Err(HandoffError::NotPrepared);
    }
    write_report(&data_root, required_name, "interrupted-update-restored", "")?;
    reboot_guard::disarm(&executable)?;
    eprintln!("Previous NeuroMita executable restored. UserData was not changed.");
    Ok(())
}

fn ensure_no_active_installation_processes(
    activator: &SingleFileActivator,
    helper: &Path,
) -> Result<(), HandoffError> {
    for (path, allow_self) in [
        (activator.target_path(), false),
        (helper.to_path_buf(), true),
    ] {
        let scan = crate::launcher::scan_install(&path)
            .map_err(|error| HandoffError::RecoveryInspection(error.to_string()))?;
        if scan
            .matching
            .iter()
            .any(|identity| !allow_self || identity.pid != process::id())
        {
            return Err(HandoffError::RecoveryBusy);
        }
        #[cfg(target_os = "linux")]
        for pid in scan.inaccessible {
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or(HandoffError::UnsafeHelper)?;
            let expected = &name.as_bytes()[..name.len().min(15)];
            let actual = fs::read(format!("/proc/{pid}/comm"))
                .map_err(|error| HandoffError::RecoveryInspection(error.to_string()))?;
            if actual.strip_suffix(b"\n").unwrap_or(&actual) == expected {
                return Err(HandoffError::RecoveryBusy);
            }
        }
    }
    #[cfg(target_os = "linux")]
    ensure_no_running_linux_inodes(activator)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_no_running_linux_inodes(activator: &SingleFileActivator) -> Result<(), HandoffError> {
    use std::os::unix::fs::MetadataExt;

    let workspace = activator.install_root().join(".neuromita-update");
    let mut tracked = Vec::new();
    for path in [
        activator.target_path(),
        workspace.join("candidate"),
        workspace.join("backup"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                tracked.push((meta.dev(), meta.ino()));
            }
            Ok(_) => return Err(HandoffError::UnsafeHelper),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        match fs::metadata(format!("/proc/{pid}/exe")) {
            Ok(meta) if tracked.contains(&(meta.dev(), meta.ino())) => {
                return Err(HandoffError::RecoveryBusy);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
                    .map_err(|error| HandoffError::RecoveryInspection(error.to_string()))?;
                if comm.trim_end() == "neuromita-gui" {
                    return Err(HandoffError::RecoveryBusy);
                }
            }
            Err(error) => return Err(HandoffError::RecoveryInspection(error.to_string())),
        }
    }
    Ok(())
}

/// Called only after Slint has entered its event loop and the window is visible.
/// The nonce comes from the private helper-started child environment and the
/// durable journal is checked before any receipt is written.
pub fn acknowledge_gui_ready() -> Result<bool, HandoffError> {
    let Some(raw) = std::env::var_os(READY_NONCE_ENV) else {
        return Ok(false);
    };
    let nonce = raw
        .to_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(HandoffError::InvalidInvocation)?;
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let expected = if cfg!(windows) {
        "neuromita-gui.exe"
    } else {
        "neuromita-gui"
    };
    if executable.file_name() != Some(OsStr::new(expected)) {
        return Err(HandoffError::WrongCaller);
    }
    let install_root = executable.parent().ok_or(HandoffError::WrongCaller)?;
    let data_path = install_root.join("UserData");
    let meta = fs::symlink_metadata(&data_path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(HandoffError::NonPortableDataRoot);
    }
    let data_root = PortableDataRoot::from_executable(&executable, Some(&data_path), None)?;
    let activator = SingleFileActivator::new(install_root, &data_root, expected)?;
    activator.verify_health_nonce(nonce)?;
    let receipt = ReadyReceipt {
        nonce,
        pid: process::id(),
        process_identity: current_process_identity()?,
    };
    let path = install_root.join(".neuromita-update").join(READY_NAME);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    file.write_all(&serde_json::to_vec(&receipt)?)?;
    file.sync_all()?;
    Ok(true)
}

/// A receipt is accepted only from the exact new child instance and matching
/// transaction nonce. A process merely existing is not a success signal.
fn wait_for_readiness(
    child: &mut Child,
    identity: u64,
    path: &Path,
    nonce: Uuid,
    timeout: Duration,
) -> Result<(), HandoffError> {
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Err(HandoffError::NotReady);
        }
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 {
                    return Err(HandoffError::NotReady);
                }
                let receipt: ReadyReceipt = serde_json::from_slice(&fs::read(path)?)?;
                if receipt.nonce != nonce
                    || receipt.pid != child.id()
                    || receipt.process_identity != identity
                {
                    return Err(HandoffError::NotReady);
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if started.elapsed() >= timeout {
            return Err(HandoffError::ReadinessTimeout);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn restart_and_verify(
    install_root: &Path,
    activator: &SingleFileActivator,
    nonce: Uuid,
) -> Result<(), HandoffError> {
    let mut child = Command::new(activator.target_path())
        .current_dir(install_root)
        .env_remove("NEUROMITA_DATA_DIR")
        .env(READY_NONCE_ENV, nonce.to_string())
        .spawn()?;
    let child_identity = match child_process_identity(child.id()) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let result = wait_for_readiness(
        &mut child,
        child_identity,
        &install_root.join(".neuromita-update").join(READY_NAME),
        nonce,
        READY_WAIT,
    );
    if result.is_err() {
        if child.try_wait()?.is_none() {
            if let Err(error) = child.kill()
                && child.try_wait()?.is_none()
            {
                return Err(error.into());
            }
        }
        child.wait()?;
    } else if child.try_wait()?.is_some() {
        return Err(HandoffError::NotReady);
    } else {
        activator.complete_health_check(nonce)?;
    }
    result
}

#[cfg(target_os = "linux")]
fn child_process_identity(pid: u32) -> Result<u64, HandoffError> {
    process_identity(pid)?
        .map(|(stamp, _)| stamp)
        .ok_or(HandoffError::ParentIdentity)
}

#[cfg(windows)]
fn child_process_identity(pid: u32) -> Result<u64, HandoffError> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let result = process_creation_time(handle, GetProcessTimes);
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(not(any(target_os = "linux", windows)))]
fn child_process_identity(_pid: u32) -> Result<u64, HandoffError> {
    Err(HandoffError::UnsupportedPlatform)
}

fn require_regular_file(path: &Path) -> Result<(), HandoffError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(HandoffError::UnsafeHelper);
    }
    Ok(())
}

fn write_report(
    root: &PortableDataRoot,
    target: &str,
    state: &str,
    details: &str,
) -> Result<(), HandoffError> {
    let directory = root.path().join("updates");
    fs::create_dir_all(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(HandoffError::UnsafeHelper);
    }
    let pending = directory.join(format!(".activation-{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)?;
    let report = serde_json::to_vec(&ActivationReport {
        state,
        target,
        details,
    })?;
    file.write_all(&report)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&pending, directory.join(RESULT_NAME)) {
        let _ = fs::remove_file(&pending);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_identity(pid: u32) -> Result<Option<(u64, char)>, HandoffError> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let (_, fields) = stat.rsplit_once(") ").ok_or(HandoffError::ParentIdentity)?;
    let mut parts = fields.split_whitespace();
    let state = parts
        .next()
        .and_then(|field| field.chars().next())
        .ok_or(HandoffError::ParentIdentity)?;
    let starttime = parts
        .nth(18)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(HandoffError::ParentIdentity)?;
    Ok(Some((starttime, state)))
}

#[cfg(target_os = "linux")]
fn current_process_identity() -> Result<u64, HandoffError> {
    process_identity(process::id())?
        .map(|(identity, _)| identity)
        .ok_or(HandoffError::ParentIdentity)
}

#[cfg(target_os = "linux")]
fn wait_until_parent_exits(pid: u32, expected: u64) -> Result<(), HandoffError> {
    loop {
        match process_identity(pid)? {
            None | Some((_, 'Z' | 'X')) => return Ok(()),
            Some((observed, _)) if observed != expected => return Ok(()),
            Some(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(windows)]
fn current_process_identity() -> Result<u64, HandoffError> {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    process_creation_time(unsafe { GetCurrentProcess() }, GetProcessTimes)
}

#[cfg(windows)]
fn process_creation_time(
    handle: windows_sys::Win32::Foundation::HANDLE,
    get_times: unsafe extern "system" fn(
        windows_sys::Win32::Foundation::HANDLE,
        *mut windows_sys::Win32::Foundation::FILETIME,
        *mut windows_sys::Win32::Foundation::FILETIME,
        *mut windows_sys::Win32::Foundation::FILETIME,
        *mut windows_sys::Win32::Foundation::FILETIME,
    ) -> i32,
) -> Result<u64, HandoffError> {
    use windows_sys::Win32::Foundation::FILETIME;
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { get_times(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

#[cfg(windows)]
fn wait_until_parent_exits(pid: u32, expected: u64) -> Result<(), HandoffError> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0},
        Storage::FileSystem::SYNCHRONIZE,
        System::Threading::{
            GetProcessTimes, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            WaitForSingleObject,
        },
    };
    let handle = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err(error.into());
    }
    let result = (|| {
        let observed = process_creation_time(handle, GetProcessTimes)?;
        if observed != expected {
            return Ok(());
        }
        let code = unsafe { WaitForSingleObject(handle, INFINITE) };
        if code != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    })();
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(not(any(target_os = "linux", windows)))]
fn current_process_identity() -> Result<u64, HandoffError> {
    Err(HandoffError::UnsupportedPlatform)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn wait_until_parent_exits(_pid: u32, _expected: u64) -> Result<(), HandoffError> {
    Err(HandoffError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_helper_invocations() {
        for args in [
            vec![],
            vec!["--internal-apply-update"],
            vec!["--other", "1", "2"],
            vec!["--internal-apply-update", "0", "2"],
            vec!["--internal-apply-update", "1", "2"],
            vec!["--internal-apply-update", "99", "0"],
            vec!["--internal-apply-update", "x", "2"],
            vec!["--internal-apply-update", "99", "invalid", "bad"],
            vec!["--recover-interrupted-update", "unexpected"],
        ] {
            assert!(matches!(
                run_helper(args),
                Err(HandoffError::InvalidInvocation)
            ));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_identity_prevents_pid_reuse_confusion() {
        let own = process_identity(process::id()).unwrap().unwrap();
        assert_ne!(own.0, 0);
        wait_until_parent_exits(process::id(), own.0.wrapping_add(1)).unwrap();
        assert_eq!(current_process_identity().unwrap(), own.0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_waits_until_particular_child_exits() {
        let mut child = Command::new("sleep").arg("0.3").spawn().unwrap();
        let identity = process_identity(child.id()).unwrap().unwrap().0;
        let started = std::time::Instant::now();
        wait_until_parent_exits(child.id(), identity).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(200));
        child.wait().unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn automatic_login_recovery_is_not_claimed_on_unsupported_platforms() {
        assert!(matches!(
            run_helper([AUTO_RECOVER_COMMAND]),
            Err(HandoffError::UnsupportedPlatform)
        ));
    }

    #[test]
    fn status_is_persisted_only_inside_portable_user_data() {
        let base =
            std::env::temp_dir().join(format!("neuromita-updater-report-{}", Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        let root =
            PortableDataRoot::from_executable(&base.join("neuromita-gui"), Some(&base), None)
                .unwrap();
        write_report(&root, "neuromita-gui.exe", "failed", "simulated").unwrap();
        let contents = fs::read_to_string(base.join("updates").join(RESULT_NAME)).unwrap();
        assert!(contents.contains("\"state\":\"failed\""));
        assert!(contents.contains("simulated"));
        write_report(&root, "neuromita-gui.exe", "updated-and-restarted", "").unwrap();
        let contents = fs::read_to_string(base.join("updates").join(RESULT_NAME)).unwrap();
        assert!(contents.contains("updated-and-restarted"));
        assert!(!contents.contains("simulated"));
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    #[ignore]
    fn readiness_probe() {
        let Ok(file) = std::env::var("NEUROMITA_TEST_READY_FILE") else {
            return;
        };
        let mode = std::env::var("NEUROMITA_TEST_READY_MODE").unwrap();
        let nonce = Uuid::parse_str(&std::env::var("NEUROMITA_TEST_READY_NONCE").unwrap()).unwrap();
        thread::sleep(Duration::from_millis(120));
        if mode == "valid" || mode == "mismatch" {
            let receipt = ReadyReceipt {
                nonce: if mode == "valid" {
                    nonce
                } else {
                    Uuid::new_v4()
                },
                pid: process::id(),
                process_identity: current_process_identity().unwrap(),
            };
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(file)
                .unwrap();
            output
                .write_all(&serde_json::to_vec(&receipt).unwrap())
                .unwrap();
            output.sync_all().unwrap();
        }
        if mode != "exit" {
            thread::sleep(Duration::from_millis(850));
        }
    }

    #[test]
    fn real_child_readiness_requires_exact_pid_creation_and_nonce() {
        for (mode, timeout, expected) in [
            ("valid", Duration::from_millis(700), "success"),
            ("mismatch", Duration::from_millis(700), "not-ready"),
            ("none", Duration::from_millis(250), "timeout"),
            ("exit", Duration::from_millis(700), "not-ready"),
        ] {
            let base = std::env::temp_dir().join(format!("neuromita-health-{}", Uuid::new_v4()));
            fs::create_dir_all(&base).unwrap();
            let path = base.join("receipt.json");
            let nonce = Uuid::new_v4();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "updater::handoff::tests::readiness_probe",
                    "--ignored",
                ])
                .env("NEUROMITA_TEST_READY_FILE", &path)
                .env("NEUROMITA_TEST_READY_MODE", mode)
                .env("NEUROMITA_TEST_READY_NONCE", nonce.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let identity = child_process_identity(child.id()).unwrap();
            let result = wait_for_readiness(&mut child, identity, &path, nonce, timeout);
            match expected {
                "success" => assert!(result.is_ok(), "{result:?}"),
                "not-ready" => assert!(matches!(result, Err(HandoffError::NotReady))),
                "timeout" => assert!(matches!(result, Err(HandoffError::ReadinessTimeout))),
                _ => unreachable!(),
            }
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
            }
            child.wait().unwrap();
            fs::remove_dir_all(base).unwrap();
        }
    }
}
