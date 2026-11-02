# Handoff: Rust core contracts

## Start here

This repository root is the standalone Cargo project. Enter it before running Cargo so .cargo/config.toml replaces crates.io with vendor/:

```sh
cd NeuroMita-Rust
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo fmt --all -- --check
```

Rust 1.89+ is required by the locked dependency set (uuid requires 1.89). Verified on Rust 1.99.1 / Windows MSVC. Native TLS dependency aws-lc-sys needs a working native build environment; vendor supplies crate sources, not the Rust/C toolchains. Linux web-agent compilation has not been run on this Windows host.

Tokio has full features. All locked crates, including platform-specific sources and licenses, are in vendor/. Do not normalize line endings there: Cargo verifies checksums. .gitattributes preserves bytes. target/ is intentionally excluded.

## Architecture

Read [minimal-contracts](future/minimal-contracts.md) and [checklist](future/implementation-checklist.md). These supersede limitations of the historical first-slice.md.

- Static ChatApi/SettingsApi/CharacterApi backed by a replaceable runtime slot: one running Core at a time, with a fully completed shutdown required before the next start. Old presenters and handles remain bound to their original Core. No service locator and no silent fallback presets.
- One owned Character per CharacterHandler mailbox; immutable definition and mutable state. CharacterRegistry exposes handles.
- MessageHandler owns sequential execution per character; different characters run concurrently. No LLM await under global locks.
- GenerationPreset contains typed ProviderTemplate. Gemini wire/URL/auth are inside generation/gemini, not in common application/presenter code.
- Gemini authentication matches the original Python google_gemini_default: URL query parameter key, not x-goog-api-key. Keep URL encoding and redaction; never log the authenticated request URL.
- reqwest explicitly enables json, rustls, http2 and system-proxy. Reuse the provider's Client; Windows/macOS system proxy and proxy environment variables are supported. Do not hardcode a machine's proxy address. System proxy changes require rebuilding the Client.
- Generation returns ProposedResponse with effects. Request lifecycle and internal acceptance phase are separate. Cancellation competes atomically with entry into Accepting; effects precede Finished and the next request.
- Independent GuiPresenter exposes commands and bindable settings; frontend bindings only translate input/output. No Slint dependency yet.
- ServerLifecycle owns TCP clients and routes commands to Api. It is generic across client implementations.

## Useful files

| Area | Files |
|---|---|
| Composition/lifecycle | src/app.rs, src/lifecycle.rs |
| Static entry points | src/api.rs, src/settings/mod.rs |
| Requests and observation | src/request/ |
| Characters/context | src/character/, src/timeline.rs |
| Provider port and Gemini | src/generation/ |
| Voice port | src/voice.rs |
| Settings/storage | src/settings/ |
| Migration/transfer | src/migration.rs, src/settings/migration.rs, src/settings/transfer.rs, src/settings_cli.rs |
| Presenter/binding | src/presentation/ |
| Network | src/server.rs |
| Console | src/main.rs, src/terminal.rs |
| Contract scenarios | tests/contracts.rs, tests/vertical_slice.rs |

Concurrent application starts are rejected; sequential restarts are supported once the preceding lifecycle finishes. Tests that use the static API in the same process must still coordinate concurrent starts. Local tests do not need credentials or external network access.

## Live provider status

Live Gemini passed on 2026-11-01 using the explicitly selected Main preset / gemini-3.6-flash, read by tests/live_gemini.rs: answer «Готово.» in 1.99 seconds. HTTP_PROXY, HTTPS_PROXY, ALL_PROXY and NO_PROXY were cleared in the test process; reqwest automatically used the Windows system proxy. No credential is in the repository.

The previous TCP timeout came from disabling reqwest defaults without re-enabling system-proxy. Python HTTPX already read the Windows registry proxy through trust_env/getproxies. The corrected Rust build explicitly enables system-proxy and http2. A separate keyless GET to the same API host negotiated HTTP/2 (the root path returned the expected 404).

