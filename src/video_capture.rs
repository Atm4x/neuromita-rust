use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CAPTURE_FRAMES: u8 = 4;
const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraDevice {
    pub name: String,
    input: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoSource {
    Screen,
    Camera(CameraDevice),
    #[cfg(test)]
    Fixture,
}

#[derive(Clone, Debug)]
pub struct CapturedJpeg(Vec<u8>);

impl CapturedJpeg {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn new(bytes: Vec<u8>) -> Result<Self, CaptureError> {
        if bytes.len() < 4 || !bytes.starts_with(&[255, 216]) || !bytes.ends_with(&[255, 217]) {
            return Err(CaptureError::InvalidFrame);
        }
        Ok(Self(bytes))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("Захват изображения требует FFmpeg с поддержкой Windows gdigrab и DirectShow")]
    Unavailable,
    #[error("Захват отменён")]
    Cancelled,
    #[error("Истекло время ожидания захвата")]
    TimedOut,
    #[error("Источник изображения недоступен или отключён")]
    SourceUnavailable,
    #[error("Источник вернул некорректный JPEG")]
    InvalidFrame,
    #[error("Захват превысил допустимый размер")]
    TooLarge,
    #[error("Число кадров должно быть от 1 до 4")]
    InvalidCount,
    #[error("Не удалось запустить захват: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct VideoCapture {
    binary: PathBuf,
}

impl VideoCapture {
    pub fn for_desktop(data_directory: &Path) -> Option<Self> {
        if !cfg!(windows) {
            return None;
        }
        let configured = std::env::var_os("NEUROMITA_FFMPEG").map(PathBuf::from);
        let managed = data_directory.join("AIData/bin/ffmpeg.exe");
        let candidates = [configured, Some(managed)].into_iter().flatten();
        let on_path = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|path| path.join("ffmpeg.exe"));
        let binary = candidates
            .chain(on_path)
            .find(|path| path.is_absolute() && path.is_file())?;
        Some(Self { binary })
    }

    pub async fn cameras(
        &self,
        cancelled: &CancellationToken,
    ) -> Result<Vec<CameraDevice>, CaptureError> {
        let (_, _, stderr) = self
            .run(
                &[
                    "-f".into(),
                    "dshow".into(),
                    "-list_devices".into(),
                    "true".into(),
                    "-i".into(),
                    "dummy".into(),
                ],
                MAX_DIAGNOSTIC_BYTES,
                cancelled,
            )
            .await?;
        let listing = String::from_utf8_lossy(&stderr);
        let cameras = parse_directshow(&listing);
        if cameras.is_empty()
            && !listing.contains("DirectShow")
            && !listing.lines().any(|line| line.starts_with("[dshow @"))
        {
            return Err(CaptureError::SourceUnavailable);
        }
        Ok(cameras)
    }

    pub async fn capture(
        &self,
        source: &VideoSource,
        count: u8,
        settings: &crate::settings::CaptureSettings,
        cancelled: &CancellationToken,
    ) -> Result<Vec<CapturedJpeg>, CaptureError> {
        if !(1..=MAX_CAPTURE_FRAMES).contains(&count) {
            return Err(CaptureError::InvalidCount);
        }
        let input = match source {
            VideoSource::Screen => vec![
                "-f".into(),
                "gdigrab".into(),
                "-draw_mouse".into(),
                "0".into(),
                "-i".into(),
                "desktop".into(),
            ],
            VideoSource::Camera(camera) => vec![
                "-f".into(),
                "dshow".into(),
                "-i".into(),
                format!("video={}", camera.input).into(),
            ],
            #[cfg(test)]
            VideoSource::Fixture => vec![
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                "color=red:s=64x48".into(),
            ],
        };
        let mut frames = Vec::with_capacity(usize::from(count));
        let interval = Duration::from_millis(u64::from(settings.interval_ms.clamp(100, 5000)));
        for index in 0..count {
            if index != 0 {
                tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => return Err(CaptureError::Cancelled),
                    _ = tokio::time::sleep(interval) => {}
                }
            }
            frames.push(self.frame(&input, settings.jpeg_quality, cancelled).await?);
        }
        Ok(frames)
    }

    #[cfg(test)]
    pub(crate) fn test_ffmpeg() -> Self {
        Self {
            binary: "ffmpeg".into(),
        }
    }

    async fn frame(
        &self,
        input: &[OsString],
        quality: u8,
        cancelled: &CancellationToken,
    ) -> Result<CapturedJpeg, CaptureError> {
        let quality = 31_u32 - u32::from(quality.min(100)) * 29 / 100;
        let mut args = input.to_vec();
        args.extend(
            [
                "-an",
                "-frames:v",
                "1",
                "-vf",
                "scale=w='min(iw,1280)':h='min(ih,1280)':force_original_aspect_ratio=decrease:force_divisible_by=2",
                "-threads",
                "1",
                "-c:v",
                "mjpeg",
                "-q:v",
            ]
            .into_iter()
            .map(OsString::from),
        );
        args.extend([
            quality.to_string().into(),
            "-f".into(),
            "image2pipe".into(),
            "pipe:1".into(),
        ]);
        let (success, bytes, _) = self.run(&args, MAX_FRAME_BYTES, cancelled).await?;
        if !success {
            return Err(CaptureError::SourceUnavailable);
        }
        CapturedJpeg::new(bytes)
    }

    async fn run(
        &self,
        args: &[OsString],
        max_output: usize,
        cancelled: &CancellationToken,
    ) -> Result<(bool, Vec<u8>, Vec<u8>), CaptureError> {
        let operation = async {
            let mut command = Command::new(&self.binary);
            command
                .args(["-hide_banner", "-nostdin"])
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            let mut child = command.spawn()?;
            let stdout = child.stdout.take().expect("piped stdout");
            let stderr = child.stderr.take().expect("piped stderr");
            let (status, bytes, diagnostic) = tokio::try_join!(
                async { child.wait().await.map_err(CaptureError::from) },
                read_bounded(stdout, max_output),
                read_bounded(stderr, MAX_DIAGNOSTIC_BYTES),
            )?;
            Ok::<_, CaptureError>((status.success(), bytes, diagnostic))
        };
        tokio::select! {
            biased;
            _ = cancelled.cancelled() => Err(CaptureError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(8), operation) =>
                result.map_err(|_| CaptureError::TimedOut)?,
        }
    }
}

