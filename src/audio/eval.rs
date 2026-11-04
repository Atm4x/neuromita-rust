//! Offline VAD evaluation. Annotation provenance and decision times are explicit.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    score::{ScoreError, ScoreFrames, ScoredFrame, VadScoreProvider},
    segment::{
        ScoreGate, SegmentConfig, SegmentEndReason, SegmentError, SpeechMode, SpeechSegment,
        SpeechSegmenter,
    },
    vad::{FRAME_SAMPLES, SAMPLE_RATE_HZ, VoiceThreshold},
};

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
    pub end_reason: SegmentEndReason,
}

impl From<SpeechSegment> for EmittedSegment {
    fn from(segment: SpeechSegment) -> Self {
        Self {
            start_sample: segment.start_sample,
            end_sample: segment.end_sample,
            onset_decided_at_sample: segment.onset_decided_at_sample,
            emitted_at_sample: segment.emitted_at_sample,
            end_reason: segment.end_reason,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EndpointMetrics {
    pub boundary: BoundaryError,
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

pub fn evaluate<P, F>(
    manifest_path: &Path,
    new_provider: F,
    threshold: f32,
) -> Result<Report, EvalError>
where
    P: VadScoreProvider,
    F: Fn() -> Result<P, ScoreError>,
{
    evaluate_inner(manifest_path, new_provider, threshold, None, false)
}

pub fn evaluate_endpoint<P, F>(
    manifest_path: &Path,
    new_provider: F,
    threshold: f32,
    config: SegmentConfig,
) -> Result<Report, EvalError>
where
    P: VadScoreProvider,
    F: Fn() -> Result<P, ScoreError>,
{
    evaluate_endpoint_with_policy(manifest_path, new_provider, threshold, config, false)
}

/// Strict mode rejects the entire manifest before constructing providers if any
/// fixture lacks manual speech labels or explicitly verified noise labels.
pub fn evaluate_endpoint_with_policy<P, F>(
    manifest_path: &Path,
    new_provider: F,
    threshold: f32,
    config: SegmentConfig,
    validated_only: bool,
) -> Result<Report, EvalError>
where
    P: VadScoreProvider,
    F: Fn() -> Result<P, ScoreError>,
{
    evaluate_inner(
        manifest_path,
        new_provider,
        threshold,
        Some(config),
        validated_only,
    )
}

fn evaluate_inner<P, F>(
    manifest_path: &Path,
    new_provider: F,
    threshold: f32,
    config: Option<SegmentConfig>,
    validated_only: bool,
) -> Result<Report, EvalError>
where
    P: VadScoreProvider,
    F: Fn() -> Result<P, ScoreError>,
{
    let voice = VoiceThreshold::new(threshold)
        .map_err(|_| EvalError::Manifest("threshold must be in 0..=1".into()))?;
    if config.is_some_and(|config| config.mode == SpeechMode::PushToTalk) {
        return Err(EvalError::Manifest(
            "endpoint evaluation requires automatic mode; no PTT button events are supplied".into(),
        ));
    }
    let manifest = Manifest::load(manifest_path)?;
    if validated_only {
        let invalid: Vec<_> = manifest
            .fixtures
            .iter()
            .filter(|fixture| !fixture.annotation_status.is_validated())
            .map(|fixture| fixture.id.as_str())
            .collect();
        if !invalid.is_empty() {
            return Err(EvalError::Manifest(format!(
                "validated-only rejects: {}",
                invalid.join(", ")
            )));
        }
    }
    let base = fixture_root(manifest_path);
    let mut fixtures = Vec::with_capacity(manifest.fixtures.len());
    for fixture in manifest.fixtures {
        let samples = read_pcm16_mono_wav(&base.join(&fixture.wav))?;
        let speech = fixture.speech.as_deref().unwrap_or_default();
        if speech
            .iter()
            .any(|interval| interval.end_sample > samples.len() as u64)
        {
            return Err(EvalError::Manifest(format!(
                "fixture {} interval exceeds audio length {}",
                fixture.id,
                samples.len()
            )));
        }
        let scores = score_fixture(&samples, new_provider()?)?;
        let scored_samples = scores.last().map_or(0, |score| score.end_sample);
        let labeled = fixture.annotation_status != AnnotationStatus::Unknown;
        let runs = predicted_runs(&scores, threshold);
        let metrics = labeled.then(|| frame_metrics(&scores, speech, threshold));
        let boundary = labeled.then(|| boundary_error(&runs, speech));
        let mut emitted_segments = Vec::new();
        if let Some(config) = config {
            let gate = ScoreGate::new(new_provider()?, voice)?;
            let mut segmenter = SpeechSegmenter::new(gate, config)?;
            for frame in samples.as_chunks::<FRAME_SAMPLES>().0 {
                if let Some(segment) = segmenter.process(*frame)? {
                    emitted_segments.push(segment.into());
                }
            }
            if let Some(segment) = segmenter.flush() {
                emitted_segments.push(segment.into());
            }
        }
        let endpoint =
            (labeled && config.is_some()).then(|| endpoint_metrics(&emitted_segments, speech));
        fixtures.push(FixtureReport {
            id: fixture.id,
            category: fixture.category,
            role: fixture.role,
            annotation_status: fixture.annotation_status,
            audio_samples: samples.len() as u64,
            scored_samples,
            unscored_tail_samples: samples.len() as u64 - scored_samples,
            endpoint_unprocessed_tail_samples: config
                .map(|_| (samples.len() % FRAME_SAMPLES) as u64),
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
    let runs: Vec<_> = segments
        .iter()
        .map(|s| (s.start_sample, s.end_sample))
        .collect();
    let mut boundary = boundary_error(&runs, speech);
    let (annotations, predictions) = degrees(&runs, speech);
    let mut onset = Vec::new();
    let mut emission = Vec::new();
    let mut ends = Vec::new();
    let mut censored_ends = Vec::new();
    for (i, interval) in speech.iter().enumerate() {
        if annotations[i] != 1 {
            continue;
        }
        if let Some((j, segment)) = segments
            .iter()
            .enumerate()
            .find(|(_, s)| overlaps((s.start_sample, s.end_sample), interval))
        {
            if predictions[j] != 1 {
                continue;
            }
            if let Some(decided) = segment.onset_decided_at_sample {
                onset.push(delta_ms(decided, interval.start_sample));
            }
            let error = delta_ms(segment.end_sample, interval.end_sample);
            if segment.end_reason == SegmentEndReason::Eof {
                censored_ends.push(error);
            } else {
                ends.push(error);
            }
            if segment.end_reason == SegmentEndReason::Silence {
                emission.push(delta_ms(segment.emitted_at_sample, interval.end_sample));
            }
        }
    }
    boundary.audio_end_error_ms = mean(&ends);
    EndpointMetrics {
        boundary,
        onset_decision_delay_ms: mean(&onset),
        silence_emission_delay_ms: mean(&emission),
        eof_censored_ends: segments
            .iter()
            .filter(|s| s.end_reason == SegmentEndReason::Eof)
            .count() as u64,
        eof_censored_audio_end_error_ms: mean(&censored_ends),
        spurious_seconds: segments
            .iter()
            .zip(predictions)
            .filter(|(_, n)| *n == 0)
            .map(|(s, _)| (s.end_sample - s.start_sample) as f32 / SAMPLE_RATE_HZ as f32)
            .sum(),
    }
}

fn score_fixture<P: VadScoreProvider>(
    samples: &[i16],
    provider: P,
) -> Result<Vec<ScoredFrame>, EvalError> {
    let mut framing = ScoreFrames::new(provider)?;
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
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct FixtureFiles(PathBuf);

    impl FixtureFiles {
        fn new(samples: Vec<i16>, labels: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "neuromita-eval-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            fs::write(
                root.join("clip.wav"),
                SpeechSegment::from_samples(samples).as_wav().unwrap(),
            )
            .unwrap();
            fs::write(root.join("manifest.json"), format!(
                r#"{{"fixtures":[{{"id":"clip","category":"test","wav":"clip.wav","role":"heldout"{labels}}}]}}"#
            )).unwrap();
            Self(root)
        }

        fn manifest(&self) -> PathBuf {
            self.0.join("manifest.json")
        }
    }

    impl Drop for FixtureFiles {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct MarkerScores {
        frame: usize,
    }

    impl VadScoreProvider for MarkerScores {
        fn frame_samples(&self) -> usize {
            self.frame
        }
        fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
            Ok(if frame.contains(&7) { 1.0 } else { 0.0 })
        }
        fn reset(&mut self) {}
    }

    fn provider() -> Result<MarkerScores, ScoreError> {
        Ok(MarkerScores { frame: 256 })
    }
    fn annotation(start_sample: u64, end_sample: u64) -> Annotation {
        Annotation {
            start_sample,
            end_sample,
        }
    }
    fn emitted(
        start: u64,
        end: u64,
        onset: Option<u64>,
        at: u64,
        reason: SegmentEndReason,
    ) -> EmittedSegment {
        EmittedSegment {
            start_sample: start,
            end_sample: end,
            onset_decided_at_sample: onset,
            emitted_at_sample: at,
            end_reason: reason,
        }
    }

    #[test]
    fn missing_status_or_labels_never_proves_silence() {
        for labels in [
            "",
            r#", "speech":[]"#,
            r#", "annotation_status":"unknown", "speech":[{"start_sample":0,"end_sample":256}]"#,
        ] {
            let files = FixtureFiles::new(vec![7; 512], labels);
            let report =
                evaluate_endpoint(&files.manifest(), provider, 0.5, SegmentConfig::default())
                    .unwrap();
            let fixture = &report.fixtures[0];
            assert_eq!(fixture.annotation_status, AnnotationStatus::Unknown);
            assert!(fixture.metrics.is_none());
            assert!(fixture.boundary.is_none());
            assert!(fixture.endpoint.is_none());
            assert_eq!(fixture.scored_samples, 512);
        }
        let files = FixtureFiles::new(vec![0; 512], r#", "annotation_status":"manual""#);
        assert!(Manifest::load(&files.manifest()).is_err());
    }

    #[test]
    fn strict_policy_rejects_heuristic_and_unknown_before_provider_creation() {
        for status in ["heuristic", "unknown"] {
            let files = FixtureFiles::new(
                vec![0; 512],
                &format!(r#", "annotation_status":"{status}", "speech":[]"#),
            );
            let result = evaluate_endpoint_with_policy::<MarkerScores, _>(
                &files.manifest(),
                || panic!("must reject first"),
                0.5,
                SegmentConfig::default(),
                true,
            );
            assert!(matches!(result, Err(EvalError::Manifest(_))));
        }
        for status in ["manual", "verified_noise"] {
            let files = FixtureFiles::new(
                vec![0; 512],
                &format!(r#", "annotation_status":"{status}", "speech":[]"#),
            );
            let report = evaluate_endpoint_with_policy(
                &files.manifest(),
                provider,
                0.5,
                SegmentConfig::default(),
                true,
            )
            .unwrap();
            assert!(report.fixtures[0].metrics.is_some());
            assert!(report.validated_only);
        }
    }

    #[test]
    fn labels_must_fit_audio_even_when_unknown() {
        for status in ["manual", "unknown"] {
            let files = FixtureFiles::new(
                vec![0; 512],
                &format!(
                    r#", "annotation_status":"{status}", "speech":[{{"start_sample":256,"end_sample":513}}]"#
                ),
            );
            assert!(matches!(
                evaluate(&files.manifest(), provider, 0.5),
                Err(EvalError::Manifest(_))
            ));
        }
    }

    #[test]
    fn malformed_annotations_and_duplicate_ids_are_rejected() {
        for speech in [
            r#"[{"start_sample":500,"end_sample":300}]"#,
            r#"[{"start_sample":0,"end_sample":300},{"start_sample":256,"end_sample":512}]"#,
        ] {
            let files = FixtureFiles::new(
                vec![0; 512],
                &format!(r#", "annotation_status":"manual", "speech":{speech}"#),
            );
            assert!(Manifest::load(&files.manifest()).is_err());
        }
        let files = FixtureFiles::new(
            vec![0; 512],
            r#", "annotation_status":"verified_noise", "speech":[{"start_sample":0,"end_sample":256}]"#,
        );
        assert!(Manifest::load(&files.manifest()).is_err());
        let mut manifest: Manifest = serde_json::from_str(
            r#"{"fixtures":[{"id":"a","category":"test","wav":"a.wav","role":"heldout"}]}"#,
        )
        .unwrap();
        manifest.fixtures.push(manifest.fixtures[0].clone());
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn merge_and_split_are_coverage_not_correct_utterance_matches() {
        let merged = boundary_error(&[(0, 1024)], &[annotation(100, 400), annotation(600, 900)]);
        assert_eq!(
            (merged.covered, merged.matched, merged.merged, merged.missed),
            (2, 0, 1, 0)
        );
        assert!(merged.audio_start_error_ms.is_none());
        let split = boundary_error(&[(100, 300), (400, 700)], &[annotation(100, 700)]);
        assert_eq!((split.covered, split.matched, split.split), (1, 0, 1));
        assert!(split.audio_end_error_ms.is_none());
        let missed = boundary_error(&[], &[annotation(100, 700)]);
        assert_eq!(missed.missed, 1);
        assert!(missed.audio_start_error_ms.is_none());
        assert!(missed.audio_end_error_ms.is_none());
    }

    #[test]
    fn signed_audio_errors_are_separate_from_actual_decision_times() {
        let segments = [emitted(0, 768, Some(512), 1024, SegmentEndReason::Silence)];
        let metrics = endpoint_metrics(&segments, &[annotation(256, 800)]);
        assert_eq!(metrics.boundary.audio_start_error_ms, Some(-16.0));
        assert_eq!(metrics.boundary.audio_end_error_ms, Some(-2.0));
        assert_eq!(metrics.onset_decision_delay_ms, Some(16.0));
        assert_eq!(metrics.silence_emission_delay_ms, Some(14.0));
    }

    #[test]
    fn eof_is_censored_and_forced_cuts_are_not_silence_endpoints() {
        for reason in [
            SegmentEndReason::Eof,
            SegmentEndReason::Maximum,
            SegmentEndReason::Release,
        ] {
            let metrics =
                endpoint_metrics(&[emitted(0, 512, None, 512, reason)], &[annotation(0, 600)]);
            assert!(metrics.onset_decision_delay_ms.is_none());
            assert!(metrics.silence_emission_delay_ms.is_none());
            if reason == SegmentEndReason::Eof {
                assert_eq!(metrics.eof_censored_ends, 1);
                assert!(metrics.boundary.audio_end_error_ms.is_none());
                assert_eq!(metrics.eof_censored_audio_end_error_ms, Some(-5.5));
            } else {
                assert_eq!(metrics.eof_censored_ends, 0);
                assert_eq!(metrics.boundary.audio_end_error_ms, Some(-5.5));
            }
        }
    }

    #[test]
    fn sample_weighting_agrees_across_provider_windows_and_preserves_partial_overlap() {
        let speech = [annotation(128, 384)];
        let wide = [ScoredFrame {
            start_sample: 0,
            end_sample: 512,
            probability: 1.0,
        }];
        let narrow = [
            ScoredFrame {
                start_sample: 0,
                end_sample: 256,
                probability: 1.0,
            },
            ScoredFrame {
                start_sample: 256,
                end_sample: 512,
                probability: 1.0,
            },
        ];
        let a = frame_metrics(&wide, &speech, 0.5);
        let b = frame_metrics(&narrow, &speech, 0.5);
        assert_eq!(a.true_positive_samples, 256);
        assert_eq!(a.false_positive_samples, 256);
        assert_eq!(a.f1, b.f1);
        assert_eq!(a.false_positive_samples, b.false_positive_samples);
        assert_eq!(a.true_positive_samples, b.true_positive_samples);
    }

    #[test]
    fn incomplete_provider_and_capture_tails_are_explicit() {
        let files = FixtureFiles::new(
            vec![7; 800],
            r#", "annotation_status":"manual", "speech":[{"start_sample":0,"end_sample":800}]"#,
        );
        let report = evaluate_endpoint(
            &files.manifest(),
            || Ok(MarkerScores { frame: 512 }),
            0.5,
            SegmentConfig::default(),
        )
        .unwrap();
        let fixture = &report.fixtures[0];
        assert_eq!(fixture.scored_samples, 512);
        assert_eq!(fixture.unscored_tail_samples, 288);
        assert_eq!(fixture.endpoint_unprocessed_tail_samples, Some(32));
        assert_eq!(fixture.metrics.as_ref().unwrap().true_positive_samples, 512);
        assert_eq!(fixture.metrics.as_ref().unwrap().false_negative_samples, 0);
    }

    #[test]
    fn endpoint_report_consumes_segmenter_metadata_with_preroll_and_eof() {
        let mut samples = vec![0; 512];
        samples.extend(vec![7; 1024]);
        let files = FixtureFiles::new(
            samples,
            r#", "annotation_status":"manual", "speech":[{"start_sample":512,"end_sample":1536}]"#,
        );
        let config = SegmentConfig {
            pre_roll_samples: 768,
            start_voice_samples: 512,
            min_segment_samples: 256,
            end_silence_samples: 256,
            ..SegmentConfig::default()
        };
        let report = evaluate_endpoint(&files.manifest(), provider, 0.5, config).unwrap();
        let fixture = &report.fixtures[0];
        assert_eq!(fixture.emitted_segments.len(), 1);
        let segment = &fixture.emitted_segments[0];
        assert!(segment.start_sample < 512);
        assert_eq!(segment.onset_decided_at_sample, Some(1024));
        assert_eq!(segment.emitted_at_sample, 1536);
        assert_eq!(segment.end_reason, SegmentEndReason::Eof);
        assert_eq!(
            fixture.endpoint.as_ref().unwrap().onset_decision_delay_ms,
            Some(32.0)
        );
        assert!(
            fixture
                .endpoint
                .as_ref()
                .unwrap()
                .boundary
                .audio_end_error_ms
                .is_none()
        );
    }

    #[test]
    fn unknown_fixtures_retain_emitted_segments() {
        let files = FixtureFiles::new(vec![7; 2048], "");
        let report = evaluate_endpoint(
            &files.manifest(),
            provider,
            0.5,
            SegmentConfig {
                min_segment_samples: 256,
                ..SegmentConfig::default()
            },
        )
        .unwrap();
        let fixture = &report.fixtures[0];
        assert!(fixture.metrics.is_none());
        assert!(fixture.endpoint.is_none());
        assert_eq!(fixture.emitted_segments.len(), 1);
        assert_eq!(fixture.emitted_segments[0].end_sample, 2048);
        assert_eq!(
            fixture.emitted_segments[0].end_reason,
            SegmentEndReason::Eof
        );
    }

    #[test]
    fn endpoint_rejects_ptt_without_button_events() {
        let config = SegmentConfig {
            mode: SpeechMode::PushToTalk,
            ..SegmentConfig::default()
        };
        let result = evaluate_endpoint::<MarkerScores, _>(
            Path::new("unused-manifest.json"),
            || panic!("must reject PTT first"),
            0.5,
            config,
        );
        assert!(matches!(result, Err(EvalError::Manifest(_))));
    }

    #[test]
    fn provider_factory_errors_are_returned() {
        let files = FixtureFiles::new(vec![0; 512], "");
        let result = evaluate::<MarkerScores, _>(
            &files.manifest(),
            || Err(ScoreError::Provider("construction failed".into())),
            0.5,
        );
        assert!(matches!(
            result,
            Err(EvalError::Score(ScoreError::Provider(_)))
        ));
    }

    #[test]
    fn wav_parser_rejects_invalid_format_truncation_and_oversize_before_reading() {
        let files = FixtureFiles::new(vec![0; 512], "");
        let path = files.0.join("clip.wav");
        let mut bytes = fs::read(&path).unwrap();
        bytes[22..24].copy_from_slice(&2u16.to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(matches!(read_pcm16_mono_wav(&path), Err(EvalError::Wav(_))));
        fs::write(&path, b"RIFF").unwrap();
        assert!(matches!(read_pcm16_mono_wav(&path), Err(EvalError::Wav(_))));
        File::create(&path)
            .unwrap()
            .set_len(MAX_WAV_BYTES as u64 + 1)
            .unwrap();
        assert!(read_pcm16_mono_wav(&path).is_err());
        File::create(files.manifest())
            .unwrap()
            .set_len(MAX_MANIFEST_BYTES as u64 + 1)
            .unwrap();
        assert!(Manifest::load(&files.manifest()).is_err());
    }
}