To opt into another live run, provide NEUROMITA_LIVE_GEMINI_PRESET_FILE and NEUROMITA_LIVE_GEMINI_PRESET_ID for an explicitly selected local JSON fixture, then run:

```sh
cargo test --locked --offline --test live_gemini -- --ignored --nocapture
```

The fixture is a legacy test-data source only. Production code uses typed presets and does not discover/import/rotate a catalog.

## Remaining work / deliberate limits

- Real TTS engine, Slint/TUI frontend, tools and multimodal/streamed token handling are not implemented; ports and settings contracts are present.
- Timeline supports in-memory and bundled SQLite, branch-prefix reads and atomic message/effect commits. Full request crash recovery remains incomplete.
- RequestTrace is bounded memory diagnostics. No durable DB trace or cross-restart request recovery.
- Discovery can lag, and network receipts have a bounded per-connection lifetime. No reconnect-wide deduplication/authentication/compatibility with the old Unity wire protocol.
- Typed state, working-state and memory effects are committed with the reply. Semantic retrieval and external action executors remain incomplete.

Keep working toward the user's contracts rather than copying Python architecture. Do not fix or refactor Python/Unity files as part of this Rust task.

## Standalone repository

Extracted on 2026-11-01 from [Atm4x/NeuroMita at 7f6b2625d355b1e4e22ed078283df9cfac2468ca](https://github.com/Atm4x/NeuroMita/tree/7f6b2625d355b1e4e22ed078283df9cfac2468ca/audit-docs/rust), formerly branch codex/rust-core-contracts. This is a fresh Git repository on main; the original repository and its history remain intact. The user will create the remote; none is configured here.

src/, tests/, Cargo.toml, Cargo.lock, .cargo/ and vendor/ were copied unchanged. Architecture documents are in future/; the original Python functional audit is in docs/. Local Markdown links were adjusted; Python source references link to the pinned original commit. Historical delivery notes in the audit/checklist refer to the original repository.

The source snapshot passed nine local tests, Clippy with -D warnings and rustfmt. Live Gemini passed without proxy environment overrides. Cargo.lock and vendor include 179 packages, including HTTP/2 and Windows/macOS proxy dependencies. No credentials, build archives, old Git metadata or target/ outputs were transferred.

After extraction, a clean offline build in the new directory passed all nine local tests, Clippy with -D warnings and rustfmt. Compared 11,087 code/config/vendor files against the source snapshot: no differences. All 1,743 local Markdown links resolve. Build output is generated only under the ignored target/ directory.

## Architecture audit fixes, 2026-11-02

Read future/architecture-fixes-20261102.md. CharacterHandler accepts private-field TurnProposal from turn.rs, not StructuredResponse. Rules are in character/state.rs, memory in memory.rs, read projections in character/projection.rs. Existing ContextAnchor/MitaView use one prefix read per complete context.

TTS runs in Working. Cancellation/reset/shutdown before acceptance discards text and effects. Voice failure can produce accepted text with VoiceGenerationFailed. Frontend animations and delivery/execution acknowledgments remain unfinished.

Default sqlite-system uses vendored rusqlite/bundled SQLite; external sqlite3.lib and hand-written FFI are gone. Persistent bootstrap restores Timeline; --no-default-features supports explicit transient configurations and rejects persistent Timeline. Test and run Clippy in both modes.

Settings migration/import/export: read future/settings-migrations.md. Migration<Input> is a minimal conversion port; owners determine startup order and commit. JSON startup and transfer share RustSettingsMigration (v1 -> v2); current-schema fields are mandatory. SettingsApi::prepare_import is pure; import(expected_revision, SettingsTransfer) performs CAS through the current owner, advances its own revision and publishes ordinary bindings. Presenter uses its original owner. Offline settings CLI preserves envelope revision and atomically replaces a file for a stopped app. Python migration explicitly returns NotImplemented; no real Python settings are read or imported. No new dependencies were added.
