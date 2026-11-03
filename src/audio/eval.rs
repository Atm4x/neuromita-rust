//! Offline A/B harness for VAD providers.
//!
//! Fixtures are explicit local files described by a manifest. Metrics are computed
//! per fixture; nothing here tunes thresholds or downloads anything.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    score::{ScoreError, ScoreFrames, VadScoreProvider},
    segment::{ScoreGate, SegmentConfig, SegmentError, SpeechSegmenter},
    vad::{FRAME_SAMPLES, SAMPLE_RATE_HZ, VoiceThreshold},
};

/// Longest accepted fixture: thirty seconds at 16 kHz.
pub const MAX_FIXTURE_SAMPLES: usize = 30 * SAMPLE_RATE_HZ as usize;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("fixture manifest is invalid: {0}")]
    Manifest(String),
    #[error("unsupported WAV fixture: {0}")]
    Wav(&'static str),
    #[error("fixture io failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("score provider failed")]
    Score(#[from] ScoreError),
    #[error("speech segmentation failed")]
    Segment(#[from] SegmentError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FixtureRole {
    Calibration,
    Heldout,
}

/// Speech interval in integer sample offsets. A transcript is not an annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Annotation {
    pub start_sample: u64,
    pub end_sample: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Fixture {
    pub id: String,
    pub category: String,
    pub wav: String,
    pub role: FixtureRole,
    #[serde(default)]
    pub speech: Vec<Annotation>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub fixtures: Vec<Fixture>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, EvalError> {
        let raw = fs::read_to_string(path)?;
        let manifest: Self =
            serde_json::from_str(&raw).map_err(|error| EvalError::Manifest(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), EvalError> {
        if self.fixtures.is_empty() {
            return Err(EvalError::Manifest("no fixtures".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for fixture in &self.fixtures {
            if fixture.id.is_empty() || fixture.wav.is_empty() {
                return Err(EvalError::Manifest("fixture needs id and wav".into()));
            }
            if !seen.insert(fixture.id.as_str()) {
                return Err(EvalError::Manifest(format!(
                    "duplicate fixture id {}",
                    fixture.id
                )));
            }
            let mut previous = 0;
            for interval in &fixture.speech {
                if interval.start_sample >= interval.end_sample || interval.start_sample < previous
                {
                    return Err(EvalError::Manifest(format!(
                        "fixture {} has unordered or empty intervals",
                        fixture.id
                    )));
                }
                previous = interval.end_sample;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameMetrics {
    pub frames: u64,
    pub true_positive: u64,
    pub false_positive: u64,
    pub false_negative: u64,
    pub precision: f32,
    pub recall: f32,
    pub f1: f32,
    pub false_positive_seconds: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoundaryError {
    pub matched: u64,
    pub missed: u64,
    pub onset_ms: f32,
    pub end_ms: f32,
}

/// What the player would actually get: segments produced by `ScoreGate` and
/// `SpeechSegmenter` with the configured silence policy.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointMetrics {
    pub annotated: u64,
    pub detected: u64,
    pub matched: u64,
    pub missed: u64,
    pub spurious: u64,
    pub onset_ms: Option<f32>,
    pub end_ms: Option<f32>,
    pub spurious_seconds: f32,
    /// Up to the first sixteen produced segments, in milliseconds.
    pub detected_segments: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FixtureReport {
    pub id: String,
    pub category: String,
    pub role: FixtureRole,
    pub metrics: FrameMetrics,
    pub boundary: Option<BoundaryError>,
    pub endpoint: Option<EndpointMetrics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub threshold: f32,
    pub fixtures: Vec<FixtureReport>,
}

/// Score every fixture with a fresh provider and report frame metrics.
///
/// The threshold is supplied by the caller and never adjusted here; run held-out
/// fixtures only after the value is frozen.
pub fn evaluate<P, F>(
    manifest_path: &Path,
    new_provider: F,
    threshold: f32,
) -> Result<Report, EvalError>
where
    P: VadScoreProvider,
    F: Fn() -> P,
{
    if !(0.0..=1.0).contains(&threshold) {
        return Err(EvalError::Manifest("threshold must be in 0..=1".into()));
    }
    let manifest = Manifest::load(manifest_path)?;
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let mut fixtures = Vec::with_capacity(manifest.fixtures.len());
    for fixture in &manifest.fixtures {
        let samples = read_pcm16_mono_wav(&base.join(&fixture.wav))?;
        let scores = score_fixture(&samples, new_provider())?;
        let metrics = frame_metrics(&scores, &fixture.speech, threshold);
        let boundary = boundary_error(&scores, &fixture.speech, threshold);
        fixtures.push(FixtureReport {
            id: fixture.id.clone(),
            category: fixture.category.clone(),
            role: fixture.role,
            metrics,
            boundary,
            endpoint: None,
        });
    }
    Ok(Report {
        threshold,
        fixtures,
    })
}

/// Frame metrics plus real endpoint behaviour: segments produced by
/// `ScoreGate` and `SpeechSegmenter` with the given silence policy.
pub fn evaluate_endpoint<P, F>(
    manifest_path: &Path,
    new_provider: F,
    threshold: f32,
    config: SegmentConfig,
) -> Result<Report, EvalError>
where
    P: VadScoreProvider,
    F: Fn() -> P,
{
    let mut report = evaluate(manifest_path, &new_provider, threshold)?;
    let manifest = Manifest::load(manifest_path)?;
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let voice = VoiceThreshold::new(threshold)
        .map_err(|_| EvalError::Manifest("threshold must be in 0..=1".into()))?;
    for (fixture, entry) in manifest.fixtures.iter().zip(report.fixtures.iter_mut()) {
        let samples = read_pcm16_mono_wav(&base.join(&fixture.wav))?;
        let gate = ScoreGate::new(new_provider(), voice)?;
        let mut segmenter = SpeechSegmenter::new(gate, config)?;
        let mut segments = Vec::new();
        let mut consumed = 0u64;
        for frame in samples.chunks(FRAME_SAMPLES) {
            if frame.len() < FRAME_SAMPLES {
                break;
            }
            consumed += FRAME_SAMPLES as u64;
            let frame: [i16; FRAME_SAMPLES] = frame.try_into().expect("frame size");
            if let Some(segment) = segmenter.process(frame)? {
                let end = consumed;
                let start = end.saturating_sub(segment.samples.len() as u64);
                segments.push((start, end));
            }
        }
        if let Some(segment) = segmenter.flush() {
            let end = consumed;
            let start = end.saturating_sub(segment.samples.len() as u64);
            segments.push((start, end));
        }
        entry.endpoint = Some(endpoint_metrics(&segments, &fixture.speech));
    }
    Ok(report)
}

fn endpoint_metrics(segments: &[(u64, u64)], speech: &[Annotation]) -> EndpointMetrics {
    let sample_ms = 1000.0 / SAMPLE_RATE_HZ as f32;
    let mut matched = 0u64;
    let mut missed = 0u64;
    let mut onset = Vec::new();
    let mut end = Vec::new();
    for interval in speech {
        let overlapping: Vec<&(u64, u64)> = segments
            .iter()
            .filter(|(start, stop)| *start < interval.end_sample && *stop > interval.start_sample)
            .collect();
        if overlapping.is_empty() {
            missed += 1;
            continue;
        }
        matched += 1;
        let start = overlapping.iter().map(|(s, _)| *s).min().unwrap();
        let stop = overlapping.iter().map(|(_, e)| *e).max().unwrap();
        // Latency the listener sees: how late speech is reported, never negative
        // when a merged segment already covers the interval.
        onset.push((start as f32 - interval.start_sample as f32).max(0.0) * sample_ms);
        end.push((stop as f32 - interval.end_sample as f32).max(0.0) * sample_ms);
    }
    let spurious: Vec<&(u64, u64)> = segments
        .iter()
        .filter(|(start, stop)| {
            !speech
                .iter()
                .any(|interval| *start < interval.end_sample && *stop > interval.start_sample)
        })
        .collect();
    EndpointMetrics {
        annotated: speech.len() as u64,
        detected: segments.len() as u64,
        matched,
        missed,
        spurious: spurious.len() as u64,
        onset_ms: mean(&onset),
        end_ms: mean(&end),
        spurious_seconds: spurious
            .iter()
            .map(|(start, stop)| (stop - start) as f32 / SAMPLE_RATE_HZ as f32)
            .sum(),
        detected_segments: segments
            .iter()
            .take(16)
            .map(|(start, stop)| {
                (
                    (start * 1000 / SAMPLE_RATE_HZ as u64) as u32,
                    (stop * 1000 / SAMPLE_RATE_HZ as u64) as u32,
                )
            })
            .collect(),
    }
}

fn score_fixture<P: VadScoreProvider>(
    samples: &[i16],
    provider: P,
) -> Result<Vec<super::score::ScoredFrame>, EvalError> {
    let frame = provider.frame_samples();
    let mut framing = ScoreFrames::new(provider)?;
    let mut scores = Vec::new();
    for chunk in samples.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        scores.extend(framing.push(chunk)?);
    }
    Ok(scores)
}

fn frame_metrics(
    scores: &[super::score::ScoredFrame],
    speech: &[Annotation],
    threshold: f32,
) -> FrameMetrics {
    let mut metrics = FrameMetrics {
        frames: scores.len() as u64,
        true_positive: 0,
        false_positive: 0,
        false_negative: 0,
        precision: 0.0,
        recall: 0.0,
        f1: 0.0,
        false_positive_seconds: 0.0,
    };
    for score in scores {
        let midpoint = (score.start_sample + score.end_sample) / 2;
        let expected = speech
            .iter()
            .any(|interval| (interval.start_sample..interval.end_sample).contains(&midpoint));
        let predicted = score.probability > threshold;
        match (expected, predicted) {
            (true, true) => metrics.true_positive += 1,
            (false, true) => {
                metrics.false_positive += 1;
                metrics.false_positive_seconds +=
                    (score.end_sample - score.start_sample) as f32 / SAMPLE_RATE_HZ as f32;
            }
            (true, false) => metrics.false_negative += 1,
            (false, false) => {}
        }
    }
    let predicted_positive = metrics.true_positive + metrics.false_positive;
    let actual_positive = metrics.true_positive + metrics.false_negative;
    if predicted_positive > 0 {
        metrics.precision = metrics.true_positive as f32 / predicted_positive as f32;
    }
    if actual_positive > 0 {
        metrics.recall = metrics.true_positive as f32 / actual_positive as f32;
    }
    if metrics.precision + metrics.recall > 0.0 {
        metrics.f1 =
            2.0 * metrics.precision * metrics.recall / (metrics.precision + metrics.recall);
    }
    metrics
}

fn boundary_error(
    scores: &[super::score::ScoredFrame],
    speech: &[Annotation],
    threshold: f32,
) -> Option<BoundaryError> {
    if speech.is_empty() {
        return None;
    }
    let sample_ms = 1000.0 / SAMPLE_RATE_HZ as f32;
    let runs = predicted_runs(scores, threshold);
    let mut matched = 0u64;
    let mut missed = 0u64;
    let mut onset = Vec::new();
    let mut end = Vec::new();
    for interval in speech {
        let overlapping: Vec<&(u64, u64)> = runs
            .iter()
            .filter(|(start, stop)| *start < interval.end_sample && *stop > interval.start_sample)
            .collect();
        if overlapping.is_empty() {
            missed += 1;
            continue;
        }
        matched += 1;
        let start = overlapping.iter().map(|(s, _)| *s).min().unwrap();
        let stop = overlapping.iter().map(|(_, e)| *e).max().unwrap();
        onset.push((start as f32 - interval.start_sample as f32).max(0.0) * sample_ms);
        end.push((stop as f32 - interval.end_sample as f32).max(0.0) * sample_ms);
    }
    Some(BoundaryError {
        matched,
        missed,
        onset_ms: mean(&onset).unwrap_or(0.0),
        end_ms: mean(&end).unwrap_or(0.0),
    })
}

fn predicted_runs(scores: &[super::score::ScoredFrame], threshold: f32) -> Vec<(u64, u64)> {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for score in scores {
        if score.probability <= threshold {
            continue;
        }
        match runs.last_mut() {
            Some(last) if last.1 >= score.start_sample => last.1 = score.end_sample,
            _ => runs.push((score.start_sample, score.end_sample)),
        }
    }
    runs
}

fn mean(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f32>() / values.len() as f32)
    }
}

fn read_pcm16_mono_wav(path: &Path) -> Result<Vec<i16>, EvalError> {
    let bytes = fs::read(path)?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(EvalError::Wav("not a RIFF/WAVE file"));
    }
    let mut format = None;
    let mut data = None;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = offset + 8;
        let end = body
            .checked_add(size)
            .ok_or(EvalError::Wav("chunk overflow"))?;
        if end > bytes.len() {
            return Err(EvalError::Wav("truncated chunk"));
        }
        if id == b"fmt " {
            if size < 16 {
                return Err(EvalError::Wav("short fmt chunk"));
            }
            let audio_format = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
            let channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
            let sample_rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            let bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
            if audio_format != 1 || channels != 1 || bits != 16 {
                return Err(EvalError::Wav("fixture must be 16-bit mono PCM"));
            }
            if sample_rate != SAMPLE_RATE_HZ {
                return Err(EvalError::Wav("fixture must be 16 kHz"));
            }
            format = Some(());
        } else if id == b"data" {
            data = Some(&bytes[body..end]);
        }
        offset = end + (size & 1);
    }
    format.ok_or(EvalError::Wav("missing fmt chunk"))?;
    let data = data.ok_or(EvalError::Wav("missing data chunk"))?;
    if data.len() % 2 != 0 {
        return Err(EvalError::Wav("odd PCM data length"));
    }
    let (pairs, _) = data.as_chunks::<2>();
    let samples: Vec<i16> = pairs.iter().map(|pair| i16::from_le_bytes(*pair)).collect();
    if samples.len() > MAX_FIXTURE_SAMPLES {
        return Err(EvalError::Wav("fixture exceeds thirty seconds"));
    }
    Ok(samples)
}

/// Directory that holds a manifest and its WAV files.
pub fn fixture_root(manifest_path: &Path) -> PathBuf {
    manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::vad::FRAME_SAMPLES;

    struct MarkerScores {
        marker: i16,
        always: bool,
    }

    impl VadScoreProvider for MarkerScores {
        fn frame_samples(&self) -> usize {
            FRAME_SAMPLES
        }

        fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
            Ok(if self.always || frame.contains(&self.marker) {
                1.0
            } else {
                0.0
            })
        }

        fn reset(&mut self) {}
    }

    fn wav(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&data);
        bytes
    }

    fn fixture_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("neuromita-eval-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_clip(samples: &[i16]) -> (PathBuf, PathBuf) {
        let dir = fixture_dir("clip");
        let wav_path = dir.join("clip.wav");
        fs::write(&wav_path, wav(samples, 1, SAMPLE_RATE_HZ)).unwrap();
        let manifest = dir.join("manifest.json");
        fs::write(
            &manifest,
            r#"{"fixtures":[{"id":"clip","category":"ru","wav":"clip.wav","role":"heldout",
                "speech":[{"start_sample":1024,"end_sample":2048}]}]}"#,
        )
        .unwrap();
        (manifest, wav_path)
    }

    #[test]
    fn manifest_rejects_duplicate_ids_and_unordered_intervals() {
        let duplicate: Manifest = serde_json::from_str(
            r#"{"fixtures":[
                {"id":"a","category":"ru","wav":"a.wav","role":"heldout","speech":[]},
                {"id":"a","category":"en","wav":"b.wav","role":"heldout","speech":[]}]}"#,
        )
        .unwrap();
        assert!(duplicate.validate().is_err());

        let unordered: Manifest = serde_json::from_str(
            r#"{"fixtures":[{"id":"a","category":"ru","wav":"a.wav","role":"heldout",
                "speech":[{"start_sample":500,"end_sample":300}]}]}"#,
        )
        .unwrap();
        assert!(unordered.validate().is_err());
    }

    #[test]
    fn wav_parser_rejects_stereo_and_short_files() {
        let dir = fixture_dir("wav");
        let stereo = dir.join("stereo.wav");
        fs::write(&stereo, wav(&[0; 64], 2, SAMPLE_RATE_HZ)).unwrap();
        assert!(matches!(
            read_pcm16_mono_wav(&stereo),
            Err(EvalError::Wav(_))
        ));

        let broken = dir.join("broken.wav");
        fs::write(&broken, b"RIFF").unwrap();
        assert!(matches!(
            read_pcm16_mono_wav(&broken),
            Err(EvalError::Wav(_))
        ));
    }

    #[test]
    fn perfect_provider_reaches_f1_one_and_silent_provider_misses_everything() {
        let mut samples = vec![0i16; FRAME_SAMPLES * 4];
        samples.extend([7i16; FRAME_SAMPLES * 4]);
        samples.extend(vec![0i16; FRAME_SAMPLES * 4]);
        let (manifest, _) = write_clip(&samples);

        let report = evaluate(
            &manifest,
            || MarkerScores {
                marker: 7,
                always: false,
            },
            0.5,
        )
        .unwrap();
        let metrics = &report.fixtures[0].metrics;
        assert_eq!(metrics.true_positive, 4);
        assert_eq!(metrics.false_positive, 0);
        assert_eq!(metrics.false_negative, 0);
        assert!((metrics.f1 - 1.0).abs() < 1e-6);
        let boundary = report.fixtures[0].boundary.as_ref().unwrap();
        assert_eq!(boundary.matched, 1);
        assert_eq!(boundary.missed, 0);
        assert_eq!(boundary.onset_ms, 0.0);
        assert_eq!(boundary.end_ms, 0.0);

        let report = evaluate(
            &manifest,
            || MarkerScores {
                marker: 7,
                always: false,
            },
            0.5,
        );
        let missed = evaluate(
            &manifest,
            || MarkerScores {
                marker: 99,
                always: false,
            },
            0.5,
        )
        .unwrap();
        assert!(report.is_ok());
        assert_eq!(missed.fixtures[0].metrics.false_negative, 4);
        assert_eq!(missed.fixtures[0].metrics.recall, 0.0);
        let boundary = missed.fixtures[0].boundary.as_ref().unwrap();
        assert_eq!(boundary.matched, 0);
        assert_eq!(boundary.missed, 1);
    }

    #[test]
    fn missed_utterance_is_reported_separately_from_matched_one() {
        let mut samples = vec![0i16; FRAME_SAMPLES * 10];
        for frame in [1usize, 2] {
            samples[frame * FRAME_SAMPLES..(frame + 1) * FRAME_SAMPLES].fill(9);
        }
        let dir = fixture_dir("missed");
        fs::write(dir.join("clip.wav"), wav(&samples, 1, SAMPLE_RATE_HZ)).unwrap();
        let manifest = dir.join("manifest.json");
        fs::write(
            &manifest,
            r#"{"fixtures":[{"id":"clip","category":"ru","wav":"clip.wav","role":"heldout",
                "speech":[{"start_sample":256,"end_sample":768},
                          {"start_sample":1536,"end_sample":2048}]}]}"#,
        )
        .unwrap();

        let report = evaluate(
            &manifest,
            || MarkerScores {
                marker: 9,
                always: false,
            },
            0.5,
        )
        .unwrap();
        let boundary = report.fixtures[0].boundary.as_ref().unwrap();
        assert_eq!(boundary.matched, 1);
        assert_eq!(boundary.missed, 1);
        assert_eq!(boundary.onset_ms, 0.0);

        let endpoint = evaluate_endpoint(
            &manifest,
            || MarkerScores {
                marker: 9,
                always: false,
            },
            0.5,
            SegmentConfig {
                min_segment_samples: FRAME_SAMPLES,
                start_voice_samples: FRAME_SAMPLES,
                end_silence_samples: FRAME_SAMPLES,
                pre_roll_samples: 0,
                ..SegmentConfig::default()
            },
        )
        .unwrap();
        let metrics = endpoint.fixtures[0].endpoint.as_ref().unwrap();
        assert_eq!(metrics.annotated, 2);
        assert_eq!(metrics.missed, 1);
        assert!(metrics.detected >= 1);
    }
}
