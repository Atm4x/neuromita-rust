use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use tracing::Level;
use tracing_subscriber::{
    EnvFilter, Layer, filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt,
};

use crate::portable::PortableDataRoot;

const MAX_LOG_SIZE: u64 = 256 * 1024;
const MAX_SESSION_LOGS: usize = 5;
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub enum Boundary {
    CliStart,
    CliFinish,
    GuiStart,
    GuiFinish,
}

impl Boundary {
    fn label(self) -> &'static str {
        match self {
            Self::CliStart => "cli_start",
            Self::CliFinish => "cli_finish",
            Self::GuiStart => "gui_start",
            Self::GuiFinish => "gui_finish",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Outcome {
    Started,
    Completed,
    Failed,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

pub fn boundary(stage: Boundary, outcome: Outcome) {
    tracing::info!(target: "neuromita::diagnostics", stage = stage.label(), outcome = outcome.label(), "lifecycle boundary");
}

#[derive(Clone)]
pub struct DiagnosticBootstrap {
    sink: Arc<Mutex<Option<SessionLog>>>,
}

pub fn init() -> Result<DiagnosticBootstrap, Box<dyn Error + Send + Sync>> {
    let directives = match std::env::var("RUST_LOG") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "warn,neuromita=info".to_owned(),
        Err(error) => return Err(error.into()),
    };
    let filter = EnvFilter::builder().with_regex(false).parse(directives)?;
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr())
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").as_deref() != Ok("dumb");

    let sink = Arc::new(Mutex::new(None));
    let file_writer = FileWriter { sink: sink.clone() };
    let console = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_filter(filter);
    let local_file = tracing_subscriber::fmt::layer()
        .with_writer(move || file_writer.clone())
        .with_ansi(false)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .with_filter(filter_fn(|meta| {
            meta.target() == "neuromita::diagnostics" && *meta.level() <= Level::INFO
        }));
    tracing_subscriber::registry()
        .with(console)
        .with(local_file)
        .try_init()?;
    Ok(DiagnosticBootstrap { sink })
}

impl DiagnosticBootstrap {
    pub fn attach(&self, root: &PortableDataRoot) -> io::Result<PathBuf> {
        root.ensure_writable()?;
        let mut guard = self
            .sink
            .lock()
            .map_err(|_| io::Error::other("log writer poisoned"))?;
        if guard.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "diagnostic log already attached",
            ));
        }
        let session = SessionLog::open(root.path())?;
        let path = session.path.clone();
        *guard = Some(session);
        Ok(path)
    }
}

#[derive(Clone)]
struct FileWriter {
    sink: Arc<Mutex<Option<SessionLog>>>,
}

impl Write for FileWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut guard = self
            .sink
            .lock()
            .map_err(|_| io::Error::other("log writer poisoned"))?;
        if let Some(writer) = guard.as_mut() {
            writer.append(bytes)?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self
            .sink
            .lock()
            .map_err(|_| io::Error::other("log writer poisoned"))?;
        if let Some(writer) = guard.as_mut() {
            writer.file.flush()?;
        }
        Ok(())
    }
}

struct SessionLog {
    path: PathBuf,
    file: File,
    length: u64,
}

impl SessionLog {
    fn open(root: &Path) -> io::Result<Self> {
        let directory = root.join("logs");
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic logs directory must be a real directory",
            ));
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for _ in 0..16 {
            let sequence = SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(
                "session-{timestamp:020}-{}-{sequence}.log",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    Self::prune(&directory)?;
                    return Ok(Self {
                        path,
                        file,
                        length: 0,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique diagnostics log",
        ))
    }

    fn prune(directory: &Path) -> io::Result<()> {
        let mut previous = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let filename = entry.file_name();
            let name = filename.to_string_lossy();
            let file_type = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if name.starts_with("session-") && name.ends_with(".log") && file_type.is_file() {
                previous.push((filename, entry.path()));
            }
        }
        previous.sort_by(|left, right| left.0.cmp(&right.0));
        let remove_count = previous.len().saturating_sub(MAX_SESSION_LOGS);
        for (_, path) in previous.into_iter().take(remove_count) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.length >= MAX_LOG_SIZE || bytes.len() as u64 > MAX_LOG_SIZE - self.length {
            return Ok(());
        }
        self.file.write_all(bytes)?;
        self.length += bytes.len() as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "neuromita-diagnostics-test-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn retains_only_five_sessions_and_does_not_remove_other_files() {
        let root = Scratch::new();
        fs::create_dir(root.0.join("logs")).unwrap();
        fs::write(root.0.join("logs").join("user.log"), "leave intact").unwrap();
        for _ in 0..9 {
            SessionLog::open(&root.0).unwrap();
        }
        let files = fs::read_dir(root.0.join("logs"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 6);
        assert!(files.iter().any(|name| name == "user.log"));
    }

    #[test]
    fn simultaneous_session_open_does_not_fail_if_another_pruner_removed_an_entry() {
        let root = Scratch::new();
        let directory = Arc::new(root.0.clone());
        let threads = (0..12)
            .map(|_| {
                let directory = directory.clone();
                std::thread::spawn(move || {
                    for _ in 0..15 {
                        SessionLog::open(&directory).unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn caps_log_length_without_truncating_an_existing_record() {
        let root = Scratch::new();
        let mut file = SessionLog::open(&root.0).unwrap();
        file.append(&vec![b'a'; MAX_LOG_SIZE as usize - 4]).unwrap();
        file.append(b"12345").unwrap();
        file.append(b"last").unwrap();
        assert_eq!(fs::metadata(&file.path).unwrap().len(), MAX_LOG_SIZE);
        let bytes = fs::read(&file.path).unwrap();
        assert_eq!(&bytes[bytes.len() - 4..], b"last");
    }

    #[test]
    fn rejects_a_non_directory_logs_path() {
        let root = Scratch::new();
        fs::write(root.0.join("logs"), "occupied").unwrap();
        assert_eq!(
            SessionLog::open(&root.0).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_logs_root() {
        let root = Scratch::new();
        let destination = root.0.join("external");
        fs::create_dir(&destination).unwrap();
        std::os::unix::fs::symlink(destination, root.0.join("logs")).unwrap();
        assert_eq!(
            SessionLog::open(&root.0).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
