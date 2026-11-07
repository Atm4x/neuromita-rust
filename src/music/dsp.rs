//! Deterministic native energy-onset fallback. This intentionally does not
//! claim spectral-flux or Beat This/Librosa model parity.
use super::{BeatPoint, BeatTimeline, wav::{WavAudio, WavError}};

const MIN_GAP_SECONDS: f64 = 0.18;

pub fn detect_beats(audio: &WavAudio, cancelled: &impl Fn() -> bool) -> Result<BeatTimeline, WavError> {
    let sr = audio.sample_rate_hz;
    let window = ((sr as usize) / 100).max(1); // 10 ms
    let mut energies = Vec::with_capacity(audio.samples.len().div_ceil(window));
    for (i, frame) in audio.samples.chunks(window).enumerate() {
        if i % 128 == 0 && cancelled() {
            return Err(WavError::Cancelled);
        }
        let power = frame.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>()
            / frame.len() as f64;
        energies.push(power.sqrt());
    }
    let peak = energies.iter().copied().fold(0.0_f64, f64::max);
    let mut beats = Vec::new();
    if peak > 1.0e-5 && energies.len() >= 3 {
        let mut last_time: Option<f64> = None;
        for i in 1..energies.len() - 1 {
            if i % 128 == 0 && cancelled() {
                return Err(WavError::Cancelled);
            }
            let before = energies[i.saturating_sub(5)..i].iter().sum::<f64>()
                / (i - i.saturating_sub(5)) as f64;
            let delta = (energies[i] - before).max(0.0);
            let time = i as f64 * window as f64 / sr as f64;
            if energies[i] >= energies[i - 1]
                && energies[i] > energies[i + 1]
                && delta >= peak * 0.12
                && energies[i] >= peak * 0.08
                && last_time.is_none_or(|prev| time - prev >= MIN_GAP_SECONDS)
            {
                beats.push(BeatPoint {
                    time_seconds: time,
                    confidence: (delta / peak).clamp(0.0, 1.0) as f32,
                });
                last_time = Some(time);
            }
        }
    }
    let bpm = if beats.len() >= 3 {
        let mut intervals: Vec<f64> = beats.windows(2)
            .map(|pair| pair[1].time_seconds - pair[0].time_seconds)
            .filter(|gap| *gap > 0.001 && *gap < 2.0)
            .collect();
        if intervals.is_empty() {
            None
        } else {
            intervals.sort_by(f64::total_cmp);
            let mut tempo = 60.0 / intervals[intervals.len() / 2];
            while tempo < 70.0 { tempo *= 2.0; }
            while tempo > 190.0 { tempo *= 0.5; }
            Some(tempo)
        }
    } else {
        None
    };
    let result = BeatTimeline {
        beats,
        duration_seconds: audio.samples.len() as f64 / sr as f64,
        sample_rate_hz: sr,
        method: "dsp_energy_onset_v1".to_owned(),
        bpm,
    };
    // A faulty analyzer must never be allowed to populate the cache.
    result.validate().map_err(|_| WavError::Invalid)?;
    Ok(result)
}
