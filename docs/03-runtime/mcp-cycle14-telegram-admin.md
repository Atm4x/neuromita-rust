# Telegram administration via GUI and MCP

## Ownership and operations

`TelegramAdmin` is a single application-owned MTProto session authority. Desktop GUI and the Core LocalControl PresenterProbe share the same `Arc<TelegramAdmin>` and the same `DynamicTelegramVoice` provider used for TTS. Unlike earlier GUI-local `telegram_session`, a disconnect, new authorization, or `SaveTts` refresh from either interface immediately affects the same provider.

MCP tool `presenter_telegram` supports `connect` (API ID, API hash, phone), `code`, `password`, `disconnect`, and `refresh_voice`. `inspect_presenter(surface="telegram_auth")` shows a sanitized `{revision, phase, status, authorized, voice_active}` snapshot; `presenter_wait(target="telegram_auth",after_revision=N,timeout_ms=10000)` observes a revision change. Neither API hash nor telephone number, login code, 2FA password, account identity or session tokens are sent back to MCP. GUI phase values: 0 disconnected/error, 1 code needed, 2 password needed, 3 authorized.

`SpeechSaveTts` and GUI `SaveTts` both refresh the active Telegram voice provider on the same owner after saving settings. A disconnected account cannot activate a voice provider.

## Access policy

`disconnect` and `refresh_voice` require MCP `--allow-write`. Authentication actions require **both** `--allow-write` and `--allow-content` in the MCP process, and `NEUROMITA_ENABLE_LOCAL_TELEGRAM_AUTH=1` in the Core/GUI process. This is an explicit **trusted-machine-only** opt-in because the existing local TCP control socket does not authenticate callers or encrypt secrets. Do not enable it on a shared/untrusted desktop. For stronger deployment security, add authenticated OS-user-bound IPC before making Telegram credential entry available by default. GUI's in-process auth is unaffected by this opt-in gate.

## Core-only evidence and remaining host acceptance

`cargo check --locked --offline --features mcp --all-targets` checks Core/MCP implementation. The GUI compiler requires the Slint native toolchain and system fontconfig development environment. Targeted test covers sanitized inspection, revisioned disconnect/refresh, wait and rejection of disabled password submission (no sidecar). It **does not** log into Telegram, send messages or test remote TTS playback. The main agent must run connected GUI-side acceptance with Telethon and actual credentials.

## GUI Command parity

This adds one shared owner for TelegramConnect, TelegramCode, TelegramPassword, TelegramDisconnect, and refreshes the formerly partial SaveTts behavior. `RemoteVoiceAction` (Fish Audio catalog and preview) remains GUI-only; it must be moved to the application-owned Presenter to avoid parallel remote-catalog state. No separate MCP worker should be introduced.
