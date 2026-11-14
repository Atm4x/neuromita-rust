# Telegram Voice #47 — implementation and verification checkpoint

## Implemented in this branch

- `TelegramVoiceService` implements `VoiceProvider`, serializes bot conversations, enforces a rolling 20-message/minute budget (including commands), speaker selection, polling, per-reply media limits, timeouts and owned WAV results.
- `FfmpegTelegramDecoder` converts MP3 and Telegram voice OGG to stereo PCM16/44.1 kHz WAV using an explicitly provisioned FFmpeg executable, with bounded duration and output.
- `TelethonSession` is a production-intended user-MTProto adapter over an explicit JSON-line Python sidecar (`tools/telegram_sidecar.py`). **It does not use Bot API.**
- The sidecar implements persistent Telethon user sessions, Telegram login code requests, code submission, optional 2FA, readiness, message send/receive, audio filtering and bounded download. JSON-RPC responses now echo request IDs; Rust rejects late replies from previous RPCs, limits line size and terminates timed-out sidecar sessions.
- Rust GUI voice settings have explicit connect, code, 2FA and disconnect actions. Expired codes request a fresh code. Startup autoconnect uses an existing Telethon session and explicitly supplied environment credentials. A `DynamicTelegramVoice` provider allows application activation without rebuilding Core and cancels inflight provider operations on disconnect/replacement.
- A headless executable `telegram-voice-smoke` exercises the full network/auth/synthesis-to-WAV path when the user supplies real credentials. This is **not** a completed live integration test.
- Offline tests cover stale/foreign/old reply-target message rejection, no-reply failure, speaker command suppression, rolling send limit, missing decoder, typed authorization error mapping, sidecar request IDs, 2FA/error retries and bounded media via a fake Telethon client. CrazyMita bot requests use one `/voice <text>` message, consistent with the audited Python implementation.

## External prerequisites

- Python with Telethon 1.45.0 (pinned in `tools/telegram-requirements.txt`), plus FFmpeg in PATH or `NEUROMITA_FFMPEG`. Install: `python -m pip install -r tools/telegram-requirements.txt`. Set `NEUROMITA_PYTHON` to that interpreter if needed.
- Telegram `api_id`, `api_hash`, phone number, interactive code and optionally 2FA, and a writable protected data directory for the persistent Telethon session file.
- The GUI locates the sidecar in `tools/telegram_sidecar.py` next to the executable (for staged packages) or in the development checkout, unless `NEUROMITA_TELEGRAM_SIDECAR` is set. The installer still does not bundle Python, Telethon or FFmpeg.
- For opt-in startup autoconnect, turn on Telegram TTS and Telegram Auto Connect, and provide `NEUROMITA_TG_API_ID`, `NEUROMITA_TG_API_HASH`, `NEUROMITA_TG_PHONE` in the process environment. API secrets deliberately are not saved into unencrypted app settings. A protected `telegram-user.session` file is persisted under the app data directory.

## Headless smoke route

Run the compiled `telegram-voice-smoke` binary with arguments:
`<sidecar.py> <text> <speaker> <output.wav>`.

Required environment variables: `NEUROMITA_TG_API_ID`, `NEUROMITA_TG_API_HASH`, `NEUROMITA_TG_PHONE`, `NEUROMITA_TG_SESSION`. On first login, also set `NEUROMITA_TG_CODE` and, if needed, `NEUROMITA_TG_2FA`. These are optional when an authorized session is already on disk. Never commit or print credentials or the session file. The session basename must reside in the user data directory, not the repository.

## Remaining critical review blockers

- No live Telegram auth, bot setup or media decoding test with real credentials. Bot commands and response formats may change.
- No proof of live bot compatibility. `/start` is sent for both audited bots, `/mp3` for Silero, `/speaker <id>` on voice change, and `/voice <text>` as one message for CrazyMita. Stateful `/hd` and `/videonotes` toggles are not enabled without reading their bot replies: blindly issuing toggle commands can invert an already-correct setting. Multi-character voice IDs remain Core-owned.
- Clarified scope: Telegram audio from the configured voice bot is a **TTS response** consumed by `TelegramVoiceService` and returned to Core as `AudioArtifact`. Routing that bot response into ASR/chat as if the player spoke would be incorrect. A separate feature for Telegram-originated human voice input (distinct peer, permissions, character routing and message checkpoint) needs its own contract before implementation.
- No durable checkpoint for Telegram-originated human chat messages (not part of current TTS path). Session rehydration is available when the opt-in environment credentials are provided; GUI credentials stay transient and only Telethon stores the protected user-session credential.
- No secure native GUI credential store, installer provision of the Telethon sidecar, or queued prompt-ID auth modal. The single GUI login flow handles invalid code/password and re-requests expired codes, but session-expiry mid-generation still requires explicit reconnect.
- Polling with message-ID watermarks is not strict correlation to a specific bot response. Replies that explicitly target an older Telegram message ID are now rejected; bots that do not send reply-to metadata can still return a very late response that cannot be attributed reliably.
- The GUI feature build and Core audio route still require a full CI check and live Unity/local playback. Provider replacement/disconnect now cancels in-flight synthesis; network login remains sequential within the GUI runtime command loop.
- Fake Telethon tests now cover invalid/expired code, 2FA retry and unauthorized access. GUI modal dismissal/prompt IDs and disconnect-during-login remain untested/incomplete.

## Verification policy

`cargo fmt --all -- --check`, `cargo test --locked --offline`, `cargo clippy --locked --offline --all-targets -- -D warnings`, plus GUI feature compilation on Rust 1.92 are run by the branch CI. Track their actual results; the upstream suite includes at least one previously observed failure in `ai_runtime::assets::generation` that is unrelated to Telegram. Never report that all tests pass unless CI confirms it.

This PR remains **Draft** while review blockers above remain. The purpose of this checkpoint is to keep the code and its verification boundary honest.
