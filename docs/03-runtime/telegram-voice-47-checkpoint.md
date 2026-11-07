# Telegram Voice #47 — implementation and verification checkpoint

## Implemented in this branch

- `TelegramVoiceService` implements `VoiceProvider`, serializes bot conversations, enforces a rolling 20-message/minute budget (including commands), speaker selection, polling, per-reply media limits, timeouts and owned WAV results.
- `FfmpegTelegramDecoder` converts MP3 and Telegram voice OGG to stereo PCM16/44.1 kHz WAV using an explicitly provisioned FFmpeg executable, with bounded duration and output.
- `TelethonSession` is a production-intended user-MTProto adapter over an explicit JSON-line Python sidecar (`tools/telegram_sidecar.py`). **It does not use Bot API.**
- The sidecar implements persistent Telethon user sessions, Telegram login code requests, code submission, optional 2FA, readiness, message send/receive, audio filtering and bounded download.
- Rust GUI voice settings have explicit connect, code, 2FA and disconnect actions. A `DynamicTelegramVoice` provider allows the application to activate a user-authorized Telegram TTS session without rebuilding Core.
- A headless executable `telegram-voice-smoke` exercises the full network/auth/synthesis-to-WAV path when the user supplies real credentials. This is **not** a completed live integration test.
- Offline fake transport tests cover stale/foreign message rejection, no-reply failure, speaker command suppression, rolling send limit, missing decoder and typed authorization error mapping.

## External prerequisites

- Python with Telethon installed, plus FFmpeg in PATH or `NEUROMITA_FFMPEG`.
- Telegram `api_id`, `api_hash`, phone number, interactive code and optionally 2FA, and a writable protected data directory for the persistent Telethon session file.
- The GUI currently locates the sidecar in the repository's `tools` directory unless `NEUROMITA_TELEGRAM_SIDECAR` is set. Packaging/installer distribution of the Python runtime and sidecar is not implemented.

## Headless smoke route

Run the compiled `telegram-voice-smoke` binary with arguments:
`<sidecar.py> <text> <speaker> <output.wav>`.

Required environment variables: `NEUROMITA_TG_API_ID`, `NEUROMITA_TG_API_HASH`, `NEUROMITA_TG_PHONE`, `NEUROMITA_TG_SESSION`. On first login, also set `NEUROMITA_TG_CODE` and, if needed, `NEUROMITA_TG_2FA`. These are optional when an authorized session is already on disk. Never commit or print credentials or the session file. The session basename must reside in the user data directory, not the repository.

## Remaining critical review blockers

- No live Telegram auth, bot setup or media decoding test with real credentials. Bot commands and response formats may change.
- No complete multi-character/speaker protocol adaptation for each supported bot, and no proof of `/start`, `/mp3`, `/hd`, `/videonotes` setup compatibility. Only `/speaker` is implemented in the coordinator.
- Telegram incoming voice is received and exposed at transport level but is **not** routed into Core ASR/chat. Issue #47 is not complete until this is wired.
- No durable deduplication checkpoint for incoming messages or backend-owned session rehydration/autoconnect from saved application settings. GUI credentials are currently transient; stored Telethon sessions are persisted separately.
- No secure user-credentials settings store, installer provision of the Telethon sidecar, 2FA modal/prompt-id queue, or clean recovery when the Telegram session expires during generation.
- Polling with message-ID watermarks is not strict correlation to a specific bot response. A very late response to a timed-out request could be misattributed.
- The current GUI integration needs successful feature-`gui` compilation and review of Core routing, turn cancellation, stop/start races and Unity/local playback. A test passing with a fake decoder is not a full audio-route integration test.
- No mock tests of invalid/expired auth request sequence, 2FA retries or GUI prompt closure; classified errors are not equivalent to state-machine coverage.

## Verification policy

`cargo fmt --all -- --check`, `cargo test --locked --offline`, `cargo clippy --locked --offline --all-targets -- -D warnings`, plus GUI feature compilation on Rust 1.92 are run by the branch CI. Track their actual results; the upstream suite includes at least one previously observed failure in `ai_runtime::assets::generation` that is unrelated to Telegram. Never report that all tests pass unless CI confirms it.

This PR remains **Draft** while review blockers above remain. The purpose of this checkpoint is to keep the code and its verification boundary honest.
