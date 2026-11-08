# Remote voice presets — Rust implementation (#27)

The desktop API speech backend uses the Fish Audio cloud provider. Remote presets are independent from the generation API presets and local TTS plugin settings.

## User workflow

- Select **API**, add an independent profile, edit its name, Fish Audio API key, model, speed and default voice ID.
- Enter a voice ID for a specific Mita to override that profile's default. If the field is blank the default voice is used. Stable `CharacterId`, not a display name, is routed through production generation.
- Save, switch between profiles, and remove profiles (the final profile cannot be removed).
- Preview an individual character's voice through the same HTTP synthesizer and FFplay used by the desktop app. Clicking Cancel aborts the in-flight request or playback. The PCM payload is packaged into a WAV **in memory** and no temporary audio file is created.
- Production uses the active profile **only** when API TTS is enabled; Telegram/Local retain the existing runtime's voice provider.

## Configuration and credentials

The dedicated `<data-dir>/remote_voice.json` file contains version 1, a stable active preset ID and up to 32 full presets. Writes use a temporary file and rename, and newly created files are owner-only on Unix.

**Security limitation:** API keys are saved as **plaintext** in this separate file; they are not written to AppSettings or its export, but no OS keychain/encryption is yet integrated. On Windows file access follows the containing directory's ACL. Protect the profile directory, do not sync the JSON into public repositories or logs. A future keychain migration is required for stronger at-rest protection.

The repository's documented legacy Python remote-voice JSON v1/v2/v3 format is **not** yet automatically imported by this slice. This version must not claim migration acceptance.

## Transport and error contract

The Fish Audio client POSTs to `https://api.fish.audio/v1/tts` with Bearer authentication, model header, and JSON for PCM16 44100 Hz. Strict input validation rejects missing/non-32-hex voice IDs, invalid URLs, bad model, speed and empty key before the request. Responses distinguish auth, balance, missing voice, rate limit, 422 and other HTTP status, timeout, invalid audio and cancellation.

Network operations are asynchronous with bounded memory (maximum response 64 MiB), no redirects, 15-second connection timeout, an overall request timeout, and explicit preview cancellation. Each production request observes the latest saved profile; no foreign voice is used when a nonempty override is invalid.

## Verification and limitations

Unit tests cover catalogue persistence, profile isolation, URL/PCM validation, and a local mocked Fish Audio HTTP response plus authorization failure. Neither live Fish Audio credentials nor an external HTTP request are required for CI.

The end-to-end Slint desktop/FFplay GUI workflow needs native validation. Other provider templates are not implemented. Volume adjustment in cloud preview is not yet wired. Production audio delivery depends on the existing application's audio output path and application voice gates. User confirmation of real Fish Audio subscription / model compatibility remains required.
