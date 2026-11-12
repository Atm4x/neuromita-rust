//! Bounded WAV PCM decoding for the native fallback beat detector.
//! Unsupported codecs fail explicitly; no implicit FFmpeg or model installation.
use thiserror::Error;

const MAX_WAV_BYTES: usize = 64 * 1024 * 1024;
const MAX_DURATION_SECONDS: u64 = 900;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WavError {
    #[error("audio is empty or exceeds the configured 64 MiB limit")]
    Size,
    #[error("invalid RIFF/WAVE header or truncated chunk")]
    Invalid,
    #[error("unsupported WAV encoding, channel count or sample rate")]
    Unsupported,
    #[error("audio duration exceeds 15 minutes")]
    Duration,
    #[error("analysis was cancelled")]
    Cancelled,
}

pub struct WavAudio {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

fn le16(data: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(..2)?.try_into().ok()?))
}
fn le32(data: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?))
}

/// Read RIFF chunks without trusting declared lengths or allocating by metadata.
/// Only PCM 16/24/32 and IEEE float32 WAV are accepted.
pub fn decode_wav(bytes: &[u8], cancelled: &impl Fn() -> bool) -> Result<WavAudio, WavError> {
    if bytes.len() < 44 || bytes.len() > MAX_WAV_BYTES {
        return Err(WavError::Size);
    }
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(WavError::Invalid);
    }
    let riff_end = 8usize
        .checked_add(
            le32(bytes.get(4..8).ok_or(WavError::Invalid)?).ok_or(WavError::Invalid)? as usize,
        )
        .ok_or(WavError::Invalid)?;
    if riff_end > bytes.len() || riff_end < 44 {
        return Err(WavError::Invalid);
    }
    let mut pos = 12usize;
    let mut format = None;
    let mut audio = None;
    while pos + 8 <= riff_end {
        if cancelled() {
            return Err(WavError::Cancelled);
        }
        let len = le32(&bytes[pos + 4..pos + 8]).ok_or(WavError::Invalid)? as usize;
        let start = pos + 8;
        let end = start.checked_add(len).ok_or(WavError::Invalid)?;
        if end > riff_end {
            return Err(WavError::Invalid);
        }
        match &bytes[pos..pos + 4] {
            b"fmt " if format.is_none() => format = Some(&bytes[start..end]),
            b"data" if audio.is_none() => audio = Some(&bytes[start..end]),
            _ => {}
        }
        pos = end.checked_add(len % 2).ok_or(WavError::Invalid)?;
    }
    let fmt = format.ok_or(WavError::Invalid)?;
    let data = audio.ok_or(WavError::Invalid)?;
    if fmt.len() < 16 {
        return Err(WavError::Invalid);
    }
    let encoding = le16(&fmt[0..2]).ok_or(WavError::Invalid)?;
    let channels = le16(&fmt[2..4]).ok_or(WavError::Invalid)? as usize;
    let sample_rate_hz = le32(&fmt[4..8]).ok_or(WavError::Invalid)?;
    let block_align = le16(&fmt[12..14]).ok_or(WavError::Invalid)? as usize;
    let bits = le16(&fmt[14..16]).ok_or(WavError::Invalid)? as usize;
    let valid_format = matches!((encoding, bits), (1, 16 | 24 | 32) | (3, 32));
    let bytes_per_sample = bits / 8;
    if !valid_format
        || !(1..=8).contains(&channels)
        || !(8_000..=192_000).contains(&sample_rate_hz)
        || block_align != channels * bytes_per_sample
        || block_align == 0
    {
        return Err(WavError::Unsupported);
    }
    if data.len() % block_align != 0 {
        return Err(WavError::Invalid);
    }
    let frames = data.len() / block_align;
    if (frames as u64) > (sample_rate_hz as u64) * MAX_DURATION_SECONDS {
        return Err(WavError::Duration);
    }
    let mut samples = Vec::with_capacity(frames);
    for (index, frame) in data.chunks_exact(block_align).enumerate() {
        if index % 4096 == 0 && cancelled() {
            return Err(WavError::Cancelled);
        }
        let mut sum = 0.0_f64;
        for sample in frame.chunks_exact(bytes_per_sample) {
            let value = match (encoding, bits) {
                (1, 16) => i16::from_le_bytes([sample[0], sample[1]]) as f64 / 32768.0,
                (1, 24) => {
                    let signed = i32::from_le_bytes([
                        sample[0],
                        sample[1],
                        sample[2],
                        if sample[2] & 0x80 == 0 { 0 } else { 0xff },
                    ]);
                    signed as f64 / 8_388_608.0
                }
                (1, 32) => {
                    i32::from_le_bytes(sample.try_into().map_err(|_| WavError::Invalid)?) as f64
                        / 2_147_483_648.0
                }
                (3, 32) => {
                    f32::from_le_bytes(sample.try_into().map_err(|_| WavError::Invalid)?) as f64
                }
                _ => return Err(WavError::Unsupported),
            };
            if !value.is_finite() || value.abs() > 1.001 {
                return Err(WavError::Invalid);
            }
            sum += value;
        }
        samples.push((sum / channels as f64) as f32);
    }
    Ok(WavAudio {
        samples,
        sample_rate_hz,
    })
}
