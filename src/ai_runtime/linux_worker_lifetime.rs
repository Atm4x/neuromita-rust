//! Linux parent-death ownership for directly spawned Python workers.
//!
//! A Rust file lease cannot survive an ungraceful process death. The kernel
//! therefore terminates the worker on its spawning thread's death. This only
//! covers the direct worker process; it does not authorize layer removal.

use std::{io, os::unix::process::CommandExt};

use tokio::process::Command;

unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
    fn getppid() -> i32;
}

const PR_SET_PDEATHSIG: i32 = 1;
const SIGKILL: i32 = 9;

pub(super) fn require_parent_death_kill(command: &mut Command) {
    let expected_parent = std::process::id() as i32;
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if prctl(
                PR_SET_PDEATHSIG,
                SIGKILL as std::ffi::c_ulong,
                0usize,
                0usize,
                0usize,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            if getppid() != expected_parent {
                return Err(io::Error::from_raw_os_error(32));
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        process::{Command as ProcessCommand, Stdio},
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    #[tokio::test(flavor = "current_thread")]
    async fn subprocess_owner_for_crash_test() {
        let Ok(marker) = std::env::var("NM_PDEATH_OWNER_MARKER") else {
            return;
        };
        let Some(python) = ["python3", "python"].iter().find(|python| {
            ProcessCommand::new(python)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        }) else {
            panic!("Python interpreter required for the owner crash regression");
        };
        let mut command = Command::new(python);
        require_parent_death_kill(&mut command);
        let mut child = command
            .args(["-c", "import time; time.sleep(60)"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        fs::write(marker, child.id().unwrap().to_string()).unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        let _ = child.kill().await;
    }

    #[test]
    fn sigkill_of_rust_owner_terminates_its_python_worker() {
        let root = std::env::temp_dir().join(format!("nm-pdeath-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let marker = root.join("child.pid");
        let mut owner = ProcessCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ai_runtime::linux_worker_lifetime::tests::subprocess_owner_for_crash_test",
            ])
            .env("NM_PDEATH_OWNER_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "owner did not launch Python");
            assert!(
                owner.try_wait().unwrap().is_none(),
                "owner exited before launch"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid: u32 = fs::read_to_string(&marker).unwrap().parse().unwrap();
        let proc_path = PathBuf::from(format!("/proc/{pid}/stat"));
        assert!(proc_path.exists(), "Python process was not running");
        owner.kill().unwrap();
        owner.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let still_running = fs::read_to_string(&proc_path)
                .ok()
                .and_then(|stat| stat.rsplit_once(") ").map(|(_, fields)| fields.to_owned()))
                .is_some_and(|fields| !matches!(fields.chars().next(), Some('Z' | 'X')));
            if !still_running {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "orphan Python worker survived owner SIGKILL"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        fs::remove_dir_all(root).unwrap();
    }
}
