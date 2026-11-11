# C1 — verified installed Python SDK publication

The Rust executable embeds the exact Python SDK API v1 source files. `ManagedRuntimeBootstrap::prepare` first verifies pinned uv and managed CPython and then publishes the embedded SDK to `AIData/sdk/releases/sha256-<content digest>/neuromita_ai_sdk/`. There is no runtime copy from an installed Git checkout. Each release carries `receipt.json` with the SDK API version, content-addressed revision and per-file SHA-256. The executable recomputes and verifies every pinned `.py` file before using the release.

A temporary revision directory is populated before atomic promotion to an immutable final revision; an `active.json` temporary file is atomically renamed only **after** the complete revision passes verification. A stable per-data-root publisher OS file lock serializes cooperating publishers. A crashed publisher may leave a `.staging-*` directory but cannot make it active. Already-running Python hosts continue to use their original immutable `sdk_root` revision during upgrade. An invalid active receipt or modified host module fails closed rather than silently overwriting it. A new executable's SDK revision must be explicitly published before `resolve_active` can use it.

`InstalledSpeechEnvironment::for_desktop` resolves only a checked, currently installed SDK. Missing, incompatible or corrupt receipts never cause a fallback to `python/`. `AiProductionInstaller` accepts the managed `AIData/sdk` selection or the exact verified release path, and its runtime host always receives the verified release. External configurations using source paths are not treated as production SDK installations.

## Acceptance

- Offline Rust library suite; SDK tests cover initial publication, repeated publication, corrupt files, corrupt receipts, interrupted staging, previous immutable revision survival and update/rollback to executable-pinned revision.
- `ai_managed_plugin_sdk_live` is opt-in. Linux and Windows CI install genuine uv-managed CPython 3.12.10, publish the embedded SDK, discover a community TTS and builtin ASR and execute typed plan/probe/model initialization/inference through the **installed SDK release**, not the source tree.
- This is a content pin to the Rust executable's embedded bytes. It is not an external signature or supply-chain guarantee for a separately downloaded publisher archive. Existing source-based integration tests that explicitly configure `sdk_root` are developer test fixtures; production entry points do not silently select them.

The C1 SDK contract does not certify production model weights, plugin source signatures, or OS-level hostile-code sandboxing. Physical component removal is a separate C3 gate and remains unavailable.
