//! Out-of-process activation handoff for a preverified raw application executable.
//!
//! The helper has a fixed on-disk identity and accepts only a parent process
//! identity. It never accepts an arbitrary target path, package path or URL.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    process::{self, Command, Stdio},
};

#[cfg(target_os = "linux")]
use std::{thread, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::activation::{
    ActivationError, RecoveryOutcome, SingleFileActivator, prepared_target_name,
};
use crate::portable::PortableDataRoot;

pub const HELPER_COMMAND: &str = "--internal-apply-update";
const RESULT_NAME: &str = "last-activation.json";
const HANDOFF_NAME: &str = "handoff.json";

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
    #[error("the helper supports only the default installation-owned UserData root")]
    NonPortableDataRoot,
    #[error("invalid updater helper process arguments")]
    InvalidInvocation,
    #[error("parent process identity cannot be verified")]
    ParentIdentity,
    #[error("this platform has no supported parent identity waiting mechanism")]
    UnsupportedPlatform,
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

/// Launches a separate installed helper and returns after the child has started.
/// The caller must subsequently perform its normal graceful shutdown; the
/// helper independently waits for *this exact process identity* to exit.
/// The transaction must have been prepared by `SingleFileActivator::prepare`.
pub fn schedule_after_shutdown(activator: &SingleFileActivator) -> Result<u32, HandoffError> {
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
    if activator.recover()? != RecoveryOutcome::Prepared {
        return Err(HandoffError::NotPrepared);
    }
    let helper = activator.install_root().join(HELPER_NAME);
    require_regular_file(&helper)?;
    if fs::canonicalize(&helper)? == activator.target_path() {
        return Err(HandoffError::UnsafeHelper);
    }
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
    file.write_all(&serde_json::to_vec(&intent)?)?;
    file.sync_all()?;
    drop(file);
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
        Err(error) => {
            let _ = fs::remove_file(intent_path);
            Err(error.into())
        }
    }
}

/// Entrypoint of the separately installed `neuromita-updater` binary.
/// The installation root is derived exclusively from the helper's own path.
pub fn run_helper(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<(), HandoffError> {
    let arguments = args
        .into_iter()
        .map(|part| part.as_ref().to_owned())
        .collect::<Vec<_>>();
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
        let resolved = activator.apply_or_resume()?;
        if resolved != RecoveryOutcome::NewVersionVerified {
            return Err(HandoffError::NotPrepared);
        }
        if target_name.starts_with("neuromita-gui") {
            Command::new(activator.target_path())
                .current_dir(install_root)
                .env_remove("NEUROMITA_DATA_DIR")
                .spawn()?;
            Ok("updated-and-restarted")
        } else {
            Ok("updated-restart-skipped-cli")
        }
    })();
    let details = result.as_ref().err().map(ToString::to_string);
    let outcome = result.as_ref().copied().unwrap_or("failed");
    write_report(
        &data_root,
        &target_name,
        outcome,
        details.as_deref().unwrap_or(""),
    )?;
    result.map(|_| ())
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
}
