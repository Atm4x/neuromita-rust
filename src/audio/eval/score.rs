//! Stateful score providers and bounded framing; endpoint policy belongs to the segmenter.

use thiserror::Error;

use super::super::vad::{FRAME_SAMPLES, SAMPLE_RATE_HZ};

#[derive(Debug, Error)]
pub enum ScoreError {
    #[error("invalid VAD framing: {0}")]
    Framing(&'static str),
    #[error("VAD provider failed: {0}")]
    Provider(String),
    #[error("VAD provider returned an invalid probability")]
    Probability,
    #[error("VAD score stream failed; reset is required before continuing")]
    ResetRequired,
}

pub trait VadScoreProvider: Send {
    fn frame_samples(&self) -> usize;
    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError>;
}

#[derive(Default)]
pub struct DirectEarshotScores(earshot::Detector);

impl VadScoreProvider for DirectEarshotScores {
    fn frame_samples(&self) -> usize {
        FRAME_SAMPLES
    }

    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
        let frame: &[i16; FRAME_SAMPLES] = frame
            .try_into()
            .map_err(|_| ScoreError::Framing("Earshot requires 256 samples"))?;
        Ok(self.0.predict_i16(frame))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ScoredFrame {
    pub start_sample: u64,
    pub end_sample: u64,
    pub probability: f32,
}

pub struct ScoreFrames<P> {
    provider: P,
    pending: Vec<i16>,
    frame_samples: usize,
    scored_samples: u64,
    poisoned: bool,
}

impl<P: VadScoreProvider> ScoreFrames<P> {
    pub fn new(provider: P) -> Result<Self, ScoreError> {
        let frame_samples = provider.frame_samples();
        if ![256, 512].contains(&frame_samples) {
            return Err(ScoreError::Framing("unsupported provider frame size"));
        }
        Ok(Self {
            provider,
            pending: Vec::with_capacity(frame_samples),
            frame_samples,
            scored_samples: 0,
            poisoned: false,
        })
    }

    /// Accept at most one second of mono PCM16 at 16 kHz; never pad a partial frame.
    pub fn push(&mut self, samples: &[i16]) -> Result<Vec<ScoredFrame>, ScoreError> {
        if self.poisoned {
            return Err(ScoreError::ResetRequired);
        }
        if samples.len() > SAMPLE_RATE_HZ as usize {
            return Err(ScoreError::Framing("input chunk exceeds one second"));
        }
        let mut scores =
            Vec::with_capacity((self.pending.len() + samples.len()) / self.frame_samples);
        let mut remaining = samples;
        while !remaining.is_empty() {
            let take = (self.frame_samples - self.pending.len()).min(remaining.len());
            self.pending.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.pending.len() == self.frame_samples {
                self.poisoned = true;
                let probability = self.provider.score(&self.pending)?;
                if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
                    return Err(ScoreError::Probability);
                }
                let end_sample = self
                    .scored_samples
                    .checked_add(self.frame_samples as u64)
                    .ok_or(ScoreError::Framing("sample offset overflow"))?;
                scores.push(ScoredFrame {
                    start_sample: self.scored_samples,
                    end_sample,
                    probability,
                });
                self.scored_samples = end_sample;
                self.pending.clear();
                self.poisoned = false;
            }
        }
        Ok(scores)
    }
}