async fn read_bounded(
    input: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, CaptureError> {
    let mut bytes = Vec::new();
    input.take(limit as u64 + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > limit {
        return Err(CaptureError::TooLarge);
    }
    Ok(bytes)
}

fn parse_directshow(listing: &str) -> Vec<CameraDevice> {
    let mut camera: Option<String> = None;
    let mut result = Vec::new();
    for line in listing.lines() {
        let Some((_, value)) = line
            .strip_prefix("[dshow @")
            .and_then(|line| line.split_once("] "))
        else {
            continue;
        };
        if let Some(name) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix("\" (video)"))
        {
            camera = Some(name.to_owned());
        } else if let Some(input) = value
            .strip_prefix("  Alternative name \"")
            .and_then(|value| value.strip_suffix('"'))
        {
            if let Some(name) = camera.take()
                && !result.iter().any(|item: &CameraDevice| item.input == input)
            {
                result.push(CameraDevice {
                    name,
                    input: input.to_owned(),
                });
            }
        } else {
            camera = None;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directshow_uses_device_ids_and_excludes_audio() {
        let cameras = parse_directshow(concat!(
            "[dshow @ 0000] \"USB Camera\" (video)\n",
            "[dshow @ 0000]   Alternative name \"@device_pnp_first\"\n",
            "[dshow @ 0000] \"USB Camera\" (video)\n",
            "[dshow @ 0000]   Alternative name \"@device_pnp_second\"\n",
            "[dshow @ 0000] \"Microphone\" (audio)\n",
            "[dshow @ 0000]   Alternative name \"@device_audio\"\n",
        ));
        assert_eq!(cameras.len(), 2);
        assert_eq!(cameras[0].name, cameras[1].name);
        assert_ne!(cameras[0].input, cameras[1].input);
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg in PATH"]
    async fn real_encoder_accepts_synthetic_frames_and_honors_cancellation() {
        let capture = VideoCapture::test_ffmpeg();
        let input = [
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            "color=red:s=64x48".into(),
        ];
        let stop = CancellationToken::new();
        assert!(
            capture
                .frame(&input, 80, &stop)
                .await
                .unwrap()
                .bytes()
                .len()
                > 100
        );
        stop.cancel();
        assert!(matches!(
            capture.frame(&input, 80, &stop).await,
            Err(CaptureError::Cancelled)
        ));
    }
}
