//! Bounded HTTPS transfer for independently compiled native model packages.
//! Network content is untrusted; publication is delegated to the existing installer.

use std::{
    fs::{self, DirBuilder},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use reqwest::{Client, Url, header::ACCEPT_ENCODING, redirect::Policy};
use ring::digest::{Context, SHA256};
use serde_json::from_slice;
use thiserror::Error;
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::mpsc, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    native_model::NativeBackend,
    native_package_install::NativePackageInstallResult,
    native_registry::{
        NativeModelManifest, NativeRegistryError, check_path, parse_sha256, valid_identifier,
        validate_manifest,
    },
    native_revisions::{NativeRevisionError, NativeRevisionStore},
    native_trust::{NativeReleaseTrust, NativeTrustError},
};

const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_TOTAL_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const MAX_BASE_URL_BYTES: usize = 2048;

/// A progress observation is transfer telemetry, not an installation receipt or ModelReady.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTransferPhase {
    Manifest,
    Downloading,
    Publishing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeTransferProgress {
    pub phase: NativeTransferPhase,
    pub transferred_bytes: u64,
    pub planned_bytes: Option<u64>,
}

fn notify(
    progress: Option<&mpsc::UnboundedSender<NativeTransferProgress>>,
    status: NativeTransferProgress,
) {
    if let Some(sender) = progress {
        let _ = sender.send(status);
    }
}

