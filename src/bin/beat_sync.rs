//! Manual Beat Sync smoke entry point:
//! cargo run --locked --offline --bin beat_sync -- <input.wav> <cache-dir>
use neuromita::music::{
    BeatBackend, BeatCache,
    runtime::{BeatRuntime, MusicJob},
};
use std::{env, path::PathBuf};

fn main() {
    let mut args = env::args().skip(1);
    let (Some(file), Some(cache_dir), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: beat_sync <input.wav> <cache-dir>");
        std::process::exit(2);
    };
    let source = PathBuf::from(&file);
    let bytes = match BeatRuntime::read_source(&source) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("Beat Sync input failed: {error}");
            std::process::exit(1);
        }
    };
    let runtime = BeatRuntime::new(BeatCache::new(cache_dir));
    let job = MusicJob {
        request_id: "manual-smoke",
        track_name: source
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("audio"),
        audio_path: &file,
        stream: false,
        min_confidence: 0.2,
        backend: BeatBackend::Dsp,
        enabled: true,
    };
    let result = runtime.run(&bytes, &job, &|| false, |event| {
        println!("{event}");
    });
    if let Err(error) = result {
        eprintln!("Beat Sync analysis failed: {error}");
        std::process::exit(1);
    }
}
