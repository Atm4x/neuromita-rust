//! Offline VAD evaluation. Annotation provenance and decision times are explicit.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod score;

use super::{
    segment::{SegmentConfig, SegmentError, SpeechMode, SpeechSegment, SpeechSegmenter},
    vad::{EarshotVad, FRAME_SAMPLES, SAMPLE_RATE_HZ, VoiceThreshold},
};
use score::{DirectEarshotScores, ScoreError, ScoreFrames, ScoredFrame};

pub const MAX_FIXTURE_SAMPLES: usize = 30 * SAMPLE_RATE_HZ as usize;
const MAX_WAV_BYTES: usize = MAX_FIXTURE_SAMPLES * 2 + 64 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationStatus {
    #[default]
    Unknown,
    Heuristic,
    Manual,
    VerifiedNoise,
}

impl AnnotationStatus {
    pub fn is_validated(self) -> bool {
        matches!(self, Self::Manual | Self::VerifiedNoise)
    }
}

/// Half-open absolute sample interval; a transcript alone is not an annotation.
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
    pub annotation_status: AnnotationStatus,
    pub speech: Option<Vec<Annotation>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub fixtures: Vec<Fixture>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, EvalError> {
        let raw = read_bounded(path, MAX_MANIFEST_BYTES)?;
        let manifest: Self =
            serde_json::from_slice(&raw).map_err(|error| EvalError::Manifest(error.to_string()))?;
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
            let wav_path = Path::new(&fixture.wav);
            if wav_path.components().count() != 1
                || !matches!(
                    wav_path.components().next(),
                    Some(std::path::Component::Normal(_))
                )
                || !fixture.wav.ends_with(".wav")
                || fixture.wav.contains('\\')
            {
                return Err(EvalError::Manifest(format!(
                    "fixture {} must use a simple relative wav filename",
                    fixture.id
                )));
            }
            if !seen.insert(fixture.id.as_str()) {
                return Err(EvalError::Manifest(format!(
                    "duplicate fixture id {}",
                    fixture.id
                )));
            }
            if fixture.annotation_status != AnnotationStatus::Unknown && fixture.speech.is_none() {
                return Err(EvalError::Manifest(format!(
                    "fixture {} lacks explicit speech labels",
                    fixture.id
                )));
            }
            let speech = fixture.speech.as_deref().unwrap_or_default();
            if fixture.annotation_status == AnnotationStatus::VerifiedNoise && !speech.is_empty() {
                return Err(EvalError::Manifest(format!(
                    "fixture {} marks speech as verified noise",
                    fixture.id
                )));
            }
            let mut previous = 0;
            for interval in speech {
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

/// Counts are sample durations, so 256- and 512-sample providers use the same units.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FrameMetrics {
    pub scored_samples: u64,
    pub true_positive_samples: u64,
    pub false_positive_samples: u64,
    pub false_negative_samples: u64,
    pub precision: f32,
    pub recall: f32,
    pub f1: f32,
    pub false_positive_seconds: f32,
}

/// Coverage is distinct from one-to-one matches. Errors use only one-to-one pairs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BoundaryError {
    pub annotated: u64,
    pub detected: u64,
    pub covered: u64,
    pub matched: u64,
    pub missed: u64,
    pub spurious: u64,
    /// Number of predictions overlapping multiple annotations.
    pub merged: u64,
    /// Number of annotations overlapping multiple predictions.
    pub split: u64,
    pub audio_start_error_ms: Option<f32>,
    pub audio_end_error_ms: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmittedSegment {
    pub start_sample: u64,
    pub end_sample: u64,
    pub onset_decided_at_sample: Option<u64>,
    pub emitted_at_sample: u64,
    /// Current Core segmenter does not expose a typed end reason.
    pub end_reason: Option<String>,
}

fn observed_segment(segment: SpeechSegment, emitted_at_sample: u64) -> EmittedSegment {
    let end_sample = emitted_at_sample;
    let start_sample = end_sample.saturating_sub(segment.samples.len() as u64);
    EmittedSegment {
        start_sample,
        end_sample,
        onset_decided_at_sample: None,
        emitted_at_sample,
        end_reason: None,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EndpointMetrics {
    pub boundary: BoundaryError,
    /// Unknown end-reason count; Core does not expose silence/EOF metadata.
    pub unknown_end_reason_count: u64,
    pub onset_decision_delay_ms: Option<f32>,
    /// Emission delay only for natural (Silence) endpoints, never forced cuts.
    pub silence_emission_delay_ms: Option<f32>,
    pub eof_censored_ends: u64,
    pub eof_censored_audio_end_error_ms: Option<f32>,
    pub spurious_seconds: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FixtureReport {
    pub id: String,
    pub category: String,
    pub role: FixtureRole,
    pub annotation_status: AnnotationStatus,
    pub audio_samples: u64,
    pub scored_samples: u64,
    pub unscored_tail_samples: u64,
    pub endpoint_unprocessed_tail_samples: Option<u64>,
    pub metrics: Option<FrameMetrics>,
    pub boundary: Option<BoundaryError>,
    pub endpoint: Option<EndpointMetrics>,
    pub emitted_segments: Vec<EmittedSegment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub threshold: f32,
    pub validated_only: bool,
    pub fixtures: Vec<FixtureReport>,
}

/// Run direct Earshot score evaluation and production Core segmentation on the
/// same verified recorded fixture. No Python or second segmenter is instantiated.
pub fn evaluate_direct(
    manifest_path: &Path,
    threshold: f32,
    config: SegmentConfig,
    validated_only: bool,
) -> Result<Report, EvalError> {
    let threshold_value = VoiceThreshold::new(threshold)
        .map_err(|_| EvalError::Manifest("threshold must be between zero and one".into()))?;
    if config.mode != SpeechMode::Automatic {
        return Err(EvalError::Manifest(
            "offline data has no PTT button events".into(),
        ));
    }
    let manifest = Manifest::load(manifest_path)?;
    if validated_only
        && manifest
            .fixtures
            .iter()
            .any(|f| !f.annotation_status.is_validated())
    {
        return Err(EvalError::Manifest(
            "unverified fixture in validated-only mode".into(),
        ));
    }
    let base = fixture_root(manifest_path);
    let mut fixtures = Vec::with_capacity(manifest.fixtures.len());
    for fixture in manifest.fixtures {
        let samples = read_pcm16_mono_wav(&base.join(&fixture.wav))?;
        let speech = fixture.speech.as_deref().unwrap_or_default();
        if speech.iter().any(|a| a.end_sample > samples.len() as u64) {
            return Err(EvalError::Manifest(format!(
                "fixture {} labels exceed audio",
                fixture.id
            )));
        }
        let scores = score_fixture(&samples)?;
        let scored_samples = scores.last().map_or(0, |s| s.end_sample);
        let labeled = fixture.annotation_status != AnnotationStatus::Unknown;
        let metrics = labeled.then(|| frame_metrics(&scores, speech, threshold));
        let boundary = labeled.then(|| boundary_error(&predicted_runs(&scores, threshold), speech));
        let mut segmenter = SpeechSegmenter::new(EarshotVad::new(threshold_value), config)?;
        let mut emitted_segments = Vec::new();
        let mut consumed = 0u64;
        for frame in samples.as_chunks::<FRAME_SAMPLES>().0 {
            consumed += FRAME_SAMPLES as u64;
            if let Some(segment) = segmenter.process(*frame) {
                emitted_segments.push(observed_segment(segment, consumed));
            }
        }
        if let Some(segment) = segmenter.flush() {
            emitted_segments.push(observed_segment(segment, consumed));
        }
        let endpoint = labeled.then(|| endpoint_metrics(&emitted_segments, speech));
        fixtures.push(FixtureReport {
            id: fixture.id,
            category: fixture.category,
            role: fixture.role,
            annotation_status: fixture.annotation_status,
            audio_samples: samples.len() as u64,
            scored_samples,
            unscored_tail_samples: samples.len() as u64 - scored_samples,
            endpoint_unprocessed_tail_samples: Some((samples.len() % FRAME_SAMPLES) as u64),
            metrics,
            boundary,
            endpoint,
            emitted_segments,
        });
    }
    Ok(Report {
        threshold,
        validated_only,
        fixtures,
    })
}

fn overlaps(run: (u64, u64), interval: &Annotation) -> bool {
    run.0 < interval.end_sample && run.1 > interval.start_sample
}

fn degrees(runs: &[(u64, u64)], speech: &[Annotation]) -> (Vec<usize>, Vec<usize>) {
    let annotations = speech
        .iter()
        .map(|interval| runs.iter().filter(|run| overlaps(**run, interval)).count())
        .collect();
    let predictions = runs
        .iter()
        .map(|run| {
            speech
                .iter()
                .filter(|interval| overlaps(*run, interval))
                .count()
        })
        .collect();
    (annotations, predictions)
}

fn boundary_error(runs: &[(u64, u64)], speech: &[Annotation]) -> BoundaryError {
    let (annotations, predictions) = degrees(runs, speech);
    let mut start_errors = Vec::new();
    let mut end_errors = Vec::new();
    for (i, interval) in speech.iter().enumerate() {
        if annotations[i] != 1 {
            continue;
        }
        if let Some((j, run)) = runs
            .iter()
            .enumerate()
            .find(|(_, run)| overlaps(**run, interval))
            && predictions[j] == 1
        {
            start_errors.push(delta_ms(run.0, interval.start_sample));
            end_errors.push(delta_ms(run.1, interval.end_sample));
        }
    }
    BoundaryError {
        annotated: speech.len() as u64,
        detected: runs.len() as u64,
        covered: annotations.iter().filter(|&&n| n > 0).count() as u64,
        matched: start_errors.len() as u64,
        missed: annotations.iter().filter(|&&n| n == 0).count() as u64,
        spurious: predictions.iter().filter(|&&n| n == 0).count() as u64,
        merged: predictions.iter().filter(|&&n| n > 1).count() as u64,
        split: annotations.iter().filter(|&&n| n > 1).count() as u64,
        audio_start_error_ms: mean(&start_errors),
        audio_end_error_ms: mean(&end_errors),
    }
}

fn endpoint_metrics(segments: &[EmittedSegment], speech: &[Annotation]) -> EndpointMetrics {
    let runs = segments
        .iter()
        .map(|s| (s.start_sample, s.end_sample))
        .collect::<Vec<_>>();
    let mut boundary = boundary_error(&runs, speech);
    // The production segmenter exposes no terminal reason or onset decision.
    // Consequently these timing metrics are unavailable, not estimated.
    boundary.audio_end_error_ms = None;
    let (_, predictions) = degrees(&runs, speech);
    EndpointMetrics {
        boundary,
        unknown_end_reason_count: segments.len() as u64,
        onset_decision_delay_ms: None,
        silence_emission_delay_ms: None,
        eof_censored_ends: 0,
        eof_censored_audio_end_error_ms: None,
        spurious_seconds: segments
            .iter()
            .zip(predictions)
            .filter(|(_, n)| *n == 0)
            .map(|(s, _)| (s.end_sample - s.start_sample) as f32 / SAMPLE_RATE_HZ as f32)
            .sum(),
    }
}

fn score_fixture(samples: &[i16]) -> Result<Vec<ScoredFrame>, EvalError> {
    let mut framing = ScoreFrames::new(DirectEarshotScores::default())?;
    let mut scores = Vec::new();
    for chunk in samples.chunks(FRAME_SAMPLES) {
        scores.extend(framing.push(chunk)?);
    }
    Ok(scores)
}

fn frame_metrics(scores: &[ScoredFrame], speech: &[Annotation], threshold: f32) -> FrameMetrics {
    let mut metrics = FrameMetrics::default();
    for score in scores {
        let duration = score.end_sample - score.start_sample;
        let expected: u64 = speech
            .iter()
            .map(|interval| {
                score
                    .end_sample
                    .min(interval.end_sample)
                    .saturating_sub(score.start_sample.max(interval.start_sample))
            })
            .sum();
        metrics.scored_samples += duration;
        if score.probability > threshold {
            metrics.true_positive_samples += expected;
            metrics.false_positive_samples += duration - expected;
        } else {
            metrics.false_negative_samples += expected;
        }
    }
    let predicted = metrics.true_positive_samples + metrics.false_positive_samples;
    let actual = metrics.true_positive_samples + metrics.false_negative_samples;
    if predicted > 0 {
        metrics.precision = metrics.true_positive_samples as f32 / predicted as f32;
    }
    if actual > 0 {
        metrics.recall = metrics.true_positive_samples as f32 / actual as f32;
    }
    if metrics.precision + metrics.recall > 0.0 {
        metrics.f1 =
            2.0 * metrics.precision * metrics.recall / (metrics.precision + metrics.recall);
    }
    metrics.false_positive_seconds = metrics.false_positive_samples as f32 / SAMPLE_RATE_HZ as f32;
    metrics
}

fn predicted_runs(scores: &[ScoredFrame], threshold: f32) -> Vec<(u64, u64)> {
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

fn delta_ms(actual: u64, expected: u64) -> f32 {
    ((actual as f64 - expected as f64) * 1000.0 / SAMPLE_RATE_HZ as f64) as f32
}

fn mean(values: &[f32]) -> Option<f32> {
    (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, EvalError> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(EvalError::Manifest(format!(
            "{} exceeds {} byte read limit",
            path.display(),
            limit
        )));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(EvalError::Manifest(format!(
            "{} exceeds {} byte read limit",
            path.display(),
            limit
        )));
    }
    Ok(bytes)
}

fn read_pcm16_mono_wav(path: &Path) -> Result<Vec<i16>, EvalError> {
    let bytes = read_bounded(path, MAX_WAV_BYTES)?;
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
    if data.len() / 2 > MAX_FIXTURE_SAMPLES {
        return Err(EvalError::Wav("fixture exceeds thirty seconds"));
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
    use std::fs;

    #[test]
    fn strict_manifest_rejects_untrusted_labels_and_traversal() {
        let mut base: Manifest = serde_json::from_str(r#"{"fixtures":[{"id":"a","category":"ru","wav":"a.wav","role":"heldout","annotation_status":"manual","speech":[{"start_sample":0,"end_sample":256}]}]}"#).unwrap();
        assert!(base.validate().is_ok());
        base.fixtures[0].wav = "../secrets.wav".into();
        assert!(base.validate().is_err());
        base.fixtures[0].wav = "a.wav".into();
        base.fixtures.push(base.fixtures[0].clone());
        assert!(base.validate().is_err());
    }

    #[test]
    fn no_invented_endpoint_latency_or_end_reason() {
        let emitted = observed_segment(
            SpeechSegment {
                samples: vec![7; 512],
            },
            1024,
        );
        assert_eq!((emitted.start_sample, emitted.end_sample), (512, 1024));
        assert!(emitted.end_reason.is_none());
        let m = endpoint_metrics(
            &[emitted],
            &[Annotation {
                start_sample: 500,
                end_sample: 900,
            }],
        );
        assert_eq!(m.unknown_end_reason_count, 1);
        assert!(m.onset_decision_delay_ms.is_none());
        assert!(m.silence_emission_delay_ms.is_none());
        assert!(m.boundary.audio_end_error_ms.is_none());
    }

    #[test]
    fn validated_corpus_runs_on_real_production_segmenter() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/eval/manifest.json");
        let report = evaluate_direct(&manifest, 0.5, SegmentConfig::default(), true).unwrap();
        assert_eq!(report.fixtures.len(), 18);
        assert!(
            report
                .fixtures
                .iter()
                .all(|f| f.annotation_status.is_validated())
        );
        assert!(report.fixtures.iter().all(|f| f.endpoint.is_some()));
        let n = report
            .fixtures
            .iter()
            .filter(|f| f.category == "noise")
            .count();
        assert_eq!(n, 6);
        let total_scored: u64 = report.fixtures.iter().map(|f| f.scored_samples).sum();
        assert!(total_scored > 0);
    }

    #[test]
    fn unknown_annotations_do_not_generate_quality_metrics() {
        let root = std::env::temp_dir().join(format!("p2-eval-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let filename = root.join("manifest.json");
        fs::write(&filename, r#"{"fixtures":[{"id":"unknown","category":"ru","wav":"clip.wav","role":"heldout","speech":null}]}"#).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/eval/ru-01.wav");
        fs::copy(source, root.join("clip.wav")).unwrap();
        let report = evaluate_direct(&filename, 0.5, SegmentConfig::default(), false).unwrap();
        assert!(report.fixtures[0].metrics.is_none());
        assert!(report.fixtures[0].boundary.is_none());
        assert!(report.fixtures[0].endpoint.is_none());
        assert!(evaluate_direct(&filename, 0.5, SegmentConfig::default(), true).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
