# TTS/ASR parity continuation — remote voice backends

Based on Python `src/handlers/remote_voice/fish_audio.py`, `src/presets/remote_voice_templates.py`, `src/handlers/telegram_handler.py` and Rust `VoiceProvider`/`SettingsOwner` contracts. This changes neither LLM presets nor their API.

## Closed at the code-contract level

- [x] Runtime voice routing among local, Fish Audio API and Telegram, using one immutable request-scoped `VoiceBackend` selection rather than reading mutable GUI state midway through generation.
- [x] Fish Audio API transport: canonical HTTPS endpoint, selected model, character voice ID, speed, JSON-to-PCM contract, streaming 100 MiB limit, PCM16 mono WAV result, no redirects, 45-second timeout, typed failures without leaking credentials.
- [x] API secret committed from Slint separately from `AppSettings`; atomic file replace with 0600 Unix file permissions, optional `NEUROMITA_FISH_AUDIO_API_KEY` environment override.
- [x] Telegram synthesized audio path against an **existing authorized** Telethon session: constrained bot username, serialized requests, replies with audio newer than the sent message, bounded base64 IPC, FFmpeg PCM16 WAV conversion, cancellation when worker is dropped.
- [x] Embedding Telegram bridge source in the Rust executable; no dependency installation from GUI or runtime adapter.

## Not yet closed

- [ ] Telegram's initial authorization and 2FA in Slint (requires real session contract). Pre-authorized session via `NEUROMITA_TELEGRAM_SESSION` or `AIData/telegram/speech.session` is required.
- [ ] Automatic discovery/installation of a Python executable containing Telethon. `NEUROMITA_TELEGRAM_PYTHON` must point to an existing absolute interpreter, and `NM_TELEGRAM_API_ID` / `NM_TELEGRAM_API_HASH` must be set.
- [ ] GUI voice preview/test dispatch and playback for both remote backends (UI's preview control is currently disabled).
- [ ] Remote voice profile CRUD from the original Python `RemoteVoiceRepository`. Existing UI can select provider/model/speed/default and per-character voice ID, but does not offer multiple saved remote profiles.
- [ ] End-to-end tests against mock Telegram/HTTP services, and real account/network inference verification.
- [ ] Game-owner ASR routing, message delivery idempotency, external speech suppression, Python-equivalent ASR deduplication, speech chain mock E2E, hardware configuration constraints.

Secrets must never enter diagnostic events, logs, `AppSettings`, LLM presets, or plugin metadata.
