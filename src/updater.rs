//! Verified staging for application release assets. Installation and activation are separate.

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{Client, Url, redirect::Policy};
use ring::digest::{Context, SHA256};
use serde::Deserialize;
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub mod activation;
pub mod check;
pub mod handoff;

use crate::portable::PortableDataRoot;

const MAX_PACKAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_RELEASE_JSON_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid release metadata: {0}")]
    Metadata(String),
    #[error("release asset has no publisher SHA-256 digest")]
    MissingDigest,
    #[error("update package exceeds the configured size limit")]
    SizeLimit,
    #[error("downloaded package does not match its declared size")]
    SizeMismatch,
    #[error("downloaded package does not match the publisher SHA-256 digest")]
    DigestMismatch,
    #[error("a previously staged artifact failed integrity verification: {0}")]
    CorruptStage(PathBuf),
    #[error("download cancelled before artifact publication")]
    Cancelled,
    #[error("update staging path cannot be a symlink: {0}")]
    Symlink(PathBuf),
    #[error("release network request failed")]
    Network(#[from] reqwest::Error),
    #[error("update staging I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct UpdateAsset {
    name: String,
    url: Url,
    size: u64,
    digest: [u8; 32],
}

impl UpdateAsset {
    pub fn new(
        name: &str,
        url: &str,
        size: u64,
        publisher_digest: &str,
    ) -> Result<Self, UpdateError> {
        if name.is_empty() || name.len() > 255 || name.contains(['/', '\\', '\0']) {
            return Err(UpdateError::Metadata("unsafe asset name".into()));
        }
        if size == 0 || size > MAX_PACKAGE_BYTES {
            return Err(UpdateError::SizeLimit);
        }
        let url = Url::parse(url).map_err(|_| UpdateError::Metadata("invalid asset URL".into()))?;
        if !safe_download_url(&url) {
            return Err(UpdateError::Metadata("asset URL must use HTTPS".into()));
        }
        let Some(raw) = publisher_digest.strip_prefix("sha256:") else {
            return Err(UpdateError::Metadata("expected sha256: digest".into()));
        };
        if raw.len() != 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(UpdateError::Metadata("invalid SHA-256 digest".into()));
        }
        let mut digest = [0u8; 32];
        for (index, pair) in raw.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let hex = std::str::from_utf8(pair).expect("validated ASCII hex");
            digest[index] = u8::from_str_radix(hex, 16).expect("validated ASCII hex");
        }
        Ok(Self {
            name: name.into(),
            url,
            size,
            digest,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn sha256(&self) -> String {
        hex_digest(&self.digest)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedArtifact {
    path: PathBuf,
    size: u64,
    sha256: String,
    reused: bool,
}

impl StagedArtifact {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn reused(&self) -> bool {
        self.reused
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CheckedRelease {
    pub tag: String,
    pub assets: Vec<UpdateAsset>,
}

pub struct GithubUpdateCatalog {
    repository: String,
    client: Client,
}

impl GithubUpdateCatalog {
    pub fn new(repository: &str) -> Result<Self, UpdateError> {
        let mut parts = repository.split('/');
        if parts.clone().count() != 2 || !parts.all(valid_github_segment) {
            return Err(UpdateError::Metadata("invalid GitHub repository".into()));
        }
        Ok(Self {
            repository: repository.into(),
            client: secured_client()?,
        })
    }

    pub async fn release_assets(&self, tag: &str) -> Result<CheckedRelease, UpdateError> {
        if !valid_github_segment(tag) {
            return Err(UpdateError::Metadata("invalid release tag".into()));
        }
        let url = format!(
            "https://api.github.com/repos/{}/releases/tags/{tag}",
            self.repository
        );
        let bytes = self.fetch_metadata(url).await?;
        self.parse_release(Some(tag), &bytes)
    }

    pub async fn latest_release(&self) -> Result<CheckedRelease, UpdateError> {
        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.repository
        );
        let bytes = self.fetch_metadata(url).await?;
        self.parse_release(None, &bytes)
    }

    async fn fetch_metadata(&self, url: String) -> Result<Vec<u8>, UpdateError> {
        let mut response = self.client.get(url).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RELEASE_JSON_BYTES as u64)
        {
            return Err(UpdateError::Metadata(
                "release metadata is too large".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > MAX_RELEASE_JSON_BYTES.saturating_sub(bytes.len()) {
                return Err(UpdateError::Metadata(
                    "release metadata is too large".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    fn parse_release(
        &self,
        expected_tag: Option<&str>,
        json: &[u8],
    ) -> Result<CheckedRelease, UpdateError> {
        let release: GithubRelease = serde_json::from_slice(json)
            .map_err(|error| UpdateError::Metadata(format!("invalid GitHub release: {error}")))?;
        if release.draft
            || !valid_github_segment(&release.tag_name)
            || expected_tag.is_some_and(|tag| release.tag_name != tag)
        {
            return Err(UpdateError::Metadata(
                "release tag mismatch or draft release".into(),
            ));
        }
        let mut names = HashSet::new();
        let assets = release
            .assets
            .into_iter()
            .map(|asset| {
                if !names.insert(asset.name.clone()) {
                    return Err(UpdateError::Metadata("duplicate asset name".into()));
                }
                let Some(digest) = asset.digest else {
                    return Err(UpdateError::MissingDigest);
                };
                UpdateAsset::new(
                    &asset.name,
                    &asset.browser_download_url,
                    asset.size,
                    &digest,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CheckedRelease {
            tag: release.tag_name,
            assets,
        })
    }
}

pub struct UpdateStager {
    stage_directory: PathBuf,
    client: Client,
}

impl UpdateStager {
    pub fn new(data_root: &PortableDataRoot) -> Result<Self, UpdateError> {
        Ok(Self {
            stage_directory: data_root.path().join("updates").join("staged"),
            client: secured_client()?,
        })
    }

    pub async fn stage(
        &self,
        asset: &UpdateAsset,
        cancel: &CancellationToken,
    ) -> Result<StagedArtifact, UpdateError> {
        if cancel.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        self.prepare_directory().await?;
        let digest = asset.sha256();
        let final_path = self
            .stage_directory
            .join(format!("sha256-{digest}.artifact"));
        if fs::try_exists(&final_path).await? {
            tokio::select! {
                _ = cancel.cancelled() => return Err(UpdateError::Cancelled),
                result = self.verify_file(&final_path, asset) => result
                    .map_err(|_| UpdateError::CorruptStage(final_path.clone()))?,
            }
            return Ok(StagedArtifact {
                path: final_path,
                size: asset.size,
                sha256: digest,
                reused: true,
            });
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(UpdateError::Cancelled),
            result = self.client.get(asset.url.clone()).send() => result.map_err(UpdateError::Network)?,
        }
        .error_for_status().map_err(UpdateError::Network)?;
        if response
            .content_length()
            .is_some_and(|length| length != asset.size)
        {
            return Err(UpdateError::SizeMismatch);
        }
        let temp_path = self
            .stage_directory
            .join(format!(".download-{}.part", Uuid::new_v4()));
        let temporary = TemporaryFile(temp_path);
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary.0)
            .await?;
        let mut response = response;
        let mut count = 0u64;
        let mut context = Context::new(&SHA256);
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(UpdateError::Cancelled),
                next = response.chunk() => next.map_err(UpdateError::Network)?,
            };
            let Some(chunk) = next else { break };
            count = count
                .checked_add(chunk.len() as u64)
                .ok_or(UpdateError::SizeLimit)?;
            if count > asset.size {
                return Err(UpdateError::SizeMismatch);
            }
            context.update(&chunk);
            tokio::select! {
                _ = cancel.cancelled() => return Err(UpdateError::Cancelled),
                result = output.write_all(&chunk) => result?,
            }
        }
        if count != asset.size {
            return Err(UpdateError::SizeMismatch);
        }
        if context.finish().as_ref() != asset.digest {
            return Err(UpdateError::DigestMismatch);
        }
        output.sync_all().await?;
        drop(output);
        if cancel.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        // Hard-link publication fails on collisions; unlike rename it cannot overwrite an existing file.
        fs::hard_link(&temporary.0, &final_path).await?;
        // Re-verify published bytes so a storage error is never reported as a verified stage.
        self.verify_file(&final_path, asset).await?;
        Ok(StagedArtifact {
            path: final_path,
            size: asset.size,
            sha256: digest,
            reused: false,
        })
    }

    async fn prepare_directory(&self) -> Result<(), UpdateError> {
        let parent = self
            .stage_directory
            .parent()
            .expect("fixed staged directory");
        for directory in [parent, self.stage_directory.as_path()] {
            match fs::create_dir(directory).await {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = fs::symlink_metadata(directory).await?;
            if metadata.file_type().is_symlink() {
                return Err(UpdateError::Symlink(directory.to_path_buf()));
            }
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    directory.display().to_string(),
                )
                .into());
            }
        }
        Ok(())
    }

    async fn verify_file(&self, path: &Path, asset: &UpdateAsset) -> Result<(), UpdateError> {
        let metadata = fs::symlink_metadata(path).await?;
        if !metadata.is_file() || metadata.len() != asset.size {
            return Err(UpdateError::SizeMismatch);
        }
        let mut file = fs::File::open(path).await?;
        let mut context = Context::new(&SHA256);
        let mut buf = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buf).await?;
            if count == 0 {
                break;
            }
            context.update(&buf[..count]);
        }
        if context.finish().as_ref() != asset.digest {
            return Err(UpdateError::DigestMismatch);
        }
        Ok(())
    }
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn valid_github_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 120
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && segment != "."
        && segment != ".."
}

fn safe_download_url(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    #[cfg(test)]
    if url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "::1" || host == "localhost")
    {
        return true;
    }
    false
}

fn secured_client() -> Result<Client, UpdateError> {
    Ok(Client::builder()
        .user_agent("NeuroMita-Rust/0.1 updater")
        .timeout(Duration::from_secs(120))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many redirects");
            }
            if safe_download_url(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("insecure redirect")
            }
        }))
        .build()?)
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("String write");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::SocketAddr,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    fn scratch(label: &str) -> PortableDataRoot {
        let path = std::env::temp_dir().join(format!(
            "nm-update-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        PortableDataRoot::resolve(Some(&path)).unwrap()
    }

    fn digest(bytes: &[u8]) -> String {
        format!(
            "sha256:{}",
            hex_digest(ring::digest::digest(&SHA256, bytes).as_ref())
        )
    }

    async fn serve(body: &'static [u8], declared_len: usize) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {declared_len}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });
        address
    }

    #[tokio::test]
    async fn verified_stage_reuses_content_addressed_package() {
        let root = scratch("success");
        let addr = serve(b"safe update", 11).await;
        let asset = UpdateAsset::new(
            "update.zip",
            &format!("http://{addr}/package"),
            11,
            &digest(b"safe update"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        let token = CancellationToken::new();
        let first = stage.stage(&asset, &token).await.unwrap();
        assert!(!first.reused());
        assert!(first.path().starts_with(root.path()));
        assert_eq!(fs::read(first.path()).await.unwrap(), b"safe update");
        let second = stage.stage(&asset, &token).await.unwrap();
        assert!(second.reused());
        assert_eq!(first.path(), second.path());
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn digest_mismatch_does_not_publish_or_leave_partial() {
        let root = scratch("digest");
        let addr = serve(b"tampered", 8).await;
        let asset = UpdateAsset::new(
            "update.zip",
            &format!("http://{addr}/package"),
            8,
            &digest(b"expected"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        assert!(matches!(
            stage.stage(&asset, &CancellationToken::new()).await,
            Err(UpdateError::DigestMismatch)
        ));
        assert_eq!(std::fs::read_dir(stage.stage_directory).unwrap().count(), 0);
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn incorrect_content_length_rejected_before_file_creation() {
        let root = scratch("size");
        let addr = serve(b"short", 5).await;
        let asset = UpdateAsset::new(
            "update.zip",
            &format!("http://{addr}/package"),
            6,
            &digest(b"short!"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        assert!(matches!(
            stage.stage(&asset, &CancellationToken::new()).await,
            Err(UpdateError::SizeMismatch)
        ));
        assert_eq!(std::fs::read_dir(stage.stage_directory).unwrap().count(), 0);
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn cancelled_stage_does_not_touch_storage() {
        let root = scratch("cancel");
        let asset = UpdateAsset::new(
            "update.zip",
            "https://example.com/archive",
            1,
            &digest(b"x"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            stage.stage(&asset, &token).await,
            Err(UpdateError::Cancelled)
        ));
        assert!(!stage.stage_directory.exists());
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn corrupt_cached_package_is_never_reused() {
        let root = scratch("corrupt");
        let asset = UpdateAsset::new(
            "update.zip",
            "https://example.com/archive",
            4,
            &digest(b"safe"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        stage.prepare_directory().await.unwrap();
        let target = stage
            .stage_directory
            .join(format!("sha256-{}.artifact", asset.sha256()));
        fs::write(&target, b"evil").await.unwrap();
        assert!(matches!(
            stage.stage(&asset, &CancellationToken::new()).await,
            Err(UpdateError::CorruptStage(_))
        ));
        assert_eq!(fs::read(&target).await.unwrap(), b"evil");
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_stage_directory_rejected() {
        let root = scratch("symlink");
        let external = scratch("external");
        std::os::unix::fs::symlink(external.path(), root.path().join("updates")).unwrap();
        let asset = UpdateAsset::new(
            "update.zip",
            "https://example.com/archive",
            4,
            &digest(b"safe"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        assert!(matches!(
            stage.stage(&asset, &CancellationToken::new()).await,
            Err(UpdateError::Symlink(_))
        ));
        assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 0);
        std::fs::remove_dir_all(root.path()).unwrap();
        std::fs::remove_dir_all(external.path()).unwrap();
    }

    #[tokio::test]
    async fn interrupted_response_never_publishes_or_leaves_partial() {
        let root = scratch("interrupted");
        let addr = serve(b"short", 8).await;
        let asset = UpdateAsset::new(
            "update.zip",
            &format!("http://{addr}/package"),
            8,
            &digest(b"eightbyt"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        assert!(matches!(
            stage.stage(&asset, &CancellationToken::new()).await,
            Err(UpdateError::Network(_))
        ));
        assert_eq!(std::fs::read_dir(stage.stage_directory).unwrap().count(), 0);
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn staged_package_never_modifies_existing_user_settings() {
        let root = scratch("userdata");
        let settings = root.path().join("settings.json");
        std::fs::write(&settings, b"private settings").unwrap();
        let addr = serve(b"valid", 5).await;
        let asset = UpdateAsset::new(
            "update.zip",
            &format!("http://{addr}/package"),
            5,
            &digest(b"valid"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        assert!(stage.stage(&asset, &CancellationToken::new()).await.is_ok());
        assert_eq!(std::fs::read(&settings).unwrap(), b"private settings");
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn active_cancel_while_downloading_removes_partial() {
        let root = scratch("cancel-during-download");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            stream.write_all(b"ab").await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let asset = UpdateAsset::new(
            "update.zip",
            &format!("http://{addr}/package"),
            5,
            &digest(b"abcde"),
        )
        .unwrap();
        let stage = UpdateStager::new(&root).unwrap();
        let token = CancellationToken::new();
        let handle = tokio::spawn({
            let token = token.clone();
            async move { stage.stage(&asset, &token).await }
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        token.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .unwrap()
                .unwrap(),
            Err(UpdateError::Cancelled)
        ));
        assert_eq!(
            std::fs::read_dir(root.path().join("updates/staged"))
                .unwrap()
                .count(),
            0
        );
        server.abort();
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn invalid_release_metadata_fails_closed() {
        let hash = digest(b"safe");
        assert!(matches!(
            UpdateAsset::new("bad/asset", "https://example.com/a", 4, &hash),
            Err(UpdateError::Metadata(_))
        ));
        assert!(matches!(
            UpdateAsset::new("a.zip", "http://example.com/a", 4, &hash),
            Err(UpdateError::Metadata(_))
        ));
        assert!(matches!(
            UpdateAsset::new("a.zip", "https://example.com/a", 4, "sha256:00"),
            Err(UpdateError::Metadata(_))
        ));
        assert!(matches!(
            UpdateAsset::new("a.zip", "https://example.com/a", 0, &hash),
            Err(UpdateError::SizeLimit)
        ));
    }

    #[test]
    fn github_catalog_requires_digest_and_matching_tag() {
        let source = GithubUpdateCatalog::new("Atm4x/neuromita-rust").unwrap();
        let valid = serde_json::json!({"tag_name":"v1.2.3","assets":[{"name":"app.zip","size":4,"browser_download_url":"https://example.com/archive","digest":digest(b"safe")}]});
        let records = source
            .parse_release(Some("v1.2.3"), valid.to_string().as_bytes())
            .unwrap();
        assert_eq!(records.tag, "v1.2.3");
        assert_eq!(records.assets.len(), 1);
        assert_eq!(records.assets[0].name(), "app.zip");
        assert!(
            source
                .parse_release(Some("v2"), valid.to_string().as_bytes())
                .is_err()
        );
        let missing = serde_json::json!({"tag_name":"v1","assets":[{"name":"app.zip","size":4,"browser_download_url":"https://example.com/archive"}]});
        assert!(matches!(
            source.parse_release(Some("v1"), missing.to_string().as_bytes()),
            Err(UpdateError::MissingDigest)
        ));
        let draft = serde_json::json!({"tag_name":"v1","draft":true,"assets":[]});
        assert!(
            source
                .parse_release(Some("v1"), draft.to_string().as_bytes())
                .is_err()
        );
        let latest = source
            .parse_release(None, valid.to_string().as_bytes())
            .unwrap();
        assert_eq!(latest.tag, "v1.2.3");
        let duplicates = serde_json::json!({"tag_name":"v1","assets":[
            {"name":"same.zip","size":4,"browser_download_url":"https://example.com/a","digest":digest(b"safe")},
            {"name":"same.zip","size":4,"browser_download_url":"https://example.com/b","digest":digest(b"safe")}
        ]});
        assert!(matches!(
            source.parse_release(Some("v1"), duplicates.to_string().as_bytes()),
            Err(UpdateError::Metadata(_))
        ));
        assert!(GithubUpdateCatalog::new("owner/repo/extra").is_err());
    }
}