#[derive(Debug, Error)]
pub enum NativeHttpsError {
    #[error("HTTPS native source is invalid: {0}")]
    Invalid(&'static str),
    #[error("native model download was canceled")]
    Canceled,
    #[error("native model download exceeded its time limit")]
    Timeout,
    #[error("native model server did not return HTTP 200")]
    HttpStatus,
    #[error("native download size or SHA-256 does not match its approved manifest")]
    Integrity,
    #[error("native model package exceeds its bounded download budget")]
    TooLarge,
    #[error("native HTTPS transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error(transparent)]
    Trust(#[from] NativeTrustError),
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
    #[error(transparent)]
    Revisions(#[from] NativeRevisionError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("native installer task failed")]
    InstallerTask,
}

/// A transport endpoint chosen by the application, not an origin found in the
/// native model manifest. Redirects and URL credentials are never permitted.
#[derive(Debug, Clone)]
pub struct NativeHttpsOrigin(Url);

impl NativeHttpsOrigin {
    pub fn parse(raw: &str) -> Result<Self, NativeHttpsError> {
        let parsed = Url::parse(raw).map_err(|_| NativeHttpsError::Invalid("invalid URL"))?;
        if raw.len() > MAX_BASE_URL_BYTES
            || parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.path().ends_with('/')
            || parsed.path().contains("//")
            || parsed.path().contains('%')
            || parsed
                .path_segments()
                .is_none_or(|mut s| s.any(|part| part == "." || part == ".."))
        {
            return Err(NativeHttpsError::Invalid("invalid HTTPS origin"));
        }
        Ok(Self(parsed))
    }
}

/// Exact publisher-approved model identity. The origin is supplied separately
/// by trusted application configuration, not by downloaded model metadata.
pub struct NativeHttpsPackageRequest<'a> {
    pub model_id: &'a str,
    pub manifest_sha256: [u8; 32],
    pub backend: NativeBackend,
    pub origin: &'a NativeHttpsOrigin,
}

#[derive(Clone, Copy)]
struct DownloadSpec<'a> {
    model_id: &'a str,
    manifest_sha256: [u8; 32],
    backend: NativeBackend,
}

/// Downloads a specifically authorized native revision to an exclusive, disposable
/// staging directory. The local installer still performs a second full hash pass
/// and publishes the model directory last. Cancellation is honored until that
/// synchronous publication begins; publication itself is deliberately atomic.
pub struct NativeHttpsPackageTransfer {
    client: Client,
    file_timeout: Duration,
}

impl NativeHttpsPackageTransfer {
    pub fn new(file_timeout: Duration) -> Result<Self, NativeHttpsError> {
        if file_timeout.is_zero() || file_timeout > Duration::from_secs(3600) {
            return Err(NativeHttpsError::Invalid("invalid per-file timeout"));
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(file_timeout)
            .build()?;
        Ok(Self {
            client,
            file_timeout,
        })
    }

    pub async fn download_and_install(
        &self,
        revisions: &NativeRevisionStore,
        trust: &NativeReleaseTrust,
        request: NativeHttpsPackageRequest<'_>,
        staging_root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<NativePackageInstallResult, NativeHttpsError> {
        let spec = DownloadSpec {
            model_id: request.model_id,
            manifest_sha256: request.manifest_sha256,
            backend: request.backend,
        };
        self.download_from_origin(
            revisions,
            trust,
            spec,
            &request.origin.0,
            staging_root,
            cancellation,
        )
        .await
    }

    /// Streams bounded transfer observations. Publication is a separate stage;
    /// reaching the byte total never implies the model was installed or started.
    pub async fn download_and_install_observed(
        &self,
        revisions: &NativeRevisionStore,
        trust: &NativeReleaseTrust,
        request: NativeHttpsPackageRequest<'_>,
        staging_root: &Path,
        cancellation: &CancellationToken,
        progress: &mpsc::UnboundedSender<NativeTransferProgress>,
    ) -> Result<NativePackageInstallResult, NativeHttpsError> {
        self.download_from_origin_observed(
            revisions,
            trust,
            DownloadSpec {
                model_id: request.model_id,
                manifest_sha256: request.manifest_sha256,
                backend: request.backend,
            },
            &request.origin.0,
            staging_root,
            cancellation,
            Some(progress),
        )
        .await
    }

    async fn download_from_origin(
        &self,
        revisions: &NativeRevisionStore,
        trust: &NativeReleaseTrust,
        spec: DownloadSpec<'_>,
        origin: &Url,
        staging_root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<NativePackageInstallResult, NativeHttpsError> {
        self.download_from_origin_observed(
            revisions,
            trust,
            spec,
            origin,
            staging_root,
            cancellation,
            None,
        )
        .await
    }

    async fn download_from_origin_observed(
        &self,
        revisions: &NativeRevisionStore,
        trust: &NativeReleaseTrust,
        spec: DownloadSpec<'_>,
        origin: &Url,
        staging_root: &Path,
        cancellation: &CancellationToken,
        progress: Option<&mpsc::UnboundedSender<NativeTransferProgress>>,
    ) -> Result<NativePackageInstallResult, NativeHttpsError> {
        let DownloadSpec {
            model_id,
            manifest_sha256,
            backend,
        } = spec;
        if !valid_identifier(model_id) {
            return Err(NativeHttpsError::Invalid("invalid native model ID"));
        }
        ensure_not_cancelled(cancellation)?;
        trust.verify(model_id, &manifest_sha256)?;
        revisions.verify_download_staging_root(staging_root)?;
        let stage = DownloadStage::new(staging_root, model_id)?;
        notify(
            progress,
            NativeTransferProgress {
                phase: NativeTransferPhase::Manifest,
                transferred_bytes: 0,
                planned_bytes: None,
            },
        );
        let manifest_url = artifact_url(origin, "packages", Some(model_id), "native-model.json")?;
        let manifest_path = stage.package.join("native-model.json");
        self.fetch_verified(
            manifest_url,
            &manifest_path,
            &manifest_sha256,
            None,
            MAX_MANIFEST_BYTES,
            cancellation,
            None,
        )
        .await?;
        let bytes = tokio::fs::read(&manifest_path).await?;
        let manifest: NativeModelManifest = from_slice(&bytes)?;
        validate_manifest(&manifest)?;
        if manifest.model_id != model_id
            || manifest.os != std::env::consts::OS
            || manifest.arch != std::env::consts::ARCH
            || !manifest.backends.contains(&backend)
        {
            return Err(NativeHttpsError::Invalid("model/backend/platform mismatch"));
        }
        let manifest_len = bytes.len() as u64;
        let mut total_bytes = manifest_len;
        for file in std::iter::once(&manifest.executable).chain(manifest.assets.iter()) {
            total_bytes = budget(total_bytes, file.size_bytes)?;
        }
        for file in manifest
            .runtime_files
            .iter()
            .filter(|entry| entry.backends.contains(&backend))
        {
            total_bytes = budget(total_bytes, file.file.size_bytes)?;
        }
        let planned = total_bytes - manifest_len;
        let mut transferred = 0u64;
        notify(
            progress,
            NativeTransferProgress {
                phase: NativeTransferPhase::Downloading,
                transferred_bytes: 0,
                planned_bytes: Some(planned),
            },
        );
        let package_files = std::iter::once(&manifest.executable).chain(manifest.assets.iter());
        for file in package_files {
            let url = artifact_url(origin, "packages", Some(model_id), &file.path)?;
            let dest = create_file_parent(&stage.package, &file.path)?;
            let expected = parse_sha256(&file.sha256)?;
            self.fetch_verified(
                url,
                &dest,
                &expected,
                Some(file.size_bytes),
                file.size_bytes,
                cancellation,
                progress.map(|sink| (sink, transferred, planned)),
            )
            .await?;
            transferred += file.size_bytes;
            if file.path == manifest.executable.path {
                set_executable_permissions(&dest)?;
            }
        }
        for runtime in &manifest.runtime_files {
            if !runtime.backends.contains(&backend) {
                continue;
            }
            let file = &runtime.file;
            let url = artifact_url(origin, "runtime", None, &file.path)?;
            let dest = create_file_parent(&stage.runtime, &file.path)?;
            let expected = parse_sha256(&file.sha256)?;
            self.fetch_verified(
                url,
                &dest,
                &expected,
                Some(file.size_bytes),
                file.size_bytes,
                cancellation,
                progress.map(|sink| (sink, transferred, planned)),
            )
            .await?;
            transferred += file.size_bytes;
        }
        ensure_not_cancelled(cancellation)?;
        let approved = trust.verify(model_id, &manifest_sha256)?;
        notify(
            progress,
            NativeTransferProgress {
                phase: NativeTransferPhase::Publishing,
                transferred_bytes: transferred,
                planned_bytes: Some(planned),
            },
        );
        let revisions = revisions.clone();
        let cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            ensure_not_cancelled(&cancellation)?;
            revisions
                .install_verified_local(&stage.packages_root, &stage.runtime, &approved, backend)
                .map_err(Into::into)
        })
        .await
        .map_err(|_| NativeHttpsError::InstallerTask)?
    }

    async fn fetch_verified(
        &self,
        url: Url,
        dest: &Path,
        expected_digest: &[u8; 32],
        exact_size: Option<u64>,
        max_size: u64,
        cancellation: &CancellationToken,
        observation: Option<(&mpsc::UnboundedSender<NativeTransferProgress>, u64, u64)>,
    ) -> Result<(), NativeHttpsError> {
        ensure_not_cancelled(cancellation)?;
        let future = async {
            let mut response = self
                .client
                .get(url)
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(NativeHttpsError::HttpStatus);
            }
            if let Some(length) = response.content_length() {
                if length > max_size || exact_size.is_some_and(|expected| length != expected) {
                    return Err(NativeHttpsError::Integrity);
                }
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest)
                .await?;
            let mut context = Context::new(&SHA256);
            let mut total = 0u64;
            let mut last_report = Instant::now();
            while let Some(chunk) = response.chunk().await? {
                total = total
                    .checked_add(chunk.len() as u64)
                    .ok_or(NativeHttpsError::TooLarge)?;
                if total > max_size {
                    return Err(NativeHttpsError::TooLarge);
                }
                context.update(&chunk);
                output.write_all(&chunk).await?;
                if last_report.elapsed() >= Duration::from_millis(250) {
                    if let Some((sink, earlier, planned)) = observation {
                        notify(
                            Some(sink),
                            NativeTransferProgress {
                                phase: NativeTransferPhase::Downloading,
                                transferred_bytes: earlier + total,
                                planned_bytes: Some(planned),
                            },
                        );
                    }
                    last_report = Instant::now();
                }
            }
            if total == 0
                || exact_size.is_some_and(|expected| expected != total)
                || context.finish().as_ref() != expected_digest
            {
                return Err(NativeHttpsError::Integrity);
            }
            output.sync_all().await?;
            if let Some((sink, earlier, planned)) = observation {
                notify(
                    Some(sink),
                    NativeTransferProgress {
                        phase: NativeTransferPhase::Downloading,
                        transferred_bytes: earlier + total,
                        planned_bytes: Some(planned),
                    },
                );
            }
            Ok(())
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(NativeHttpsError::Canceled),
            result = timeout(self.file_timeout, future) => {
                result.map_err(|_| NativeHttpsError::Timeout)?
            }
        }
    }
}

fn budget(accumulated: u64, added: u64) -> Result<u64, NativeHttpsError> {
    let new_size = accumulated
        .checked_add(added)
        .ok_or(NativeHttpsError::TooLarge)?;
    if new_size > MAX_TOTAL_DOWNLOAD_BYTES {
        return Err(NativeHttpsError::TooLarge);
    }
    Ok(new_size)
}

fn ensure_not_cancelled(token: &CancellationToken) -> Result<(), NativeHttpsError> {
    if token.is_cancelled() {
        Err(NativeHttpsError::Canceled)
    } else {
        Ok(())
    }
}

fn artifact_url(
    origin: &Url,
    kind: &str,
    model_id: Option<&str>,
    path: &str,
) -> Result<Url, NativeHttpsError> {
    let mut url = origin.clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| NativeHttpsError::Invalid("invalid endpoint path"))?;
        segments.pop_if_empty();
        segments.push(kind);
        if let Some(model) = model_id {
            segments.push(model);
        }
        for segment in path.split('/') {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn create_file_parent(root: &Path, relative: &str) -> Result<PathBuf, NativeHttpsError> {
    let mut dest = root.to_path_buf();
    let mut segments = relative.split('/').peekable();
    while let Some(segment) = segments.next() {
        dest.push(segment);
        if segments.peek().is_some() {
            match fs::create_dir(&dest) {
                Ok(()) => (),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => (),
                Err(err) => return Err(err.into()),
            }
            check_path(&dest, true)?;
        }
    }
    Ok(dest)
}

fn set_executable_permissions(path: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

struct DownloadStage {
    root: PathBuf,
    packages_root: PathBuf,
    package: PathBuf,
    runtime: PathBuf,
}

impl DownloadStage {
    fn new(parent: &Path, model_id: &str) -> Result<Self, NativeHttpsError> {
        let root = parent.join(format!(".native-download-{}", Uuid::new_v4()));
        private_create_dir(&root)?;
        let stage = Self {
            packages_root: root.join("packages"),
            package: root.join("packages").join(model_id),
            runtime: root.join("runtime"),
            root,
        };
        private_create_dir(&stage.packages_root)?;
        private_create_dir(&stage.runtime)?;
        private_create_dir(&stage.package)?;
        Ok(stage)
    }
}

impl Drop for DownloadStage {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.root)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn private_create_dir(path: &Path) -> Result<(), io::Error> {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use base64::{Engine, engine::general_purpose::STANDARD};
    use ring::{
        digest::{SHA256, digest},
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;
    use crate::ai_runtime::{
        native_approval::{SignedNativeRelease, signing_message},
        native_registry::{NativeFileRef, NativeRuntimeFile},
        native_trust::NativePolicyPin,
    };

    fn sha(bytes: &[u8]) -> [u8; 32] {
        let mut result = [0u8; 32];
        result.copy_from_slice(digest(&SHA256, bytes).as_ref());
        result
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    struct Fixture {
        root: PathBuf,
        staging: PathBuf,
        store: NativeRevisionStore,
        trust: NativeReleaseTrust,
        manifest_sha256: [u8; 32],
        responses: HashMap<String, Vec<u8>>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("native-https-{}", Uuid::new_v4()));
            let staging = root.join("staging");
            let revisions = root.join("revisions");
            let runtime = root.join("runtime");
            let approvals = root.join("approvals");
            let config = root.join("config");
            for dir in [&staging, &revisions, &runtime, &approvals, &config] {
                fs::create_dir_all(dir).unwrap();
            }
            fs::create_dir(approvals.join("voice")).unwrap();

            let keypair = Ed25519KeyPair::from_pkcs8(
                Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .unwrap()
                    .as_ref(),
            )
            .unwrap();
            let policy = serde_json::json!({
                "schema_version": 1,
                "publishers": [{
                    "publisher_id": "sample-author",
                    "ed25519_public_key_base64": STANDARD.encode(keypair.public_key().as_ref()),
                }],
            });
            let policy_bytes = serde_json::to_vec(&policy).unwrap();
            let policy_path = config.join("native-publisher-keys.json");
            fs::write(&policy_path, &policy_bytes).unwrap();
            let pin = NativePolicyPin::from_sha256_hex(&hex(&sha(&policy_bytes))).unwrap();

            let executable = b"test-worker-executable";
            let weights = b"test-model-weights";
            let runtime_file = b"test-native-runtime";
            let manifest = NativeModelManifest {
                schema_version: 1,
                model_id: "voice".into(),
                display_name: "Native fixture".into(),
                package_version: "1.0".into(),
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                protocol_version: 1,
                backends: vec![NativeBackend::Cpu, NativeBackend::Cuda],
                executable: NativeFileRef {
                    path: "bin/worker".into(),
                    size_bytes: executable.len() as u64,
                    sha256: hex(&sha(executable)),
                },
                assets: vec![NativeFileRef {
                    path: "weights/model.bin".into(),
                    size_bytes: weights.len() as u64,
                    sha256: hex(&sha(weights)),
                }],
                runtime_files: vec![
                    NativeRuntimeFile {
                        backends: vec![NativeBackend::Cpu],
                        file: NativeFileRef {
                            path: "lib/runtime.so".into(),
                            size_bytes: runtime_file.len() as u64,
                            sha256: hex(&sha(runtime_file)),
                        },
                    },
                    NativeRuntimeFile {
                        backends: vec![NativeBackend::Cuda],
                        file: NativeFileRef {
                            path: "cuda/cublas-stub.dll".into(),
                            size_bytes: 8,
                            sha256: hex(&sha(b"cuda-stub")),
                        },
                    },
                ],
            };
            let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
            let manifest_sha256 = sha(&manifest_bytes);
            let signed = SignedNativeRelease {
                schema_version: 1,
                publisher_id: "sample-author".into(),
                model_id: "voice".into(),
                manifest_sha256: hex(&manifest_sha256),
                signature_base64: STANDARD.encode(
                    keypair
                        .sign(
                            signing_message("sample-author", "voice", &hex(&manifest_sha256))
                                .as_bytes(),
                        )
                        .as_ref(),
                ),
            };
            fs::write(
                approvals
                    .join("voice")
                    .join(format!("{}.json", hex(&manifest_sha256))),
                serde_json::to_vec(&signed).unwrap(),
            )
            .unwrap();
            let trust = NativeReleaseTrust::new(policy_path, approvals, Some(pin));
            let responses = HashMap::from([
                (
                    "/release/packages/voice/native-model.json".into(),
                    manifest_bytes,
                ),
                (
                    "/release/packages/voice/bin/worker".into(),
                    executable.to_vec(),
                ),
                (
                    "/release/packages/voice/weights/model.bin".into(),
                    weights.to_vec(),
                ),
                (
                    "/release/runtime/lib/runtime.so".into(),
                    runtime_file.to_vec(),
                ),
            ]);
            Self {
                root,
                staging,
                store: NativeRevisionStore::new(revisions, runtime),
                trust,
                manifest_sha256,
                responses,
            }
        }

        fn no_staging(&self) -> bool {
            fs::read_dir(&self.staging).unwrap().next().is_none()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct LocalServer {
        base: Url,
        task: JoinHandle<()>,
    }

    impl LocalServer {
        async fn start(responses: HashMap<String, Vec<u8>>, redirect: bool) -> Self {
            Self::start_with_delay(responses, redirect, Duration::ZERO).await
        }

        async fn start_with_delay(
            responses: HashMap<String, Vec<u8>>,
            redirect: bool,
            response_delay: Duration,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let responses = Arc::new(responses);
            let task = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let responses = Arc::clone(&responses);
                    tokio::spawn(async move {
                        let mut request = [0u8; 4096];
                        let n = socket.read(&mut request).await.unwrap_or(0);
                        let request = String::from_utf8_lossy(&request[..n]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("");
                        let body = responses.get(path);
                        let (status, payload, extra) = if redirect {
                            (
                                "302 Found",
                                &[][..],
                                "Location: https://example.com/evil\r\n",
                            )
                        } else if let Some(body) = body {
                            ("200 OK", body.as_slice(), "")
                        } else {
                            ("404 Not Found", &[][..], "")
                        };
                        let headers = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                            payload.len(),
                        );
                        let _ = socket.write_all(headers.as_bytes()).await;
                        if !response_delay.is_zero() {
                            tokio::time::sleep(response_delay).await;
                        }
                        let _ = socket.write_all(payload).await;
                    });
                }
            });
            Self {
                base: Url::parse(&format!("http://{addr}/release/")).unwrap(),
                task,
            }
        }
    }

    impl Drop for LocalServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn run(
        fixture: &Fixture,
        base: &Url,
        token: &CancellationToken,
    ) -> Result<NativePackageInstallResult, NativeHttpsError> {
        NativeHttpsPackageTransfer::new(Duration::from_secs(10))
            .unwrap()
            .download_from_origin(
                &fixture.store,
                &fixture.trust,
                DownloadSpec {
                    model_id: "voice",
                    manifest_sha256: fixture.manifest_sha256,
                    backend: NativeBackend::Cpu,
                },
                base,
                &fixture.staging,
                token,
            )
            .await
    }

    #[test]
    fn only_https_without_redirect_prone_url_metadata_is_publicly_allowed() {
        assert!(NativeHttpsOrigin::parse("https://releases.example.org/models/").is_ok());
        for bad in [
            "http://releases.example.org/models/",
            "https://user:pass@releases.example.org/models/",
            "https://releases.example.org/models/?key=secret",
            "https://releases.example.org/models/#fragment",
            "https://releases.example.org/models",
            "https://releases.example.org/models//",
            "file:///tmp/models/",
        ] {
            assert!(NativeHttpsOrigin::parse(bad).is_err(), "accepted {bad}");
        }
        let parsed = NativeHttpsOrigin::parse("https://releases.example.org/downloads/").unwrap();
        assert_eq!(
            artifact_url(&parsed.0, "packages", Some("voice"), "weights/model.bin")
                .unwrap()
                .as_str(),
            "https://releases.example.org/downloads/packages/voice/weights/model.bin"
        );
    }

    #[tokio::test]
    async fn signed_manifest_and_files_can_be_downloaded_and_atomically_published() {
        let fixture = Fixture::new();
        let server = LocalServer::start(fixture.responses.clone(), false).await;
        let token = CancellationToken::new();
        let result = run(&fixture, &server.base, &token).await.unwrap();
        assert_eq!(result.model_id, "voice");
        assert_eq!(result.manifest_sha256, fixture.manifest_sha256);
        assert!(!result.reused_package);
        assert_eq!(result.new_runtime_files, 1);
        assert_eq!(fixture.store.overview().unwrap().revisions.len(), 1);
        assert!(fixture.no_staging());
        let second = run(&fixture, &server.base, &token).await.unwrap();
        assert!(second.reused_package);
        assert!(fixture.no_staging());
    }

    #[tokio::test]
    async fn progress_reports_real_bytes_then_atomic_publication_not_ready() {
        let fixture = Fixture::new();
        let server = LocalServer::start(fixture.responses.clone(), false).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();
        let installed = NativeHttpsPackageTransfer::new(Duration::from_secs(10))
            .unwrap()
            .download_from_origin_observed(
                &fixture.store,
                &fixture.trust,
                DownloadSpec {
                    model_id: "voice",
                    manifest_sha256: fixture.manifest_sha256,
                    backend: NativeBackend::Cpu,
                },
                &server.base,
                &fixture.staging,
                &token,
                Some(&tx),
            )
            .await
            .unwrap();
        assert_eq!(installed.model_id, "voice");
        drop(tx);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        assert_eq!(events[0].phase, NativeTransferPhase::Manifest);
        assert_eq!(events[0].planned_bytes, None);
        let expected = (b"test-worker-executable".len()
            + b"test-model-weights".len()
            + b"test-native-runtime".len()) as u64;
        assert_eq!(events[1].planned_bytes, Some(expected));
        let last = events.last().unwrap();
        assert_eq!(last.phase, NativeTransferPhase::Publishing);
        assert_eq!(last.transferred_bytes, expected);
        assert_eq!(last.planned_bytes, Some(expected));
        assert!(
            events
                .iter()
                .filter(|e| e.phase == NativeTransferPhase::Downloading)
                .all(|e| e.transferred_bytes <= expected)
        );
        assert!(fixture.no_staging());
    }

    #[tokio::test]
    async fn canceled_observed_transfer_does_not_emit_publication() {
        let fixture = Fixture::new();
        let server = LocalServer::start_with_delay(
            fixture.responses.clone(),
            false,
            Duration::from_millis(300),
        )
        .await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();
        let canceller = token.clone();
        let cancel = async move {
            tokio::time::sleep(Duration::from_millis(35)).await;
            canceller.cancel();
        };
        let fetch = NativeHttpsPackageTransfer::new(Duration::from_secs(10)).unwrap();
        let (result, _) = tokio::join!(
            fetch.download_from_origin_observed(
                &fixture.store,
                &fixture.trust,
                DownloadSpec {
                    model_id: "voice",
                    manifest_sha256: fixture.manifest_sha256,
                    backend: NativeBackend::Cpu,
                },
                &server.base,
                &fixture.staging,
                &token,
                Some(&tx),
            ),
            cancel,
        );
        assert!(matches!(result, Err(NativeHttpsError::Canceled)));
        drop(tx);
        while let Ok(event) = rx.try_recv() {
            assert_ne!(event.phase, NativeTransferPhase::Publishing);
        }
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
        assert!(fixture.no_staging());
    }

    #[tokio::test]
    async fn corrupted_asset_is_not_published_and_temp_files_are_removed() {
        let fixture = Fixture::new();
        let mut responses = fixture.responses.clone();
        responses.insert(
            "/release/packages/voice/weights/model.bin".into(),
            b"test-model-wrights".to_vec(),
        );
        let server = LocalServer::start(responses, false).await;
        let result = run(&fixture, &server.base, &CancellationToken::new()).await;
        assert!(matches!(result, Err(NativeHttpsError::Integrity)));
        assert!(fixture.no_staging());
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
    }

    #[tokio::test]
    async fn redirects_cannot_leak_requests_or_publish_a_partial_model() {
        let fixture = Fixture::new();
        let server = LocalServer::start(fixture.responses.clone(), true).await;
        assert!(matches!(
            run(&fixture, &server.base, &CancellationToken::new()).await,
            Err(NativeHttpsError::HttpStatus)
        ));
        assert!(fixture.no_staging());
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
    }

    #[tokio::test]
    async fn canceled_transfer_never_publishes_or_starts_a_request() {
        let fixture = Fixture::new();
        let server = LocalServer::start(fixture.responses.clone(), false).await;
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            run(&fixture, &server.base, &token).await,
            Err(NativeHttpsError::Canceled)
        ));
        assert!(fixture.no_staging());
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
    }

