use std::{
    env,
    fs::{File, OpenOptions},
    io,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn owner_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let base = env::temp_dir().join("neuromita-c3-windows-lock-smoke");
    let lock = base.join("owner.lock");
    let ready = base.join("ready");
    let release = base.join("release");

    match args.get(1).map(String::as_str) {
        Some("--child") => {
            std::fs::write(&ready, "child running")?;
            while !release.exists() {
                thread::sleep(Duration::from_millis(25));
            }
        }
        Some("--parent") => {
            let owner = owner_lock(&lock)?;
            owner.lock()?;
            let _child = Command::new(env::current_exe()?)
                .arg("--child")
                .stdin(Stdio::from(owner.try_clone()?))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            loop {
                thread::sleep(Duration::from_millis(100));
            }
        }
        _ => {
            std::fs::create_dir_all(&base)?;
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_file(&release);
            let _ = std::fs::remove_file(&lock);
            let mut parent = Command::new(env::current_exe()?)
                .arg("--parent")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            let started = Instant::now();
            while !ready.exists() && started.elapsed() < Duration::from_secs(20) {
                if let Some(status) = parent.try_wait()? {
                    panic!("parent exited before child ready: {status}");
                }
                thread::sleep(Duration::from_millis(25));
            }
            assert!(ready.exists(), "child never acquired inherited stdin");
            parent.kill()?;
            parent.wait()?;
            let observer = owner_lock(&lock)?;
            assert!(
                matches!(observer.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                "the surviving Windows child lost the parent-owned file lock"
            );
            println!("WINDOWS_INHERITED_LOCK_HELD_AFTER_PARENT_EXIT=PASS");
            std::fs::write(&release, "release")?;
            let started = Instant::now();
            loop {
                match observer.try_lock() {
                    Ok(()) => break,
                    Err(std::fs::TryLockError::WouldBlock)
                        if started.elapsed() < Duration::from_secs(20) =>
                    {
                        thread::sleep(Duration::from_millis(25));
                    }
                    other => panic!("lock not released after child exit: {other:?}"),
                }
            }
            println!("WINDOWS_INHERITED_LOCK_RELEASED_AFTER_CHILD_EXIT=PASS");
            drop(observer);
            std::fs::remove_dir_all(&base)?;
        }
    }
    Ok(())
}
