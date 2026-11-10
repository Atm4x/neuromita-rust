use std::{fs, path::Path, process::Command};

#[cfg(windows)]
use std::{
    os::windows::fs::OpenOptionsExt,
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};

use ring::digest::{SHA256, digest};
use serde_json::json;
use uuid::Uuid;

fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn stopped_installation_recovers_terminated_helper_transaction() {
    let root = std::env::temp_dir().join(format!("nm-offline-recovery-{}", Uuid::new_v4()));
    let install = root.join("app");
    let workspace = install.join(".neuromita-update");
    let data = install.join("UserData");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("settings.json"), b"original user settings").unwrap();

    let gui_name = if cfg!(windows) {
        "neuromita-gui.exe"
    } else {
        "neuromita-gui"
    };
    let helper_name = if cfg!(windows) {
        "neuromita-updater.exe"
    } else {
        "neuromita-updater"
    };
    let installed = install.join(gui_name);
    let old = b"verified previous executable";

    #[cfg(target_os = "linux")]
    let new = fs::read("/bin/sleep").unwrap();
    #[cfg(windows)]
    let new = fs::read(std::env::current_exe().unwrap()).unwrap();

    fs::write(&installed, &new).unwrap();
    #[cfg(target_os = "linux")]
    fs::set_permissions(
        &installed,
        fs::metadata("/bin/sleep").unwrap().permissions(),
    )
    .unwrap();
    fs::write(workspace.join("backup"), old).unwrap();
    fs::write(
        workspace.join("activation.json"),
        serde_json::to_vec(&json!({
            "target_name": gui_name,
            "previous_size": old.len(),
            "previous_sha256": hash(old),
            "next_size": new.len(),
            "next_sha256": hash(&new),
            "health_nonce": Uuid::new_v4(),
        }))
        .unwrap(),
    )
    .unwrap();
    let helper = install.join(helper_name);
    fs::copy(env!("CARGO_BIN_EXE_neuromita-updater"), &helper).unwrap();

    #[cfg(windows)]
    {
        let marker = root.join("gui-running");
        let mut gui = RunningWindowsGui::spawn(&installed, &marker);
        let rejected = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !rejected.status.success(),
            "a running installed GUI must block recovery"
        );
        assert!(workspace.join("backup").is_file());
        assert_eq!(fs::read(&installed).unwrap(), new);
        gui.stop();
        fs::remove_file(marker).unwrap();

        let installed_lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&installed)
            .unwrap();
        let rejected = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !rejected.status.success(),
            "a sharing-locked installed EXE must not be overwritten"
        );
        assert!(workspace.join("backup").exists());
        drop(installed_lock);
        assert_eq!(fs::read(&installed).unwrap(), new);

        let backup = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(workspace.join("backup"))
            .unwrap();
        let rejected = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !rejected.status.success(),
            "an exclusively locked backup must not be consumed"
        );
        assert!(workspace.join("backup").exists());
        assert_eq!(fs::read(&installed).unwrap(), new);
        drop(backup);
    }

    #[cfg(target_os = "linux")]
    {
        let mut child = Command::new(&installed).arg("4").spawn().unwrap();
        let denied = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !denied.status.success(),
            "must not replace a live executable"
        );
        assert!(Path::new(&workspace.join("backup")).exists());
        assert_eq!(fs::read(&installed).unwrap(), new);
        fs::rename(&installed, workspace.join("candidate")).unwrap();
        let denied_during_rename = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !denied_during_rename.status.success(),
            "a running but renamed candidate must not be deleted"
        );
        assert!(workspace.join("backup").exists());
        assert!(!installed.exists());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    let result = Command::new(&helper)
        .arg("--recover-interrupted-update")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&installed).unwrap(), old);
    assert_eq!(
        fs::read(data.join("settings.json")).unwrap(),
        b"original user settings"
    );
    assert!(!workspace.exists());
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("updates/last-activation.json")).unwrap())
            .unwrap();
    assert_eq!(report["state"], "interrupted-update-restored");
    let second = Command::new(&helper)
        .arg("--recover-interrupted-update")
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "already recovered update must not be replayed"
    );
    assert_eq!(fs::read(&installed).unwrap(), old);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
struct RunningWindowsGui(Child);

