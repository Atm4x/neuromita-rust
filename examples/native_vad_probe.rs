//! Native capture/endpoint acceptance only; no Core, Python, ASR or network.

use std::{
    env,
    error::Error,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use neuromita::audio::{
    device::{AudioCapture, CaptureBackend, FfmpegCapture},
    score::{DirectEarshotScores, VadScoreProvider},
    segment::{PttPolicy, ScoreGate, SegmentConfig, SegmentEndReason, SpeechMode, SpeechSegmenter},
    vad::{FRAME_SAMPLES, SAMPLE_RATE_HZ, VoiceThreshold},
};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep_until};

type ProbeResult<T> = Result<T, Box<dyn Error>>;
const CONFIG_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    ffmpeg_binary: PathBuf,
    backend: Backend,
    device: String,
    provider: Provider,
    threshold: f32,
    duration_secs: u64,
    mode: Mode,
    ptt_events: Vec<PttEvent>,
    stop_at_ms: Option<u64>,
    segment: Policy,
    ptt_policy: Option<PttConfig>,
    output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Backend {
    Pulse,
    Alsa,
    Dshow,
    Avfoundation,
    Lavfi,
}

impl From<Backend> for CaptureBackend {
    fn from(value: Backend) -> Self {
        match value {
            Backend::Pulse => Self::Pulse,
            Backend::Alsa => Self::Alsa,
            Backend::Dshow => Self::Dshow,
            Backend::Avfoundation => Self::Avfoundation,
            Backend::Lavfi => Self::Lavfi,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Provider {
    Direct,
    Wavekat,
    Silero,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Auto,
    Ptt,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PttEvent {
    time_ms: u64,
    pressed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    pre_roll_samples: usize,
    start_voice_samples: usize,
    end_silence_samples: usize,
    min_segment_samples: usize,
    max_segment_samples: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PttConfig {
    pre_roll_samples: usize,
    tail_samples: usize,
    min_voiced_samples: usize,
}

impl Config {
    fn policy(&self) -> SegmentConfig {
        SegmentConfig {
            mode: match self.mode {
                Mode::Auto => SpeechMode::Automatic,
                Mode::Ptt => SpeechMode::PushToTalk,
            },
            pre_roll_samples: self.segment.pre_roll_samples,
            start_voice_samples: self.segment.start_voice_samples,
            end_silence_samples: self.segment.end_silence_samples,
            min_segment_samples: self.segment.min_segment_samples,
            max_segment_samples: self.segment.max_segment_samples,
        }
    }

    fn validate(&self) -> ProbeResult<()> {
        if !self.ffmpeg_binary.is_absolute() || !self.ffmpeg_binary.is_file() {
            return Err("ffmpeg_binary must name an existing absolute local file".into());
        }
        if self.device.trim().is_empty() || self.device.len() > 512 || self.device.contains('\0') {
            return Err("device must be an explicit backend identity (1..512 bytes)".into());
        }
        if !(1..=120).contains(&self.duration_secs) {
            return Err("duration_secs must be 1..120".into());
        }
        VoiceThreshold::new(self.threshold)?;
        let end_ms = self.duration_secs * 1000;
        if self.stop_at_ms.is_some_and(|ms| ms == 0 || ms >= end_ms) {
            return Err("stop_at_ms must be strictly inside the duration".into());
        }
        if matches!(self.mode, Mode::Auto) && !self.ptt_events.is_empty() {
            return Err("auto mode requires an empty ptt_events array".into());
        }
        let mut previous = None;
        let mut pressed = false;
        for event in &self.ptt_events {
            if event.time_ms >= end_ms
                || previous.is_some_and(|ms| event.time_ms <= ms)
                || event.pressed == pressed
            {
                return Err("PTT events must strictly increase, alternate from pressed=true, and precede duration".into());
            }
            previous = Some(event.time_ms);
            pressed = event.pressed;
        }
        if matches!(self.mode, Mode::Ptt) && self.ptt_events.is_empty() {
            return Err("ptt mode requires an explicit event timeline".into());
        }
        if matches!(self.mode, Mode::Ptt) != self.ptt_policy.is_some() {
            return Err("ptt mode requires ptt_policy; auto mode does not accept it".into());
        }
        if let Some(path) = &self.output_dir
            && (!path.is_absolute()
                || path.file_name().is_none()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                || path.symlink_metadata().is_ok()
                || !path.parent().is_some_and(Path::is_dir))
        {
            return Err("output_dir must be a new absolute child directory of an existing parent, without '..'".into());
        }
        Ok(())
    }
}

fn read_config(path: &Path) -> ProbeResult<Config> {
    if !path.is_absolute() {
        return Err("config path must be absolute".into());
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err("config must be a regular JSON file".into());
    }
    if metadata.len() > CONFIG_LIMIT {
        return Err("config exceeds 64 KiB".into());
    }
    let mut bytes = Vec::new();
    file.take(CONFIG_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err("config exceeds 64 KiB".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Finish {
    Duration,
    Stop,
    Eof,
    CaptureError,
    ProviderError,
}

#[derive(Serialize)]
struct SegmentReport {
    start_sample: u64,
    end_sample: u64,
    onset_decided_at_sample: Option<u64>,
    emitted_at_sample: u64,
    end_reason: SegmentEndReason,
    sample_count: usize,
    peak_amplitude: u16,
    wav_path: Option<PathBuf>,
}

#[derive(Serialize)]
struct Summary {
    backend: Backend,
    provider: Provider,
    mode: Mode,
    threshold: f32,
    sample_rate_hz: u32,
    duration_limit_secs: u64,
    elapsed_ms: u128,
    finish: Finish,
    error: Option<String>,
    sample_count: u64,
    peak_amplitude: u16,
    applied_ptt_events: usize,
    segments: Vec<SegmentReport>,
}

async fn probe<P: VadScoreProvider>(config: &Config, provider: P) -> ProbeResult<Summary> {
    let gate = ScoreGate::new(provider, VoiceThreshold::new(config.threshold)?)?;
    let mut segmenter = SpeechSegmenter::new(gate, config.policy())?;
    if let Some(policy) = &config.ptt_policy {
        segmenter = segmenter.with_ptt_policy(PttPolicy {
            pre_roll_samples: policy.pre_roll_samples,
            tail_samples: policy.tail_samples,
            min_voiced_samples: policy.min_voiced_samples,
            require_release_after_max: true,
        })?;
    }
    if let Some(path) = &config.output_dir {
        fs::create_dir(path)?;
    }
    let start = Instant::now();
    let deadline = start + Duration::from_secs(config.duration_secs);
    let stop_time = config
        .stop_at_ms
        .map_or(deadline, |ms| start + Duration::from_millis(ms));
    let mut report = Summary {
        backend: config.backend,
        provider: config.provider,
        mode: config.mode,
        threshold: config.threshold,
        sample_rate_hz: SAMPLE_RATE_HZ,
        duration_limit_secs: config.duration_secs,
        elapsed_ms: 0,
        finish: Finish::Duration,
        error: None,
        sample_count: 0,
        peak_amplitude: 0,
        applied_ptt_events: 0,
        segments: Vec::new(),
    };
    let mut capture = match FfmpegCapture::start(
        config.ffmpeg_binary.clone(),
        config.backend.into(),
        &config.device,
    ) {
        Ok(capture) => capture,
        Err(error) => {
            report.finish = Finish::CaptureError;
            report.error = Some(error.to_string());
            report.elapsed_ms = start.elapsed().as_millis();
            return Ok(report);
        }
    };
    let mut emitted = Vec::new();
    let mut frame_ready = false;
    let signal = tokio::signal::ctrl_c();
    tokio::pin!(signal);
    let sample_limit = config.duration_secs * u64::from(SAMPLE_RATE_HZ);
    loop {
        let event_time = config
            .ptt_events
            .get(report.applied_ptt_events)
            .map_or(deadline, |event| {
                start + Duration::from_millis(event.time_ms)
            });
        let frame_time = start
            + Duration::from_secs_f64(
                (report.sample_count + FRAME_SAMPLES as u64) as f64 / f64::from(SAMPLE_RATE_HZ),
            );
        tokio::select! {
            biased;
            _ = sleep_until(deadline) => break,
            _ = sleep_until(stop_time), if config.stop_at_ms.is_some() => {
                report.finish = Finish::Stop;
                break;
            }
            result = &mut signal => {
                result?;
                report.finish = Finish::Stop;
                break;
            }
            _ = sleep_until(event_time), if report.applied_ptt_events < config.ptt_events.len() => {
                let event = &config.ptt_events[report.applied_ptt_events];
                if let Some(segment) = segmenter.set_pressed(event.pressed) {
                    emitted.push(segment);
                }
                report.applied_ptt_events += 1;
            }
            _ = sleep_until(frame_time), if !frame_ready => {
                frame_ready = true;
            }
            result = capture.next_frame(), if frame_ready => {
                frame_ready = false;
                match result {
                    Ok(Some(frame)) => {
                        if report.sample_count + FRAME_SAMPLES as u64 > sample_limit {
                            break;
                        }
                        report.sample_count += FRAME_SAMPLES as u64;
                        report.peak_amplitude = report.peak_amplitude.max(
                            frame.iter().map(|sample| sample.unsigned_abs()).max().unwrap_or(0),
                        );
                        match segmenter.process(frame) {
                            Ok(Some(segment)) => emitted.push(segment),
                            Ok(None) => {}
                            Err(error) => {
                                report.finish = Finish::ProviderError;
                                report.error = Some(error.to_string());
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        report.finish = Finish::Eof;
                        break;
                    }
                    Err(error) => {
                        report.finish = Finish::CaptureError;
                        report.error = Some(error.to_string());
                        break;
                    }
                }
            }
        }
    }
    drop(capture);
    report.elapsed_ms = start.elapsed().as_millis();
    if !matches!(report.finish, Finish::CaptureError | Finish::ProviderError)
        && let Some(segment) = segmenter.flush()
    {
        emitted.push(segment);
    }
    for (index, segment) in emitted.iter().enumerate() {
        let wav_path = if let Some(directory) = &config.output_dir {
            let path = directory.join(format!("segment-{index:04}.wav"));
            let mut file = File::options().write(true).create_new(true).open(&path)?;
            file.write_all(&segment.as_wav()?)?;
            Some(path)
        } else {
            None
        };
        report.segments.push(SegmentReport {
            start_sample: segment.start_sample,
            end_sample: segment.end_sample,
            onset_decided_at_sample: segment.onset_decided_at_sample,
            emitted_at_sample: segment.emitted_at_sample,
            end_reason: segment.end_reason,
            sample_count: segment.samples.len(),
            peak_amplitude: segment
                .samples
                .iter()
                .map(|sample| sample.unsigned_abs())
                .max()
                .unwrap_or(0),
            wav_path,
        });
    }
    Ok(report)
}

async fn dispatch(config: &Config) -> ProbeResult<Summary> {
    match config.provider {
        Provider::Direct => probe(config, DirectEarshotScores::default()).await,
        Provider::Wavekat => {
            #[cfg(feature = "vad-wavekat")]
            {
                probe(config, neuromita::audio::score::WavekatEarshotScores::new()).await
            }
            #[cfg(not(feature = "vad-wavekat"))]
            {
                Err("wavekat requires --features vad-wavekat".into())
            }
        }
        Provider::Silero => {
            #[cfg(feature = "vad-silero")]
            {
                use neuromita::audio::score::{
                    SileroScores, VerifiedSileroAsset, init_onnx_runtime,
                };
                let model = PathBuf::from(
                    env::var_os("NEUROMITA_SILERO_MODEL")
                        .ok_or("set NEUROMITA_SILERO_MODEL to an absolute local model path")?,
                );
                let dll = PathBuf::from(
                    env::var_os("NEUROMITA_ORT_DLL")
                        .ok_or("set NEUROMITA_ORT_DLL to an absolute local DLL path")?,
                );
                init_onnx_runtime(&dll)?;
                let asset = VerifiedSileroAsset::open(&model)?;
                probe(config, SileroScores::from_verified(&asset)?).await
            }
            #[cfg(not(feature = "vad-silero"))]
            {
                Err("silero requires --features vad-silero and explicit local model/DLL env".into())
            }
        }
    }
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    let mut args = env::args_os().skip(1);
    let path = args
        .next()
        .ok_or("usage: native_vad_probe <absolute-config.json>")?;
    if args.next().is_some() {
        return Err("usage: native_vad_probe <absolute-config.json>".into());
    }
    let config = read_config(Path::new(&path))?;
    config.validate()?;
    let report = dispatch(&config).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.error.is_some() {
        return Err("probe failed; see JSON summary and FFmpeg stderr".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            ffmpeg_binary: env::current_exe().unwrap(),
            backend: Backend::Lavfi,
            device: "anullsrc=r=16000:cl=mono,atrim=end_sample=16384".into(),
            provider: Provider::Direct,
            threshold: 0.5,
            duration_secs: 3,
            mode: Mode::Ptt,
            ptt_events: vec![
                PttEvent {
                    time_ms: 0,
                    pressed: true,
                },
                PttEvent {
                    time_ms: 500,
                    pressed: false,
                },
            ],
            stop_at_ms: None,
            segment: Policy {
                pre_roll_samples: 3072,
                start_voice_samples: 768,
                end_silence_samples: 7168,
                min_segment_samples: 2560,
                max_segment_samples: 160000,
            },
            output_dir: None,
            ptt_policy: Some(PttConfig {
                pre_roll_samples: 3200,
                tail_samples: 2048,
                min_voiced_samples: 5600,
            }),
        }
    }

    #[test]
    fn rejects_unbounded_duration_and_ambiguous_ptt() {
        let mut config = config();
        assert!(config.validate().is_ok());
        config.duration_secs = 121;
        assert!(config.validate().is_err());
        config.duration_secs = 0;
        assert!(config.validate().is_err());
        config.duration_secs = 3;
        config.ptt_events[1].time_ms = 0;
        assert!(config.validate().is_err());
        config.ptt_events[1].time_ms = 500;
        config.ptt_events[1].pressed = true;
        assert!(config.validate().is_err());
        config.ptt_events[1].pressed = false;
        config.mode = Mode::Auto;
        assert!(config.validate().is_err());
    }

    #[test]
    fn nested_json_rejects_unknown_fields() {
        assert!(
            serde_json::from_str::<PttEvent>(r#"{"time_ms":0,"pressed":true,"typo":1}"#).is_err()
        );
        assert!(serde_json::from_str::<Policy>(
            r#"{"pre_roll_samples":0,"start_voice_samples":256,"end_silence_samples":256,"min_segment_samples":256,"max_segment_samples":256,"typo":1}"#
        ).is_err());
    }

    #[test]
    fn ptt_release_and_eof_preserve_offsets() {
        let config = config();
        let gate = ScoreGate::new(
            DirectEarshotScores::default(),
            VoiceThreshold::new(0.5).unwrap(),
        )
        .unwrap();
        let mut segmenter = SpeechSegmenter::new(gate, config.policy()).unwrap();
        assert!(segmenter.process([0; FRAME_SAMPLES]).unwrap().is_none());
        assert!(segmenter.set_pressed(true).is_none());
        for _ in 0..10 {
            assert!(segmenter.process([123; FRAME_SAMPLES]).unwrap().is_none());
        }
        let segment = segmenter.set_pressed(false).unwrap();
        assert_eq!(segment.end_reason, SegmentEndReason::Release);
        assert_eq!((segment.start_sample, segment.end_sample), (256, 2816));
        assert!(segmenter.set_pressed(true).is_none());
        for _ in 0..10 {
            assert!(segmenter.process([0; FRAME_SAMPLES]).unwrap().is_none());
        }
        let segment = segmenter.flush().unwrap();
        assert_eq!(segment.end_reason, SegmentEndReason::Eof);
        assert_eq!((segment.start_sample, segment.end_sample), (2816, 5376));
    }
}
