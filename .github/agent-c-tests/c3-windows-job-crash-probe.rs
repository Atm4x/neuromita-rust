#[path = "../../src/ai_runtime/windows_uv_job.rs"]
mod windows_uv_job;

use std::{
    env, fs, io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};

fn main() -> io::Result<()> {
    let args: Vec<_> = env::args().collect();
    if args.get(1).is_some_and(|x| x == "--parent") {
        let ready = PathBuf::from(&args[2]);
        let child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 120"])
            .stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null())
            .spawn()?;
        let _job = windows_uv_job::WindowsUvJob::attach(child.as_raw_handle())?;
        fs::write(&ready, child.id().to_string())?;
        loop { thread::sleep(Duration::from_millis(100)); }
    }
    let root = env::temp_dir().join(format!("nm-c3-job-crash-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let ready = root.join("ready.txt");
    let mut parent = Command::new(env::current_exe()?)
        .arg("--parent").arg(&ready)
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    while !ready.exists() {
        assert!(start.elapsed() < Duration::from_secs(15), "parent did not attach job");
        assert!(parent.try_wait()?.is_none(), "parent failed before attaching job");
        thread::sleep(Duration::from_millis(25));
    }
    let pid: u32 = fs::read_to_string(&ready)?.parse().unwrap();
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    assert!(!handle.is_null(), "cannot open child process handle");
    let child = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
    assert_eq!(
        unsafe { WaitForSingleObject(child.as_raw_handle().cast(), 0) },
        WAIT_TIMEOUT, "child exited before owner crash"
    );
    parent.kill()?;
    parent.wait()?;
    assert_eq!(
        unsafe { WaitForSingleObject(child.as_raw_handle().cast(), 10_000) },
        WAIT_OBJECT_0,
        "surviving uv child was not killed by parent's Job Object closure"
    );
    println!("WINDOWS_JOB_KILLS_CHILD_AFTER_PARENT_CRASH=PASS");
    drop(child);
    fs::remove_dir_all(root)?;
    Ok(())
}
