//! Native model executable process, with one long-lived correlated IPC session.
//! This owner does not install runtimes, interpret tensors, or infer model readiness from disk.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use super::native_speech::NativeSpeechCapability;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};
use uuid::Uuid;

const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_SPEECH_FRAME_BYTES: usize = 24 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeBackend {
    Cpu,
    Cuda,
    Wgpu,
    Onnx,
}

impl NativeBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Wgpu => "wgpu",
            Self::Onnx => "onnx",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePhase {
    Started,
    Loaded,
    Warm,
    Ready,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeFailure {
    Process,
    Timeout,
    Protocol,
    Worker,
}

impl NativeFailure {
    fn from_error(error: &NativeModelError) -> Self {
        match error {
            NativeModelError::Timeout => Self::Timeout,
            NativeModelError::Protocol => Self::Protocol,
            NativeModelError::Remote => Self::Worker,
            _ => Self::Process,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeModelSnapshot {
    pub model_id: String,
    pub backend: NativeBackend,
    pub session_id: Uuid,
    pub phase: NativePhase,
    pub last_failure: Option<NativeFailure>,
}

#[derive(Debug, Clone)]
pub struct NativeModelLaunch {
    pub model_id: String,
    pub executable: PathBuf,
    pub executable_sha256: [u8; 32],
    pub model_directory: PathBuf,
    pub runtime_directory: PathBuf,
    pub backend: NativeBackend,
    pub request_timeout: Duration,
    pub speech_contract_version: Option<u32>,
    pub speech_capabilities: Vec<NativeSpeechCapability>,
}

#[derive(Debug, Error)]
pub enum NativeModelError {
    #[error("invalid native model launch configuration: {0}")]
    Invalid(&'static str),
    #[error("native model filesystem/process failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("native model executable does not match its pinned SHA-256")]
    Integrity,
    #[error("native model did not respond before the deadline")]
    Timeout,
    #[error("native model process exited unexpectedly")]
    Exited,
    #[error("native model IPC protocol mismatch")]
    Protocol,
    #[error("native model operation rejected by worker")]
    Remote,
    #[error("native model operation is invalid in the current lifecycle phase")]
    Phase,
}

#[derive(Serialize)]
struct WorkerRequest<'a> {
    version: u32,
    id: Uuid,
    command: &'a str,
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerResponse {
    version: u32,
    id: Uuid,
    ok: bool,
    result: Value,
    error: Option<String>,
}

fn valid_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn check_path(path: &Path, directory: bool) -> Result<(), NativeModelError> {
    if !path.is_absolute() {
        return Err(NativeModelError::Invalid("paths must be absolute"));
    }
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() {
            return Err(NativeModelError::Invalid(
                "symlink in managed native model path",
            ));
        }
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(NativeModelError::Invalid("wrong native model path kind"));
    }
    Ok(())
}

impl NativeModelLaunch {
    pub fn validate(&self) -> Result<(), NativeModelError> {
        if !valid_model_id(&self.model_id) || self.request_timeout.is_zero() {
            return Err(NativeModelError::Invalid("model id or deadline"));
        }
        check_path(&self.executable, false)?;
        let mut input = std::fs::File::open(&self.executable)?;
        if input.metadata()?.len() > 512 * 1024 * 1024 {
            return Err(NativeModelError::Invalid(
                "native executable exceeds size limit",
            ));
        }
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buffer = [0u8; 65536];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        if digest.finish().as_ref() != self.executable_sha256 {
            return Err(NativeModelError::Integrity);
        }
        check_path(&self.model_directory, true)?;
        check_path(&self.runtime_directory, true)?;
        Ok(())
    }
}

async fn read_frame(
    stdout: &mut BufReader<ChildStdout>,
    max_bytes: usize,
) -> Result<Vec<u8>, NativeModelError> {
    let mut result = Vec::new();
    loop {
        let bytes = stdout.fill_buf().await?;
        if bytes.is_empty() {
            return Err(NativeModelError::Exited);
        }
        let count = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |index| index + 1);
        if result.len() + count > max_bytes {
            return Err(NativeModelError::Protocol);
        }
        result.extend_from_slice(&bytes[..count]);
        stdout.consume(count);
        if result.ends_with(b"\n") {
            return Ok(result);
        }
    }
}

async fn exchange(
    stdin: &mut BufWriter<ChildStdin>,
    stdout: &mut BufReader<ChildStdout>,
    command: &str,
    payload: Value,
    max_bytes: usize,
) -> Result<Value, NativeModelError> {
    let id = Uuid::new_v4();
    let request = serde_json::to_vec(&WorkerRequest {
        version: PROTOCOL_VERSION,
        id,
        command,
        payload,
    })
    .map_err(|_| NativeModelError::Protocol)?;
    if request.len() + 1 > max_bytes {
        return Err(NativeModelError::Protocol);
    }
    stdin.write_all(&request).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    let bytes = read_frame(stdout, max_bytes).await?;
    let response: WorkerResponse =
        serde_json::from_slice(&bytes).map_err(|_| NativeModelError::Protocol)?;
    if response.version != PROTOCOL_VERSION || response.id != id {
        return Err(NativeModelError::Protocol);
    }
    if !response.ok || response.error.is_some() {
        return Err(NativeModelError::Remote);
    }
    Ok(response.result)
}

pub struct NativeModelProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    launch: NativeModelLaunch,
    session_id: Uuid,
    phase: NativePhase,
    last_failure: Option<NativeFailure>,
}

impl NativeModelProcess {
    pub async fn start(launch: NativeModelLaunch) -> Result<Self, NativeModelError> {
        launch.validate()?;
        let mut command = Command::new(&launch.executable);
        command
            .arg("--ipc-stdio")
            .arg("--model-dir")
            .arg(&launch.model_directory)
            .arg("--runtime-dir")
            .arg(&launch.runtime_directory)
            .arg("--backend")
            .arg(launch.backend.as_str())
            .current_dir(&launch.model_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or(NativeModelError::Protocol)?;
        let stdout = child.stdout.take().ok_or(NativeModelError::Protocol)?;
        let mut process = Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            launch,
            session_id: Uuid::new_v4(),
            phase: NativePhase::Started,
            last_failure: None,
        };
        let capabilities = process.request("capabilities", json!({})).await?;
        let supported = capabilities
            .get("backends")
            .and_then(Value::as_array)
            .is_some_and(|backends| {
                backends
                    .iter()
                    .any(|backend| backend.as_str() == Some(process.launch.backend.as_str()))
            });
        let speech_attested = match process.launch.speech_contract_version {
            None => {
                capabilities.get("speech_contract_version").is_none()
                    && capabilities.get("speech_capabilities").is_none()
            }
            Some(version) => {
                capabilities
                    .get("speech_contract_version")
                    .and_then(Value::as_u64)
                    == Some(version as u64)
                    && capabilities
                        .get("speech_capabilities")
                        .cloned()
                        .and_then(|value| {
                            serde_json::from_value::<Vec<NativeSpeechCapability>>(value).ok()
                        })
                        .is_some_and(|attested| attested == process.launch.speech_capabilities)
            }
        };
        if capabilities.get("model_id").and_then(Value::as_str)
            != Some(process.launch.model_id.as_str())
            || !supported
            || !speech_attested
        {
            process.invalidate(NativeFailure::Protocol).await;
            return Err(NativeModelError::Protocol);
        }
        Ok(process)
    }

    pub fn speech_capabilities(&self) -> &[NativeSpeechCapability] {
        &self.launch.speech_capabilities
    }

    pub fn snapshot(&self) -> NativeModelSnapshot {
        NativeModelSnapshot {
            model_id: self.launch.model_id.clone(),
            backend: self.launch.backend,
            session_id: self.session_id,
            phase: self.phase,
            last_failure: self.last_failure,
        }
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    async fn invalidate(&mut self, cause: NativeFailure) {
        self.phase = NativePhase::Failed;
        self.last_failure = Some(cause);
        let _ = self.child.start_kill();
        let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
    }

    async fn request(&mut self, command: &str, payload: Value) -> Result<Value, NativeModelError> {
        if self.phase == NativePhase::Failed || self.phase == NativePhase::Stopped {
            return Err(NativeModelError::Phase);
        }
        match self.child.try_wait() {
            Ok(None) => (),
            Ok(Some(_)) => {
                self.invalidate(NativeFailure::Process).await;
                return Err(NativeModelError::Exited);
            }
            Err(error) => {
                self.invalidate(NativeFailure::Process).await;
                return Err(NativeModelError::Io(error));
            }
        }
        let speech_frame = command == "infer"
            && self.launch.speech_contract_version.is_some()
            && matches!(
                payload.get("kind").and_then(Value::as_str),
                Some("tts" | "asr")
            );
        let limit = if speech_frame {
            MAX_SPEECH_FRAME_BYTES
        } else {
            MAX_FRAME_BYTES
        };
        match timeout(
            self.launch.request_timeout,
            exchange(&mut self.stdin, &mut self.stdout, command, payload, limit),
        )
        .await
        {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                self.invalidate(NativeFailure::from_error(&error)).await;
                Err(error)
            }
            Err(_) => {
                self.invalidate(NativeFailure::Timeout).await;
                Err(NativeModelError::Timeout)
            }
        }
    }

    pub async fn load(&mut self) -> Result<(), NativeModelError> {
        if self.phase != NativePhase::Started {
            return Err(NativeModelError::Phase);
        }
        let result = self
            .request("load", json!({"model_id":self.launch.model_id}))
            .await?;
        if result.get("loaded").and_then(Value::as_bool) != Some(true) {
            self.invalidate(NativeFailure::Protocol).await;
            return Err(NativeModelError::Protocol);
        }
        self.phase = NativePhase::Loaded;
        Ok(())
    }

    pub async fn warmup(&mut self) -> Result<(), NativeModelError> {
        if self.phase != NativePhase::Loaded {
            return Err(NativeModelError::Phase);
        }
        let result = self.request("warmup", json!({})).await?;
        if result.get("warm").and_then(Value::as_bool) != Some(true) {
            self.invalidate(NativeFailure::Protocol).await;
            return Err(NativeModelError::Protocol);
        }
        self.phase = NativePhase::Warm;
        Ok(())
    }

    pub async fn refresh(&mut self) -> Result<NativeModelSnapshot, NativeModelError> {
        let result = self.request("status", json!({})).await?;
        if self.phase == NativePhase::Warm || self.phase == NativePhase::Ready {
            if result.get("phase").and_then(Value::as_str) != Some("ready")
                || result.get("model_id").and_then(Value::as_str)
                    != Some(self.launch.model_id.as_str())
            {
                self.invalidate(NativeFailure::Protocol).await;
                return Err(NativeModelError::Protocol);
            }
            self.phase = NativePhase::Ready;
        }
        Ok(self.snapshot())
    }

    pub async fn infer(&mut self, payload: Value) -> Result<Value, NativeModelError> {
        if self.phase != NativePhase::Ready {
            return Err(NativeModelError::Phase);
        }
        self.request("infer", payload).await
    }

    pub async fn unload(&mut self) -> Result<(), NativeModelError> {
        if !matches!(
            self.phase,
            NativePhase::Loaded | NativePhase::Warm | NativePhase::Ready
        ) {
            return Err(NativeModelError::Phase);
        }
        self.request("unload", json!({})).await?;
        self.phase = NativePhase::Started;
        Ok(())
    }

    pub async fn shutdown(mut self) -> Result<(), NativeModelError> {
        self.request("shutdown", json!({})).await?;
        match timeout(self.launch.request_timeout, self.child.wait()).await {
            Ok(Ok(status)) if status.success() => {
                self.phase = NativePhase::Stopped;
                Ok(())
            }
            Ok(Ok(_)) => {
                self.invalidate(NativeFailure::Process).await;
                Err(NativeModelError::Exited)
            }
            Ok(Err(error)) => {
                self.invalidate(NativeFailure::Process).await;
                Err(NativeModelError::Io(error))
            }
            Err(_) => {
                self.invalidate(NativeFailure::Timeout).await;
                Err(NativeModelError::Timeout)
            }
        }
    }
}
