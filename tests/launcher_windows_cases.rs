use super::*;
use std::{ffi::c_void, ptr};
use windows_sys::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
        PostQuitMessage, RegisterClassW, TranslateMessage, WM_CLOSE, WM_DESTROY, WNDCLASSW,
        WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    },
};

const WINDOW_MODE: &str = "NEUROMITA_LAUNCHER_WINDOW_MODE";
const WINDOW_READY: &str = "NEUROMITA_LAUNCHER_WINDOW_READY";
const WINDOW_CLOSE_SEEN: &str = "NEUROMITA_LAUNCHER_WINDOW_CLOSE_SEEN";

unsafe extern "system" fn window_proc(
    hwnd: *mut c_void,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match message {
        WM_CLOSE => {
            let seen = std::env::var_os(WINDOW_CLOSE_SEEN).expect("close marker path");
            fs::write(seen, b"close received").expect("record close request");
            if std::env::var(WINDOW_MODE).as_deref() != Ok("ignore") {
                unsafe { DestroyWindow(hwnd) };
            }
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[test]
#[ignore = "spawned as a real Windows window by launcher integration tests"]
fn launcher_window_helper_process() {
    let Ok(mode) = std::env::var(WINDOW_MODE) else {
        return;
    };
    assert!(mode == "exit" || mode == "ignore");
    let class: Vec<u16> = "NeuroMitaOwnedWindowFixture\0".encode_utf16().collect();
    let title: Vec<u16> = "NeuroMita Launcher Test\0".encode_utf16().collect();
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    assert!(!module.is_null(), "module handle");
    let registration = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: module,
        lpszClassName: class.as_ptr(),
        ..WNDCLASSW::default()
    };
    assert_ne!(
        unsafe { RegisterClassW(&registration) },
        0,
        "register class"
    );
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            20,
            20,
            320,
            200,
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null(),
        )
    };
    assert!(!hwnd.is_null(), "create visible top-level window");
    let ready = std::env::var_os(WINDOW_READY).expect("ready marker path");
    fs::write(ready, b"visible").expect("write ready marker");
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        assert_ne!(result, -1, "window message loop failed");
        if result == 0 {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn window_request(mode: &str, ready: &Path, seen: &Path) -> LaunchRequest {
    LaunchRequest::new()
        .with_arguments(
            [
                "--exact",
                "launcher_windows_cases::launcher_window_helper_process",
                "--ignored",
                "--nocapture",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .with_environment([
            (OsString::from(WINDOW_MODE), OsString::from(mode)),
            (
                OsString::from(WINDOW_READY),
                ready.as_os_str().to_os_string(),
            ),
            (
                OsString::from(WINDOW_CLOSE_SEEN),
                seen.as_os_str().to_os_string(),
            ),
        ])
}

fn spawn_window_helper(executable: &Path, mode: &str, ready: &Path, seen: &Path) -> ChildGuard {
    use std::os::windows::process::CommandExt;

    let request = window_request(mode, ready, seen);
    let mut command = Command::new(executable);
    command
        .args(request.arguments)
        .envs(request.environment)
        .current_dir(executable.parent().expect("fixture install"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.creation_flags(0x08000000);
    ChildGuard::new(command.spawn().expect("spawn foreign GUI window"))
}

#[test]
fn owned_visible_window_exits_cooperatively_after_wm_close() {
    let directory = TestDirectory::new("gui-cooperative-stop");
    let root = directory.path().join("install");
    install_candidate(&root);
    let ready = directory.path().join("ready");
    let seen = directory.path().join("close");
    let mut session = installed_service(&root).session();
    session
        .start_with(window_request("exit", &ready, &seen))
        .expect("launch GUI helper");
    wait_until(Duration::from_secs(10), || ready.exists());

    let stopped = session.stop().expect("cooperative stop");
    assert!(matches!(
        stopped,
        OwnedProcessState::Finished {
            exit_code: Some(0),
            requested_stop: true
        }
    ));
    assert!(seen.exists(), "window must receive WM_CLOSE");
}

#[test]
fn ignored_wm_close_times_out_before_owned_force_kill() {
    let directory = TestDirectory::new("gui-ignored-stop");
    let root = directory.path().join("install");
    install_candidate(&root);
    let ready = directory.path().join("ready");
    let seen = directory.path().join("close");
    let mut session = installed_service(&root).session();
    let launched = session
        .start_with(window_request("ignore", &ready, &seen))
        .expect("launch ignoring GUI helper");
    let pid = launched.pid().expect("owned pid");
    wait_until(Duration::from_secs(10), || ready.exists());

    let started = Instant::now();
    let stopped = session.stop().expect("forced fallback after timeout");
    assert!(started.elapsed() >= Duration::from_millis(2800));
    assert!(seen.exists(), "window must observe ignored WM_CLOSE");
    assert!(matches!(
        stopped,
        OwnedProcessState::Finished {
            exit_code: Some(code),
            requested_stop: true,
        } if code != 0
    ));
    assert!(identity_for_pid(pid).expect("post-stop identity").is_none());
}

#[test]
fn owned_window_close_does_not_touch_foreign_same_name_window() {
    let directory = TestDirectory::new("gui-foreign-isolation");
    let root = directory.path().join("install");
    install_candidate(&root);
    let foreign_root = directory.path().join("different-install");
    let foreign_executable = install_candidate(&foreign_root);
    let foreign_ready = directory.path().join("foreign-ready");
    let foreign_close = directory.path().join("foreign-close");
    let mut foreign =
        spawn_window_helper(&foreign_executable, "exit", &foreign_ready, &foreign_close);
    wait_until(Duration::from_secs(10), || foreign_ready.exists());

    let ready = directory.path().join("owned-ready");
    let seen = directory.path().join("owned-close");
    let mut session = installed_service(&root).session();
    session
        .start_with(window_request("exit", &ready, &seen))
        .expect("launch owned GUI helper");
    wait_until(Duration::from_secs(10), || ready.exists());
    session.stop().expect("close owned window only");

    assert!(seen.exists(), "owned window must receive WM_CLOSE");
    assert!(
        !foreign_close.exists(),
        "foreign window must not receive WM_CLOSE"
    );
    assert!(
        foreign
            .child_mut()
            .try_wait()
            .expect("foreign status")
            .is_none()
    );
}
