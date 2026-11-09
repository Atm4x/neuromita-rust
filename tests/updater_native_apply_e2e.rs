#[cfg(target_os = "linux")]
use std::{
    env,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use neuromita::{
    portable::PortableDataRoot,
    updater::{
        UpdateAsset, UpdateStager,
        activation::SingleFileActivator,
        check::native_asset_name,
        handoff::{READY_NONCE_ENV, acknowledge_gui_ready},
        install::schedule_prepared_native_update,
    },
};
#[cfg(target_os = "linux")]
use ring::digest::{SHA256, digest};
#[cfg(target_os = "linux")]
use tokio_util::sync::CancellationToken;
#[cfg(target_os = "linux")]
use uuid::Uuid;

#[cfg(target_os = "linux")]
const NEW_OK: &[u8] = b"NM_APPLY_PROBE_OK";
#[cfg(target_os = "linux")]
const NEW_BAD: &[u8] = b"NM_APPLY_PROBE_NO";

fn main() {
    #[cfg(windows)]
    windows_acceptance::run();
    #[cfg(not(any(windows, target_os = "linux")))]
    eprintln!("Native updater E2E is supported on Linux and Windows only");
    #[cfg(target_os = "linux")]
    {
        let result = if env::var_os(READY_NONCE_ENV).is_some() {
            restarted_gui()
        } else if env::args().nth(1).as_deref() == Some("--e2e-install-parent") {
            installed_parent()
        } else if env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|s| s == "neuromita-gui"))
            .unwrap_or(false)
        {
            restored_gui()
        } else {
            conductor()
        };
        if let Err(error) = result {
            eprintln!("Native apply E2E failed: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn installed_parent() -> Result<(), Box<dyn std::error::Error>> {
    let exe = env::current_exe()?;
    let install = exe.parent().ok_or("no parent")?;
    let data = PortableDataRoot::resolve(Some(&install.join("UserData")))?;
    schedule_prepared_native_update(&data)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn restarted_gui() -> Result<(), Box<dyn std::error::Error>> {
    let exe = env::current_exe()?;
    let mut file = File::open(&exe)?;
    file.seek(SeekFrom::End(-(NEW_OK.len() as i64)))?;
    let mut signature = vec![0; NEW_OK.len()];
    file.read_exact(&mut signature)?;
    if signature == NEW_BAD {
        return Err("simulated startup failure before readiness".into());
    }
    if signature != NEW_OK {
        return Err("unknown update probe binary".into());
    }
    if !acknowledge_gui_ready()? {
        return Err("new child had no readiness nonce".into());
    }
    fs::write(exe.parent().unwrap().join("new-gui-ready"), b"ready")?;
    thread::sleep(Duration::from_secs(3));
    Ok(())
}

#[cfg(target_os = "linux")]
fn restored_gui() -> Result<(), Box<dyn std::error::Error>> {
    let exe = env::current_exe()?;
    fs::write(
        exe.parent().unwrap().join("old-gui-relaunched"),
        b"relaunched",
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(target_os = "linux")]
fn executable_bytes(overlay: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = fs::read(env::current_exe()?)?;
    bytes.extend_from_slice(overlay);
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn wait_until_report(data: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let path = data.join("updates/last-activation.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(bytes) = fs::read(&path) {
            let report: serde_json::Value = serde_json::from_slice(&bytes)?;
            return Ok(report["state"]
                .as_str()
                .ok_or("report state missing")?
                .to_owned());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("installed helper did not publish activation result".into())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Healthy,
    BadStartup,
    HelperNotExecutable,
    HelperMissing,
    ConflictingHandoff,
}

#[cfg(target_os = "linux")]
fn case(mode: Scenario, base: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let name = match mode {
        Scenario::Healthy => "healthy",
        Scenario::BadStartup => "rollback",
        Scenario::HelperNotExecutable => "denied",
        Scenario::HelperMissing => "missing",
        Scenario::ConflictingHandoff => "conflicting-handoff",
    };
    let install = base.join(name);
    let data_path = install.join("UserData");
    fs::create_dir_all(&data_path)?;
    fs::write(
        data_path.join("settings.json"),
        b"unmodified private settings",
    )?;
    let gui = install.join("neuromita-gui");
    let old_source = env::current_exe()?;
    if fs::hard_link(&old_source, &gui).is_err() {
        fs::copy(&old_source, &gui)?;
    }
    let old = fs::read(&gui)?;
    let installed_probe = matches!(mode, Scenario::Healthy | Scenario::BadStartup);
    let next_source = base.join(if mode == Scenario::BadStartup {
        "new-bad"
    } else {
        "new-good"
    });
    let new = if installed_probe {
        fs::read(&next_source)?
    } else {
        b"candidate never applied when helper startup fails".to_vec()
    };
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&gui, fs::Permissions::from_mode(0o755))?;
    let helper = install.join("neuromita-updater");
    match mode {
        Scenario::HelperMissing => {}
        Scenario::HelperNotExecutable => {
            fs::write(&helper, b"helper without execute permission")?;
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o644))?;
        }
        _ => {
            if fs::hard_link(base.join("helper"), &helper).is_err() {
                fs::copy(base.join("helper"), &helper)?;
            }
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
        }
    }

    let data = PortableDataRoot::resolve(Some(&data_path))?;
    let asset_name =
        native_asset_name("linux", env::consts::ARCH).ok_or("no native Linux asset")?;
    let digest = sha256(&new);
    let asset = UpdateAsset::new(
        asset_name,
        "https://example.invalid/native",
        new.len() as u64,
        &format!("sha256:{digest}"),
    )?;
    let stage = data_path.join("updates/staged");
    fs::create_dir_all(&stage)?;
    let staged_path = stage.join(format!("sha256-{digest}.artifact"));
    if installed_probe {
        if fs::hard_link(&next_source, &staged_path).is_err() {
            fs::copy(&next_source, &staged_path)?;
        }
    } else {
        fs::write(&staged_path, &new)?;
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let artifact =
        runtime.block_on(UpdateStager::new(&data)?.stage(&asset, &CancellationToken::new()))?;
    assert!(artifact.reused(), "publisher-hash cache was not reused");
    let activator = SingleFileActivator::new(&install, &data, "neuromita-gui")?;
    activator.prepare_native_gui(&asset, &artifact)?;
    if mode == Scenario::ConflictingHandoff {
        fs::create_dir(install.join(".neuromita-update/handoff.json"))?;
    }

    let parent = Command::new(&gui)
        .arg("--e2e-install-parent")
        .stdin(Stdio::null())
        .output()?;
    match mode {
        Scenario::ConflictingHandoff => {
            assert!(!parent.status.success());
            let diagnostics = String::from_utf8_lossy(&parent.stderr);
            assert!(
                diagnostics.contains("prepared transaction could not be discarded"),
                "unexpected collision diagnostics: {diagnostics}"
            );
            assert_eq!(fs::read(&gui)?, old);
            assert!(install.join(".neuromita-update/activation.json").exists());
            assert!(install.join(".neuromita-update/candidate").exists());
            fs::remove_dir(install.join(".neuromita-update/handoff.json"))?;
            activator.discard_prepared()?;
            assert!(!install.join(".neuromita-update").exists());
        }
        Scenario::HelperNotExecutable | Scenario::HelperMissing => {
            assert!(
                !parent.status.success(),
                "helper failure unexpectedly started update"
            );
            let diagnostics = String::from_utf8_lossy(&parent.stderr);
            assert!(
                !diagnostics.contains("prepared transaction could not be discarded"),
                "handoff failure stranded prepared transaction: {diagnostics}"
            );
            assert_eq!(fs::read(&gui)?, old);
            assert!(!install.join(".neuromita-update").exists());
        }
        Scenario::Healthy | Scenario::BadStartup => {
            if !parent.status.success() {
                return Err(format!(
                    "parent handoff failed: {}",
                    String::from_utf8_lossy(&parent.stderr)
                )
                .into());
            }
            let state = wait_until_report(&data_path)?;
            if mode == Scenario::Healthy {
                assert_eq!(state, "updated-and-restarted");
                assert_eq!(fs::read(&gui)?, new);
                assert!(install.join("new-gui-ready").exists());
            } else {
                assert_eq!(state, "rolled-back-after-startup-failure");
                assert_eq!(fs::read(&gui)?, old);
                assert!(install.join("old-gui-relaunched").exists());
            }
            assert!(!install.join(".neuromita-update").exists());
        }
    }
    assert_eq!(
        fs::read(data_path.join("settings.json"))?,
        b"unmodified private settings"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn conductor() -> Result<(), Box<dyn std::error::Error>> {
    let base = env::temp_dir().join(format!("nm-apply-e2e-{}", Uuid::new_v4()));
    fs::create_dir_all(&base)?;
    let result = (|| {
        fs::write(base.join("new-good"), executable_bytes(NEW_OK)?)?;
        fs::write(base.join("new-bad"), executable_bytes(NEW_BAD)?)?;
        fs::copy(env!("CARGO_BIN_EXE_neuromita-updater"), base.join("helper"))?;
        case(Scenario::Healthy, &base)?;
        case(Scenario::BadStartup, &base)?;
        case(Scenario::HelperNotExecutable, &base)?;
        case(Scenario::HelperMissing, &base)?;
        case(Scenario::ConflictingHandoff, &base)?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })();
    thread::sleep(Duration::from_secs(3));
    if result.is_ok() {
        fs::remove_dir_all(&base)?;
    }
    result
}

#[cfg(windows)]
mod windows_acceptance {
    use std::{
        env, fs,
        io::{Read, Seek, SeekFrom},
        path::Path,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use neuromita::{
        portable::PortableDataRoot,
        updater::{
            UpdateAsset, UpdateStager,
            activation::{RecoveryOutcome, SingleFileActivator},
            check::native_asset_name,
            handoff::{READY_NONCE_ENV, acknowledge_gui_ready},
        },
    };
    use ring::digest::{SHA256, digest};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    const GOOD: &[u8] = b"NM_APPLY_PROBE_OK";
    const BAD: &[u8] = b"NM_APPLY_PROBE_NO";

    pub fn run() {
        let result = if env::var_os(READY_NONCE_ENV).is_some() {
            restarted_probe()
        } else {
            conductor()
        };
        if let Err(error) = result {
            eprintln!("Windows activation E2E failed: {error}");
            std::process::exit(1);
        }
    }

    fn restarted_probe() -> Result<(), Box<dyn std::error::Error>> {
        let mut file = fs::File::open(env::current_exe()?)?;
        file.seek(SeekFrom::End(-(GOOD.len() as i64)))?;
        let mut signature = vec![0; GOOD.len()];
        file.read_exact(&mut signature)?;
        if signature == BAD {
            return Err("intentional failure before readiness".into());
        }
        if signature != GOOD || !acknowledge_gui_ready()? {
            return Err("invalid native readiness receipt".into());
        }
        thread::sleep(Duration::from_secs(2));
        Ok(())
    }

    fn hash(bytes: &[u8]) -> String {
        digest(&SHA256, bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn case(success: bool, base: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let install = base.join(if success { "ready" } else { "failed" });
        let data_path = install.join("UserData");
        fs::create_dir_all(&data_path)?;
        fs::write(data_path.join("settings.json"), b"user settings unchanged")?;
        let gui = install.join("neuromita-gui.exe");
        let mut previous = fs::read(env::current_exe()?)?;
        previous.extend_from_slice(b"NM_APPLY_PROBE_OLD");
        let mut next = fs::read(env::current_exe()?)?;
        next.extend_from_slice(if success { GOOD } else { BAD });
        fs::write(&gui, &previous)?;
        let data = PortableDataRoot::resolve(Some(&data_path))?;
        let digest = hash(&next);
        let asset = UpdateAsset::new(
            native_asset_name("windows", env::consts::ARCH)
                .ok_or("unsupported Windows architecture")?,
            "https://example.invalid/disposable-e2e",
            next.len() as u64,
            &format!("sha256:{digest}"),
        )?;
        let stage = data_path.join("updates/staged");
        fs::create_dir_all(&stage)?;
        fs::write(stage.join(format!("sha256-{digest}.artifact")), &next)?;
        let rt = tokio::runtime::Runtime::new()?;
        let artifact =
            rt.block_on(UpdateStager::new(&data)?.stage(&asset, &CancellationToken::new()))?;
        assert!(artifact.reused());
        let activator = SingleFileActivator::new(&install, &data, "neuromita-gui.exe")?;
        activator.prepare_native_gui(&asset, &artifact)?;
        let nonce = Uuid::new_v4();
        activator.arm_health_check(nonce)?;
        assert_eq!(
            activator.apply_or_resume()?,
            RecoveryOutcome::AwaitingHealth
        );
        assert_eq!(fs::read(&gui)?, next);
        let mut child = Command::new(&gui)
            .env(READY_NONCE_ENV, nonce.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if success {
            let receipt = install.join(".neuromita-update/readiness.json");
            let deadline = Instant::now() + Duration::from_secs(12);
            while !receipt.exists() {
                assert!(
                    child.try_wait()?.is_none(),
                    "new Windows PE exited before readiness"
                );
                assert!(Instant::now() < deadline, "Windows readiness timed out");
                thread::sleep(Duration::from_millis(40));
            }
            let value: serde_json::Value = serde_json::from_slice(&fs::read(&receipt)?)?;
            assert_eq!(value["nonce"].as_str(), Some(nonce.to_string().as_str()));
            assert_eq!(value["pid"].as_u64(), Some(child.id() as u64));
            activator.complete_health_check(nonce)?;
            assert_eq!(fs::read(&gui)?, next);
            assert!(!install.join(".neuromita-update").exists());
        } else {
            assert!(!child.wait()?.success());
            activator.rollback_health_check(nonce)?;
            assert_eq!(fs::read(&gui)?, previous);
            assert!(!install.join(".neuromita-update").exists());
        }
        if success {
            child.wait()?;
        }
        assert_eq!(
            fs::read(data_path.join("settings.json"))?,
            b"user settings unchanged"
        );
        Ok(())
    }

    fn conductor() -> Result<(), Box<dyn std::error::Error>> {
        let root = env::temp_dir().join(format!("nm-windows-apply-e2e-{}", Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        let result = (|| {
            case(true, &root)?;
            case(false, &root)?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })();
        if result.is_ok() {
            fs::remove_dir_all(root)?;
        }
        result
    }
}
