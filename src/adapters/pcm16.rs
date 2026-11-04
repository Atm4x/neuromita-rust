//! Shared PCM16 gain operation for remote voice transports.

use crate::{settings::Volume, voice::VoiceError};

pub(crate) fn apply_volume(samples: &mut [u8], volume: Volume) -> Result<(), VoiceError> {
    if samples.len() % 2 != 0 {
        return Err(VoiceError::Generation(
            "PCM16 audio must contain complete samples".into(),
        ));
    }
    let percent = i32::from(volume.get());
    if percent == 100 {
        return Ok(());
    }
    for chunk in samples.chunks_exact_mut(2) {
        let value = i32::from(i16::from_le_bytes([chunk[0], chunk[1]]));
        let scaled = (value * percent / 100).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        chunk.copy_from_slice(&scaled.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_volume_scales_without_corrupting_samples() {
        let mut samples = [0x00, 0x40, 0x00, 0xC0];
        apply_volume(&mut samples, Volume::new(50).unwrap()).unwrap();
        assert_eq!(samples, [0x00, 0x20, 0x00, 0xE0]);
        assert!(apply_volume(&mut [0], Volume::new(50).unwrap()).is_err());
    }
}
