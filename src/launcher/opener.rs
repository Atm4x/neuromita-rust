use std::path::Path;

#[cfg(not(windows))]
use std::process::{Command, Stdio};

use super::LauncherError;

pub trait LocationOpener: Send + Sync {
    fn open(&self, directory: &Path) -> Result<(), LauncherError>;
}

pub struct SystemLocationOpener;

#[cfg(not(windows))]
impl LocationOpener for SystemLocationOpener {
    fn open(&self, directory: &Path) -> Result<(), LauncherError> {
        let mut child = Command::new("xdg-open")
            .arg(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| LauncherError::OpenLocation {
                path: directory.to_path_buf(),
                message: error.to_string(),
            })?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

#[cfg(windows)]
impl LocationOpener for SystemLocationOpener {
    fn open(&self, directory: &Path) -> Result<(), LauncherError> {
        use std::ptr;
        use windows_sys::Win32::{
            System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
            UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        };

        let path = shell_folder_path(directory);
        let verb = [b'o' as u16, b'p' as u16, b'e' as u16, b'n' as u16, 0];
        let apartment = unsafe {
            CoInitializeEx(ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32)
        };
        if apartment < 0 {
            return Err(LauncherError::OpenLocation {
                path: directory.to_owned(),
                message: format!("Windows Shell initialization failed (HRESULT {apartment:#010x})"),
            });
        }
        let result = unsafe {
            let result = ShellExecuteW(ptr::null_mut(), verb.as_ptr(), path.as_ptr(), ptr::null(), ptr::null(), SW_SHOWNORMAL);
            CoUninitialize();
            result as isize
        };
        if result <= 32 {
            return Err(LauncherError::OpenLocation {
                path: directory.to_owned(),
                message: format!("Windows Shell could not open the folder (code {result})"),
            });
        }
        Ok(())
    }
}

#[cfg(windows)]
fn shell_folder_path(directory: &Path) -> Vec<u16> {
    use std::{os::windows::ffi::OsStrExt, path::{Component, Prefix}};

    let mut path: Vec<_> = directory.as_os_str().encode_wide().collect();
    if let Some(Component::Prefix(prefix)) = directory.components().next() {
        // std::fs::canonicalize returns extended paths, while Explorer expects DOS or UNC paths.
        match prefix.kind() {
            Prefix::VerbatimDisk(_) => { path.drain(..4); }
            Prefix::VerbatimUNC(..) => { path.drain(2..8); }
            _ => {}
        }
    }
    path.push(0);
    path
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;

    #[test]
    fn shell_receives_canonical_disk_and_network_paths_without_extended_prefix() {
        for (input, expected) in [
            (r"\\?\D:\Games & Tools\Мита", r"D:\Games & Tools\Мита"),
            (r"\\?\UNC\server\share\NeuroMita", r"\\server\share\NeuroMita"),
            (r"D:\NeuroMita", r"D:\NeuroMita"),
        ] {
            assert_eq!(shell_folder_path(Path::new(input)), expected.encode_utf16().chain([0]).collect::<Vec<_>>());
        }
    }
}
