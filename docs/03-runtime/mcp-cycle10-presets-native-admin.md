# MCP Cycle 10 — API connection editor and native AI Hub ownership

The MCP server is a peer administrative surface to the GUI. It does not create a second preset catalog, worker, install coordinator, timeline, or Presenter. The main acceptance agent runs Unity/GUI/AI E2E using these tools; local contract checks are not E2E evidence.

## PresetsPresenter — GUI `UpdatePreset`

The `presenter_presets` MCP tool now supports `action=update_connection`. Read `inspect_presenter(surface=presets)` first; it returns the **catalog revision** and full nonsecret editor fields for each preset, never an API key or secret-bearing reference. Submit the preset ID, the `expected_revision`, name, template ID, endpoint, model, temperature, max output tokens, timeout, stream, retry count and both send-flags. A blank template ID explicitly changes the preset to unconfigured. Other fields remain required to avoid ambiguous partial updates.

Changing a configured preset preserves its existing credential reference when the protocol matches. An optional explicit `api_key` is stored through the application's existing credential repository and requires MCP `--allow-content` as well as `--allow-write`; the result never echoes it. The catalog checks the expected revision **inside the same write lock as persistence**, and rejects stale updates. GUI code continues to use the existing update method without an optimistic concurrency requirement; the MCP version has a mandatory CAS fence.

## Native AI Hub — GUI's `AiHubNativeService`

Two MCP tools expose six native GUI commands through the **identical** application-owned `AiHubNativeService` instance injected into the Presenter probe:

- `presenter_native_inventory(refresh=false|true)`: installed revisions, signed publisher-authorized release references, selections, actual service worker phases, and an installation observation. Empty/invalid publisher trust is surfaced as an error flag, not as a trusted release.
- `presenter_native(action=refresh)`: `RefreshNativeModels`.
- `presenter_native(action=select,model_id,manifest_sha256,backend)`: `SelectNativeRevision`.
- `presenter_native(action=activate,model_id,service,revision)`: `ActivateNativeModel`.
- `presenter_native(action=stop,model_id,service,revision)`: `StopNativeModel`.
- `presenter_native(action=install,model_id,manifest_sha256,backend)`: `InstallNativePackage`, using the existing signed-release admission and bounded native download owner.
- `presenter_native(action=cancel_install)`: `CancelNativePackageInstall`.

The download owner now maintains one shared watch snapshot: `download.revision`, `operation_id`, phase (`starting`, `manifest`, `downloading`, `publishing`, `transfer_completed`, `transfer_failed`, `cancel_requested`, `installed`, `failed`), bytes, and verified publication outcome. This snapshot is updated for **GUI-initiated and MCP-initiated** installs alike. To await it, use `presenter_wait(target=native_download,after_revision=<download.revision>,timeout_ms=30000)`; refresh the inventory to get the new revision. No socket polling or additional lifecycle owner is needed.

No native package is downloaded or activated merely by running the local tests. The native service enforces publisher policy, revision hashes, declared service/backend, installation gates and model worker lifecycle. MCP write calls still require `--allow-write`. `--core-debug` is only for explicit direct Core calls, not these ordinary Presenter operations.

## Progress against 68 GUI commands

| GUI domain | Complete | Partial | Missing | Total |
|---|---:|---:|---:|---:|
| Chat / Conversation | 7 | 3 | 1 | 11 |
| Memory / Knowledge | 2 | 0 | 4 | 6 |
| Presets / Characters | 6 | 6 | 1 | 13 |
| Audio / Voice / ASR / TTS | 5 | 0 | 9 | 14 |
| AI Hub / Native | 6 | 0 | 2 | 8 |
| Telegram / Remote Voice | 0 | 0 | 5 | 5 |
| Settings / Launcher | 6 | 2 | 3 | 11 |
| **Total** | **32** | **11** | **25** | **68** |

Complete GUI Command coverage: 32/68 (47.1%); 11/68 partially covered; 25/68 missing. The denominator counts the 68 commands enumerated in GUI runtime, **not** every public Presenter method or Unity-host-only operation. Follow-up parity review must widen this inventory.

### Remaining order

1. `PreviewAiHubInstall`, `ConfirmAiHubInstall`: actual plugin/wheel review and installation owner via the shared `AiHubInstallCoordinator`.
2. Speech model/session control: `SelectSpeechModel`, `SaveAsr`, `SelectAsrRecognizer`, `RefreshSpeechInventory`, `SaveTts`, `SaveCharacterVoice`, `SpeechSessionEnabled`, `ChatSpeechToggle` through the real Presenter and settings owners, not mock inference.
3. Telegram/RemoteVoice action surface, then memory knowledge-graph operations and remaining conversation actions.

## Verification boundary

Rust 1.100.0 offline `cargo check --locked --features mcp --all-targets`, `cargo fmt --package neuromita -- --check`, and `git diff --check` passed locally. New unit/integration tests were added and type-checked, but execution timed out during linking; **do not mark those tests PASS**. Full E2E requires the main agent running actual GUI/Unity, real devices and models.
