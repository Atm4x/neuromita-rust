use std::{fs, path::PathBuf, process::Command};

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("neuromita-diag-cli-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_file_records_only_owned_lifecycle_events() {
    let root = TestRoot::new();
    let output = Command::new(env!("CARGO_BIN_EXE_neuromita"))
        .args(["settings", "--help"])
        .env("NEUROMITA_DATA_DIR", &root.0)
        .env("RUST_LOG", "neuromita=debug")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("starting application"));
    let files = fs::read_dir(root.0.join("logs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    let content = fs::read_to_string(&files[0]).unwrap();
    assert!(content.contains("cli_start"));
    assert!(content.contains("cli_finish"));
    assert!(!content.contains("starting application"));
    assert!(!content.contains("GEMINI_API_KEY"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
