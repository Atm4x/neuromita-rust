use neuromita::audio::{eval::evaluate_direct, segment::SegmentConfig};
use std::{env, error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let manifest = args
        .next()
        .ok_or("usage: vad_core_eval <manifest.json> <threshold> [--validated-only]")?;
    let threshold: f32 = args.next().ok_or("missing threshold")?.parse()?;
    let strict = match args.next().as_deref() {
        None => false,
        Some("--validated-only") => true,
        Some(_) => return Err("unexpected flag".into()),
    };
    let report = evaluate_direct(
        Path::new(&manifest),
        threshold,
        SegmentConfig::default(),
        strict,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
