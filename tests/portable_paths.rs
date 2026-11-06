use std::{ffi::OsStr, fs, io, path::Path};

use neuromita::portable::PortableDataRoot;
use uuid::Uuid;

fn fixture_root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("neuromita-portable-{}", Uuid::new_v4()))
}

#[test]
fn installed_executable_owns_userdata_and_relocates_with_installation() {
    let root = fixture_root();
    let installed = root.join("Neuro Mita").join("bin").join("нейромита.exe");
    let resolved = PortableDataRoot::from_executable(&installed, None, None).unwrap();
    assert_eq!(
        resolved.path(),
        installed.parent().unwrap().join("UserData")
    );
    let relocated = root.join("Новый каталог").join("neuromita.exe");
    let resolved = PortableDataRoot::from_executable(&relocated, None, None).unwrap();
    assert_eq!(
        resolved.path(),
        relocated.parent().unwrap().join("UserData")
    );
}

#[test]
fn explicit_directory_wins_and_never_uses_profile_fallbacks() {
    let root = fixture_root();
    let executable = root.join("install/neuromita");
    let explicit = root.join("Моя NeuroMita/data");
    let env = root.join("AppData/Other");
    let resolved =
        PortableDataRoot::from_executable(&executable, Some(&explicit), Some(env.as_os_str()))
            .unwrap();
    assert_eq!(resolved.path(), explicit);
    let env_root =
        PortableDataRoot::from_executable(&executable, None, Some(env.as_os_str())).unwrap();
    assert_eq!(env_root.path(), env);
}

#[test]
fn relative_and_empty_overrides_fail_instead_of_using_cwd() {
    let executable = fixture_root().join("app");
    for path in ["./data", "UserData", ""] {
        assert_eq!(
            PortableDataRoot::from_executable(&executable, Some(Path::new(path)), None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        PortableDataRoot::from_executable(&executable, None, Some(OsStr::new("")))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(PortableDataRoot::from_executable(Path::new("./app"), None, None).is_err());
}

#[test]
fn unwritable_or_invalid_root_is_a_hard_error() {
    let directory = fixture_root();
    fs::create_dir_all(&directory).unwrap();
    let blocked = directory.join("not-a-directory");
    fs::write(&blocked, b"existing file").unwrap();
    let root =
        PortableDataRoot::from_executable(&directory.join("install/app"), Some(&blocked), None)
            .unwrap();
    assert!(root.ensure_writable().is_err());
    let writable = PortableDataRoot::from_executable(
        &directory.join("install/app"),
        Some(&directory.join("data")),
        None,
    )
    .unwrap();
    writable.ensure_writable().unwrap();
    assert!(writable.path().is_dir());
    assert_eq!(fs::read_dir(writable.path()).unwrap().count(), 0);
    fs::remove_dir_all(directory).unwrap();
}