#[cfg(windows)]
impl RunningWindowsGui {
    fn spawn(executable: &Path, marker: &Path) -> Self {
        let child = Command::new(executable)
            .args(["--exact", "internal_running_gui_probe", "--ignored"])
            .env("NM_D52_GUI_PROBE_MARKER", marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut guard = Self(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                guard.0.try_wait().unwrap().is_none(),
                "installed GUI probe exited before starting"
            );
            assert!(Instant::now() < deadline, "installed GUI probe timed out");
            thread::sleep(Duration::from_millis(50));
        }
        guard
    }

    fn stop(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}

#[cfg(windows)]
impl Drop for RunningWindowsGui {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(windows)]
#[test]
#[ignore = "subprocess probe invoked only by the installed Windows E2E test"]
fn internal_running_gui_probe() {
    let marker = std::env::var_os("NM_D52_GUI_PROBE_MARKER").expect("missing probe marker");
    fs::write(marker, b"running").unwrap();
    thread::sleep(Duration::from_secs(20));
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn orphaned_verified_handoff_is_recovered_only_after_exact_validation() {
    for applied in [false, true] {
        let root = std::env::temp_dir().join(format!("nm-orphan-handoff-{}", Uuid::new_v4()));
        let install = root.join("app");
        let workspace = install.join(".neuromita-update");
        let data = install.join("UserData");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("settings.json"), b"private unchanged").unwrap();
        let name = if cfg!(windows) {
            "neuromita-gui.exe"
        } else {
            "neuromita-gui"
        };
        let helper_name = if cfg!(windows) {
            "neuromita-updater.exe"
        } else {
            "neuromita-updater"
        };
        let old = b"old verified bytes";
        let new = b"new verified bytes";
        fs::write(install.join(name), if applied { new } else { old }).unwrap();
        if applied {
            fs::write(workspace.join("backup"), old).unwrap();
        } else {
            fs::write(workspace.join("candidate"), new).unwrap();
        }
        let nonce = Uuid::new_v4();
        fs::write(
            workspace.join("activation.json"),
            serde_json::to_vec(&json!({
                "target_name": name,
                "previous_size": old.len(),
                "previous_sha256": hash(old),
                "next_size": new.len(),
                "next_sha256": hash(new),
                "health_nonce": nonce,
            }))
            .unwrap(),
        )
        .unwrap();
        let handoff = workspace.join("handoff.json");
        let intent = json!({
            "parent_pid": u32::MAX - 17,
            "parent_identity": 19,
            "nonce": Uuid::new_v4(),
            "target": name,
        });
        fs::write(&handoff, serde_json::to_vec(&intent).unwrap()).unwrap();
        let helper = install.join(helper_name);
        fs::copy(env!("CARGO_BIN_EXE_neuromita-updater"), &helper).unwrap();
        let denied = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(!denied.status.success(), "mismatched intent must be denied");
        assert!(workspace.join("activation.json").exists());
        assert!(handoff.exists());
        assert_eq!(
            fs::read(install.join(name)).unwrap(),
            if applied { new } else { old }
        );

        let mut matching = intent;
        matching["nonce"] = json!(nonce);
        let matching_bytes = serde_json::to_vec(&matching).unwrap();
        fs::write(&handoff, &matching_bytes).unwrap();
        fs::write(workspace.join("unknown-entry"), b"operator evidence").unwrap();
        let blocked = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !blocked.status.success(),
            "unknown workspace object must block recovery"
        );
        assert_eq!(fs::read(&handoff).unwrap(), matching_bytes);
        assert!(workspace.join("activation.json").exists());
        assert!(workspace.join("unknown-entry").exists());
        fs::remove_file(workspace.join("unknown-entry")).unwrap();

        let target = install.join(name);
        let verified = fs::read(&target).unwrap();
        fs::write(&target, b"foreign modified executable").unwrap();
        let tampered = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !tampered.status.success(),
            "foreign EXE bytes must not be replaced"
        );
        assert_eq!(fs::read(&target).unwrap(), b"foreign modified executable");
        assert_eq!(fs::read(&handoff).unwrap(), matching_bytes);
        assert!(workspace.join("activation.json").exists());
        fs::write(&target, &verified).unwrap();

        let recovered = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            recovered.status.success(),
            "orphan recovery stderr: {}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert_eq!(fs::read(install.join(name)).unwrap(), old);
        assert_eq!(
            fs::read(data.join("settings.json")).unwrap(),
            b"private unchanged"
        );
        assert!(!workspace.exists());
        assert!(
            !Command::new(&helper)
                .arg("--recover-interrupted-update")
                .output()
                .unwrap()
                .status
                .success()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
