//! Per-user Windows startup recovery lease for an explicitly requested update.
//! The registry entry runs only the installed helper's fail-closed rollback command.

use std::{io, path::Path};

#[cfg(not(windows))]
pub(crate) fn arm(_helper: &Path) -> io::Result<bool> {
    Ok(false)
}

#[cfg(not(windows))]
pub(crate) fn disarm(_helper: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn is_armed(_helper: &Path) -> io::Result<bool> {
    Ok(true)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
            RegCreateKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        },
    };

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const RECOVER_ARG: &str = "--auto-recover-interrupted-update";
    const MAX_REGISTRY_BYTES: u32 = 8192;

    struct RegistryKey(HKEY);

    impl Drop for RegistryKey {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn open_existing(subkey: &str) -> io::Result<Option<RegistryKey>> {
        let mut key = ptr::null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(OsStr::new(subkey)).as_ptr(),
                0,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                &mut key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Some(RegistryKey(key)))
    }

    fn open_or_create(subkey: &str) -> io::Result<RegistryKey> {
        if let Some(key) = open_existing(subkey)? {
            return Ok(key);
        }
        let mut key = ptr::null_mut();
        let status = unsafe {
            RegCreateKeyW(
                HKEY_CURRENT_USER,
                wide(OsStr::new(subkey)).as_ptr(),
                &mut key,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(RegistryKey(key))
    }

    fn read_entry(key: &RegistryKey, name: &str) -> io::Result<Option<String>> {
        let name = wide(OsStr::new(name));
        let mut kind = 0;
        let mut size = 0u32;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                ptr::null_mut(),
                &mut size,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if kind != REG_SZ || size < 2 || size > MAX_REGISTRY_BYTES || size % 2 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe recovery startup registry entry",
            ));
        }
        let mut bytes = vec![0u8; size as usize];
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                bytes.as_mut_ptr(),
                &mut size,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if kind != REG_SZ || size < 2 || size as usize > bytes.len() || size % 2 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recovery registry entry changed while reading",
            ));
        }
        let units: Vec<u16> = bytes[..size as usize]
            .chunks_exact(2)
            .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
            .collect();
        if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid recovery startup string",
            ));
        }
        String::from_utf16(&units[..units.len() - 1])
            .map(Some)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid UTF-16 recovery command",
                )
            })
    }

    fn identity(helper: &Path) -> io::Result<(String, String)> {
        let helper = std::fs::canonicalize(helper)?;
        let command_path = helper.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "non-Unicode Windows helper path",
            )
        })?;
        if command_path.contains(['\0', '"']) || !helper.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid helper executable path",
            ));
        }
        let command = format!("\"{command_path}\" {RECOVER_ARG}");
        if command.encode_utf16().count() > 260 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows Run startup command exceeds 260 UTF-16 units",
            ));
        }
        let digest = ring::digest::digest(
            &ring::digest::SHA256,
            command_path.to_lowercase().as_bytes(),
        );
        let suffix = digest.as_ref()[..12]
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect::<String>();
        Ok((format!("NeuroMitaUpdateRecovery-{suffix}"), command))
    }

    fn register_in_key(subkey: &str, name: &str, command: &str) -> io::Result<bool> {
        let key = open_or_create(subkey)?;
        match read_entry(&key, name)? {
            Some(existing) if existing == command => return Ok(false),
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "recovery startup entry belongs to another command",
                ));
            }
            None => {}
        }
        let data = wide(OsStr::new(command));
        let status = unsafe {
            RegSetValueExW(
                key.0,
                wide(OsStr::new(name)).as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(true)
    }

    fn is_registered_in_key(subkey: &str, name: &str, command: &str) -> io::Result<bool> {
        let Some(key) = open_existing(subkey)? else {
            return Ok(false);
        };
        match read_entry(&key, name)? {
            None => Ok(false),
            Some(existing) if existing == command => Ok(true),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "recovery startup entry was modified",
            )),
        }
    }

    fn remove_from_key(subkey: &str, name: &str, command: &str) -> io::Result<()> {
        let Some(key) = open_existing(subkey)? else {
            return Ok(());
        };
        let Some(existing) = read_entry(&key, name)? else {
            return Ok(());
        };
        if existing != command {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "refusing to remove another recovery startup command",
            ));
        }
        let status = unsafe { RegDeleteValueW(key.0, wide(OsStr::new(name)).as_ptr()) };
        if status != 0 && status != ERROR_FILE_NOT_FOUND {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }

    pub(crate) fn arm(helper: &Path) -> io::Result<bool> {
        let (name, command) = identity(helper)?;
        register_in_key(RUN_KEY, &name, &command)
    }

    pub(crate) fn disarm(helper: &Path) -> io::Result<()> {
        let (name, command) = identity(helper)?;
        remove_from_key(RUN_KEY, &name, &command)
    }

    pub(crate) fn is_armed(helper: &Path) -> io::Result<bool> {
        let (name, command) = identity(helper)?;
        is_registered_in_key(RUN_KEY, &name, &command)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use uuid::Uuid;

        #[test]
        fn registry_recovery_lease_is_exact_and_cannot_delete_modified_commands() {
            let subkey = format!("Software\\NeuroMita\\UpdaterLeaseTests\\{}", Uuid::new_v4());
            let name = "test-recovery";
            let command = r#""C:\\App\\neuromita-updater.exe" --auto-recover-interrupted-update"#;
            assert!(!is_registered_in_key(&subkey, name, command).unwrap());
            assert!(register_in_key(&subkey, name, command).unwrap());
            assert!(!register_in_key(&subkey, name, command).unwrap());
            assert!(is_registered_in_key(&subkey, name, command).unwrap());
            assert!(register_in_key(&subkey, name, "other command").is_err());
            assert!(remove_from_key(&subkey, name, "other command").is_err());
            assert!(is_registered_in_key(&subkey, name, command).unwrap());
            remove_from_key(&subkey, name, command).unwrap();
            assert!(!is_registered_in_key(&subkey, name, command).unwrap());
            unsafe {
                windows_sys::Win32::System::Registry::RegDeleteKeyW(
                    HKEY_CURRENT_USER,
                    wide(OsStr::new(&subkey)).as_ptr(),
                )
            };
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::{arm, disarm, is_armed};
