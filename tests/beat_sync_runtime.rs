use neuromita::music::{
    BeatBackend, BeatCache,
    runtime::{BeatRuntime, MusicJob, BeatRuntimeError},
    wav::WavError,
};
use std::fs;

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("beats-live-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn wav_pulses() -> Vec<u8> {
    let sr = 16_000_u32;
    let duration = 6_u32;
    let frames = sr * duration;
    let size = frames * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + size).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&sr.to_le_bytes());
    out.extend_from_slice(&(sr * 2).to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&size.to_le_bytes());
    for n in 0..frames {
        // A ten-millisecond pulse on each 500-ms beat.
        let sample: i16 = if n % (sr / 2) < (sr / 100) && n > 0 {
            20_000
        } else { 0 };
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

fn job(stream: bool) -> MusicJob<'static> {
    MusicJob {
        request_id: "beat-42", track_name: "test", audio_path: "fixture.wav",
        stream, min_confidence: 0.0, backend: BeatBackend::Dsp, enabled: true,
    }
}

#[test]
fn real_wav_pulses_produce_stable_dsp_timing_and_cache() {
    let tmp = Temp::new();
    let service = BeatRuntime::new(BeatCache::new(&tmp.0));
    let wav = wav_pulses();
    let initial = service.analyze(&wav, &job(false), &|| false).unwrap();
    assert!(initial.timeline.beats.len() >= 7, "{:?}", initial.timeline.beats);
    assert!(initial.timeline.beats.len() <= 13);
    assert_eq!(initial.timeline.sample_rate_hz, 16_000);
    assert!((initial.timeline.duration_seconds - 6.0).abs() < 1.0e-6);
    assert_eq!(initial.timeline.method, "dsp_energy_onset_v1");
    assert!(initial.timeline.bpm.is_some_and(|bpm| (bpm - 120.0).abs() <= 2.0));
    let second = service.analyze(&wav, &job(false), &|| false).unwrap();
    assert_eq!(initial.timeline, second.timeline);
    assert_eq!(second.origin, neuromita::music::BeatResultOrigin::Cache);
}

#[test]
fn stream_and_nonstream_frames_match_legacy_delivery_contract() {
    let tmp = Temp::new();
    let service = BeatRuntime::new(BeatCache::new(&tmp.0));
    let audio = wav_pulses();
    let mut stream = Vec::new();
    service.run(&audio, &job(true), &|| false, |v| stream.push(v)).unwrap();
    assert_eq!(stream.iter().map(|v| v["type"].as_str().unwrap()).collect::<Vec<_>>(),
               ["music_beats_job_started", "music_beats_chunk", "music_beats_ready"]);
    assert_eq!(stream[1]["body"]["chunks_total"], 1);
    assert_eq!(stream[1]["body"]["progress"], 1.0);
    assert!(stream[1]["body"]["beats"].as_array().unwrap().len() > 5);
    assert!(stream[2]["body"].get("beats").is_none());
    let mut single = Vec::new();
    service.run(&audio, &job(false), &|| false, |v| single.push(v)).unwrap();
    assert_eq!(single.len(), 2);
    assert_eq!(single[1]["type"], "music_beats_ready");
    assert_eq!(single[1]["body"]["beats"], stream[1]["body"]["beats"]);
    assert_eq!(single[1]["body"]["request_id"], "beat-42");
}

#[test]
fn malformed_unsupported_audio_reports_error_and_never_caches() {
    let tmp = Temp::new();
    let service = BeatRuntime::new(BeatCache::new(&tmp.0));
    let mut frames = Vec::new();
    let outcome = service.run(b"not a WAV file at all", &job(false), &|| false,
                              |v| frames.push(v));
    assert!(matches!(outcome, Err(BeatRuntimeError::Audio(WavError::Size))));
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["type"], "music_beats_job_started");
    assert_eq!(frames[1]["type"], "music_beats_error");
    assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 0);
}

#[test]
fn cancellation_and_disabled_state_are_explicit() {
    let tmp = Temp::new();
    let service = BeatRuntime::new(BeatCache::new(&tmp.0));
    let wav = wav_pulses();
    let mut frames = Vec::new();
    let result = service.run(&wav, &job(true), &|| true, |v| frames.push(v));
    assert!(matches!(result, Err(BeatRuntimeError::Audio(WavError::Cancelled))));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["type"], "music_beats_error");
    let mut disabled = job(false);
    disabled.enabled = false;
    frames.clear();
    assert!(matches!(service.run(&wav, &disabled, &|| false, |v| frames.push(v)),
                     Err(BeatRuntimeError::Disabled)));
    assert_eq!(frames[0]["type"], "music_beats_error");
}

#[test]
fn unavailable_models_are_not_misreported_as_native_dsp() {
    let tmp = Temp::new();
    let service = BeatRuntime::new(BeatCache::new(&tmp.0));
    let mut requested = job(false);
    requested.backend = BeatBackend::BeatThis;
    assert!(matches!(service.analyze(&wav_pulses(), &requested, &|| false),
                     Err(BeatRuntimeError::BackendUnavailable)));
}

#[test]
fn source_reader_returns_immutable_bounded_file_snapshot() {
    let tmp = Temp::new();
    let path = tmp.0.join("music.wav");
    let source = wav_pulses();
    fs::write(&path, &source).unwrap();
    let bytes = BeatRuntime::read_source(&path).unwrap();
    fs::write(&path, b"changed").unwrap();
    assert_eq!(bytes, source);
}
