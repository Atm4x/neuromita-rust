use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    game_link::{GameBeatConfig, GameLinkConfig},
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator, gemini::{GeminiTemplate, GenerationSettings},
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}},
    time::timeout,
};
use uuid::Uuid;

struct NoGeneration;
impl GenerationProvider for NoGeneration {
    fn kind(&self) -> ProviderKind { ProviderKind::Gemini }
    fn generate(
        &self,
        _context: neuromita::generation::GenerationInput,
        _template: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async { Err(GenerationError::Transport("generation not used".into())) })
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("beat-game-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn wav_fixture() -> Vec<u8> {
    let sr = 16_000_u32;
    let frames = sr * 4;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&sr.to_le_bytes());
    out.extend_from_slice(&(sr * 2).to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(frames * 2).to_le_bytes());
    for n in 0..frames {
        let value: i16 = if n % (sr / 2) < sr / 100 && n > 0 { 25_000 } else { 0 };
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}
async fn send(writer: &mut OwnedWriteHalf, value: Value) {
    writer.write_all(value.to_string().as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
}
async fn receive(reader: &mut BufReader<OwnedReadHalf>) -> Value {
    timeout(Duration::from_secs(15), async {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }).await.unwrap()
}
fn config(root: &Temp) -> ApplicationConfig {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "test-model", "test-key",
            GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
        ).unwrap()
    ));
    let generator = ResponseGenerator::new([Arc::new(NoGeneration) as Arc<dyn GenerationProvider>]).unwrap();
    let mut config = ApplicationConfig::minimal(
        preset, generator,
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(8).unwrap(),
            character_commands: NonZeroUsize::new(8).unwrap(),
            presenter_updates: NonZeroUsize::new(8).unwrap(),
            retained_finished: NonZeroUsize::new(8).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.game_link = Some(GameLinkConfig::new("127.0.0.1:0".parse().unwrap()));
    config.game_beats = Some(GameBeatConfig {
        media_root: root.0.join("music"),
        cache_root: root.0.join("cache"),
        enabled: true,
    });
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn game_link_really_delivers_beats_and_blocks_out_of_root_media() {
    let root = Temp::new();
    std::fs::create_dir_all(root.0.join("music")).unwrap();
    std::fs::write(root.0.join("music").join("song.wav"), wav_fixture()).unwrap();
    std::fs::write(root.0.join("secret.wav"), wav_fixture()).unwrap();
    let app = ApplicationLifecycle::start(config(&root)).await.unwrap();
    let stream = TcpStream::connect(app.game_link().unwrap().address()).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    send(&mut write, json!({"command":"hello","command_id":Uuid::new_v4()})).await;
    assert_eq!(receive(&mut reader).await["type"], "hello_ack");
    let request_id = Uuid::new_v4();
    send(&mut write, json!({
        "command":"get_music_beats","command_id":Uuid::new_v4(),
        "request_id": request_id,"audio_path":"song.wav","track_name":"song",
        "stream": false,"min_confidence":0.0,"backend":"dsp"
    })).await;
    assert_eq!(receive(&mut reader).await["type"], "music_beats_accepted");
    assert_eq!(receive(&mut reader).await["type"], "music_beats_job_started");
    let result = receive(&mut reader).await;
    assert_eq!(result["type"], "music_beats_ready", "{result}");
    assert_eq!(result["body"]["request_id"], request_id.to_string());
    assert!(result["body"]["beats_count"].as_u64().unwrap() >= 5);
    assert_eq!(result["body"]["method"], "dsp_energy_onset_v1");

    send(&mut write, json!({
        "command":"get_music_beats","command_id":Uuid::new_v4(),
        "request_id":Uuid::new_v4(),"audio_path":"../secret.wav",
        "backend":"dsp"
    })).await;
    let denied = receive(&mut reader).await;
    assert_eq!(denied["type"], "error");
    assert_eq!(denied["code"], "music_source_denied");
    drop(write);
    app.shutdown().await.unwrap();
}
