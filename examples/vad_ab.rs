//! Offline A/B on the recorded corpus.
//!
//! Usage:
//!   cargo run --example vad_ab -- <manifest.json> <direct|wavekat|silero> <threshold>
//!
//! `silero` requires --features vad-silero plus NEUROMITA_SILERO_MODEL and
//! NEUROMITA_ORT_DLL; `wavekat` requires --features vad-wavekat.

use std::{env, error::Error, path::Path};

use neuromita::audio::eval::evaluate;
use neuromita::audio::score::DirectEarshotScores;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let [manifest, provider, threshold] = args.as_slice() else {
        return Err("usage: vad_ab <manifest.json> <direct|wavekat|silero> <threshold>".into());
    };
    let threshold: f32 = threshold.parse()?;
    let manifest = Path::new(manifest);

    let report = match provider.as_str() {
        "direct" => evaluate(manifest, DirectEarshotScores::default, threshold)?,
        "wavekat" => {
            #[cfg(feature = "vad-wavekat")]
            {
                evaluate(
                    manifest,
                    neuromita::audio::score::WavekatEarshotScores::new,
                    threshold,
                )?
            }
            #[cfg(not(feature = "vad-wavekat"))]
            {
                return Err("rebuild with --features vad-wavekat".into());
            }
        }
        "silero" => {
            #[cfg(feature = "vad-silero")]
            {
                use neuromita::audio::score::{SileroScores, VerifiedSileroAsset, init_onnx_runtime};

                let model = env::var("NEUROMITA_SILERO_MODEL")?;
                let library = env::var("NEUROMITA_ORT_DLL")?;
                init_onnx_runtime(Path::new(&library))?;
                let asset = VerifiedSileroAsset::open(Path::new(&model))?;
                evaluate(
                    manifest,
                    || SileroScores::from_verified(&asset).expect("verified model"),
                    threshold,
                )?
            }
            #[cfg(not(feature = "vad-silero"))]
            {
                return Err("rebuild with --features vad-silero".into());
            }
        }
        other => return Err(format!("unknown provider {other}").into()),
    };

    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
