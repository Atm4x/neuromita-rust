# Telegram Voice #47 — implementation checkpoint

This document describes the **unfinished** implementation branch. It is not evidence of a working Telegram integration.

## Implemented and exercised by the fake transport

- `TelegramVoiceService` is a `VoiceProvider`, returns owned `AudioArtifact` in WAV format or an error, never a fake successful silence.
- An async mutex serializes a single bot conversation, including speaker selection, messages, response polling, download, and decoding.
- A fresh per-request watermark rejects older messages; outgoing messages, messages from the wrong peer, and non-audio messages are ignored.
- Speaker commands are not resent when unchanged.
- Minimum request interval, timeout, media download size bounds, and cancellation by dropping the in-flight future are represented in the coordinator.
- Fake tests cover an owned WAV response, stale/foreign media rejection, repeated speaker selection, and a no-reply timeout.

## Critical work still required — do not merge as a TTS implementation

- **No production MTProto adapter** is wired. An authorized Telegram user session (Telethon adapter or a validated Rust MTProto implementation), code/2FA coordinator, persistence, and session lifecycle are missing.
- **No real decoder** is wired for MP3/OGG to WAV. The decoder is an injection boundary; fake tests must not be confused with audio decoding.
- No Core-owned credentials, secure session storage, start/stop status, autoconnect, or settings reconciliation yet.
- The Slint Telegram settings still use preview/disabled actions, with no authorization modal integration.
- `ApplicationConfig` does not yet select this service as the live voice backend. The Core-to-Unity/local-consumer vertical path has **not** been tested.
- The service currently knows only the `/speaker` selection form. Silero startup, `/mp3`, `/hd`, and `/videonotes` require tested capability-specific command handling. External bot behavior is unconfirmed.
- The current watermark strategy is **not a server-side request correlation ID**. A late response to an earlier request can still overtake a new request; a stronger drain/quarantine protocol is needed.
- No rolling 20-messages/minute rate policy, token-aware auth cancellation, start/stop generation guard, explicit queue bounds, 2FA prompts, headless error flows, multi-character routing tests, or incoming voice support.
- Tests are synthetic only. Live Telegram behavior was not tested.

## Architecture constraint

`TelegramTransport` represents a user-authenticated MTProto transport. Replacing it with the Telegram **Bot API** would violate the required authorization model. Keep transport, auth, codec, voice queue, Core settings, and Slint presenter as separate owners.

## Validation

The branch CI workflow runs Cargo fmt, offline tests, and offline Clippy. An incomplete or failing CI run is not a successful validation. No credential is required for the fake transport tests.
