//! Windows kernel job ownership for uv installers and gated Python workers.

use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    ptr, thread,
    time::{Duration, Instant},
};

use uuid::Uuid;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation, OpenJobObjectW,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND},
    System::{RemoteDesktop::ProcessIdToSessionId, Threading::GetCurrentProcessId},
};

const JOB_QUERY_ACCESS: u32 = 0x0004;

fn name_for(stage: Uuid) -> Vec<u16> {
    format!("Local\\NeuroMita-AI-uv-stage-{stage}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

pub(super) fn current_session() -> io::Result<u32> {
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(session)
}

pub(super) fn job_is_gone(stage: Uuid, expected_session: u32) -> io::Result<bool> {
    if current_session()? != expected_session {
        return Ok(false);
    }
    let name = name_for(stage);
    let handle = unsafe { OpenJobObjectW(JOB_QUERY_ACCESS, 0, name.as_ptr()) };
    if !handle.is_null() {
        drop(unsafe { OwnedHandle::from_raw_handle(handle.cast()) });
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
        Ok(true)
    } else {
        Err(error)
    }
}

pub(super) struct WindowsKillOnCloseJob {
    handle: OwnedHandle,
}

pub(super) type WindowsUvJob = WindowsKillOnCloseJob;

impl WindowsKillOnCloseJob {
    pub(super) fn attach(process: RawHandle, stage: Uuid) -> io::Result<Self> {
        Self::attach_named(process, name_for(stage))
    }

    pub(super) fn attach_worker(process: RawHandle, generation: Uuid) -> io::Result<Self> {
        let name = format!("Local\\NeuroMita-AI-python-worker-{generation}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        Self::attach_named(process, name)
    }

    fn attach_named(process: RawHandle, name: Vec<u16>) -> io::Result<Self> {
        // No descendant inherits the owning job handle. Nested-job rejection
        // is fatal to the caller: it must kill the still-gated child.

        let handle = unsafe { CreateJobObjectW(ptr::null(), name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        if io::Error::last_os_error().raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            drop(unsafe { OwnedHandle::from_raw_handle(handle.cast()) });
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "AI child job name collision",
            ));
        }
        let job = Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle.cast()) },
        };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.handle.as_raw_handle().cast(),
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe { AssignProcessToJobObject(job.handle.as_raw_handle().cast(), process.cast()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(super) fn terminate_and_wait(&self, timeout: Duration) -> io::Result<bool> {
        if unsafe { TerminateJobObject(self.handle.as_raw_handle().cast(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + timeout;
        loop {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if unsafe {
                QueryInformationJobObject(
                    self.handle.as_raw_handle().cast(),
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast(),
                    std::mem::size_of_val(&info) as u32,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if info.ActiveProcesses == 0 {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::windows::io::AsRawHandle, process::Command};

    #[test]
    fn termination_waits_until_child_is_dead() {
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .spawn()
            .unwrap();
        let job = WindowsUvJob::attach(child.as_raw_handle(), Uuid::new_v4()).unwrap();
        assert!(job.terminate_and_wait(Duration::from_secs(10)).unwrap());
        assert!(child.wait().unwrap().code().is_some());
    }
}