    #[tokio::test]
    async fn cancellation_during_a_pending_stream_cleans_up_download_staging() {
        let fixture = Fixture::new();
        let server = LocalServer::start_with_delay(
            fixture.responses.clone(),
            false,
            Duration::from_millis(350),
        )
        .await;
        let token = CancellationToken::new();
        let canceller = token.clone();
        let task = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        };
        let (result, _) = tokio::join!(run(&fixture, &server.base, &token), task);
        assert!(matches!(result, Err(NativeHttpsError::Canceled)));
        assert!(fixture.no_staging());
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
    }

    #[tokio::test]
    async fn incorrect_manifest_bytes_never_enter_artifact_download() {
        let fixture = Fixture::new();
        let mut responses = fixture.responses.clone();
        responses.insert(
            "/release/packages/voice/native-model.json".into(),
            b"{\"schema_version\":1}".to_vec(),
        );
        let server = LocalServer::start(responses, false).await;
        assert!(matches!(
            run(&fixture, &server.base, &CancellationToken::new()).await,
            Err(NativeHttpsError::Integrity)
        ));
        assert!(fixture.no_staging());
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
    }

    #[tokio::test]
    async fn oversized_artifact_is_rejected_without_publishing_revision() {
        let fixture = Fixture::new();
        let mut responses = fixture.responses.clone();
        responses.insert(
            "/release/packages/voice/weights/model.bin".into(),
            vec![b'Z'; 200],
        );
        let server = LocalServer::start(responses, false).await;
        assert!(matches!(
            run(&fixture, &server.base, &CancellationToken::new()).await,
            Err(NativeHttpsError::Integrity | NativeHttpsError::TooLarge)
        ));
        assert!(fixture.no_staging());
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
    }

    #[tokio::test]
    async fn staging_inside_revision_store_is_rejected_before_network() {
        let fixture = Fixture::new();
        let server = LocalServer::start(fixture.responses.clone(), false).await;
        let result = NativeHttpsPackageTransfer::new(Duration::from_secs(3))
            .unwrap()
            .download_from_origin(
                &fixture.store,
                &fixture.trust,
                DownloadSpec {
                    model_id: "voice",
                    manifest_sha256: fixture.manifest_sha256,
                    backend: NativeBackend::Cpu,
                },
                &server.base,
                &fixture.root.join("revisions"),
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(NativeHttpsError::Revisions(_))));
        assert!(fixture.store.overview().unwrap().revisions.is_empty());
    }

    #[tokio::test]
    async fn untrusted_or_changed_policy_prevents_download_and_publication() {
        let fixture = Fixture::new();
        let server = LocalServer::start(fixture.responses.clone(), false).await;
        let policy = fixture.root.join("config/native-publisher-keys.json");
        fs::write(policy, b"{\"schema_version\":1,\"publishers\":[]}").unwrap();
        assert!(matches!(
            run(&fixture, &server.base, &CancellationToken::new()).await,
            Err(NativeHttpsError::Trust(_))
        ));
        assert!(fixture.no_staging());
    }
}
