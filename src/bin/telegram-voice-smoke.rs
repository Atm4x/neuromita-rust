//! Opt-in headless Telegram voice smoke path (requires Telegram account credentials).
//!
//! Required env: NEUROMITA_TG_API_ID, NEUROMITA_TG_API_HASH,
//! NEUROMITA_TG_PHONE, NEUROMITA_TG_SESSION.
//! On a new session: NEUROMITA_TG_CODE and optionally NEUROMITA_TG_2FA.
//! Usage: telegram-voice-smoke <sidecar.py> <text> <speaker> <output.wav>
//! Optional: NEUROMITA_PYTHON and NEUROMITA_FFMPEG.
use std::{env, path::Path, sync::Arc, time::Duration};
use neuromita::{
    telegram_sidecar::TelethonSession,
    telegram_voice::{FfmpegTelegramDecoder, TelegramConfig, TelegramVoiceService},
};

fn required(key: &str) -> Result<String, String> {
    env::var(key).map_err(|_| format!("missing required variable {key}"))
}
async fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let sidecar = args.next().ok_or("missing sidecar.py path")?;
    let text = args.next().ok_or("missing voice text")?;
    let speaker = args.next().ok_or("missing speaker")?;
    let output = args.next().ok_or("missing output.wav path")?;
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let python = env::var("NEUROMITA_PYTHON").unwrap_or_else(|_| "python3".into());
    let ffmpeg = env::var("NEUROMITA_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let session = TelethonSession::spawn(Path::new(&python), Path::new(&sidecar))
        .map_err(|error| error.to_string())?;
    let id = required("NEUROMITA_TG_API_ID")?.parse::<u32>()
        .map_err(|_| "invalid NEUROMITA_TG_API_ID")?;
    let authorized = session.connect(
        id,
        &required("NEUROMITA_TG_API_HASH")?,
        &required("NEUROMITA_TG_PHONE")?,
        Path::new(&required("NEUROMITA_TG_SESSION")?),
    ).await.map_err(|error| error.to_string())?;
    if !authorized {
        session.request_code().await.map_err(|error| error.to_string())?;
        let code = required("NEUROMITA_TG_CODE")?;
        let needs_password = session.sign_in_code(&code).await
            .map_err(|error| error.to_string())?;
        if needs_password {
            session.sign_in_password(&required("NEUROMITA_TG_2FA")?).await
                .map_err(|error| error.to_string())?;
        }
    }
    let service = TelegramVoiceService::new(
        session.clone(),
        Arc::new(FfmpegTelegramDecoder::new(ffmpeg)),
        TelegramConfig {
            peer: env::var("NEUROMITA_TG_BOT")
                .unwrap_or_else(|_| "@silero_voice_bot".into()),
            wait: Duration::from_secs(60),
            interval: Duration::from_secs(2),
            poll: Duration::from_millis(330),
            max_media_bytes: 32 * 1024 * 1024,
        },
    ).map_err(|error| error.to_string())?;
    let audio = service.speak(&text, &speaker).await
        .map_err(|error| error.to_string())?;
    tokio::fs::write(output, &audio.bytes).await
        .map_err(|error| error.to_string())?;
    session.disconnect().await.map_err(|error| error.to_string())?;
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("telegram voice error: {error}");
        std::process::exit(1);
    }
}
