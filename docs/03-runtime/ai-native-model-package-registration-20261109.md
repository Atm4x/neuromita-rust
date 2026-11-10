# Native model directory registration — Agent C, 2026-11-09

## Contract and ownership

`NativeModelRegistry` is **read-only** and scans one explicit absolute package root. Each child directory is a single native model package named by its canonical model ID, with `native-model.json` inside. The registry does not download files, install dependencies, own worker processes, create catalog defaults or keep another mutable state store. The Core native process owner (`NativeModelProcess`) still owns runtime state, and `NativeModelPresenter` remains the pure projection of that live state. The older Python production installer and `AiInstallOperationPresenter` are not modified.

An application can discover model metadata without starting code. To prepare a process launch, it must supply an **externally approved manifest SHA-256**. The registry verifies that the manifest is unchanged since discovery, then streams and checks every referenced package file and required shared runtime file against the exact declared size and SHA-256. Finally it returns an existing `NativeModelLaunch` (which rechecks the executable at `NativeModelProcess::start`). Declared backend and current OS/architecture must match. This is a fail-closed **content admission** boundary, not a native runtime installer or a cryptographic signature verifier.

Only an approved digest from a trusted distribution source or an explicit user approval flow can establish intended provenance. Computing a checksum from an untrusted folder and immediately approving that same checksum provides **no security** against malicious executables; the caller must not do this in production. Manifests and EXEs are not sandboxed. Process launch is not TOCTOU-free, and weights can change again after admission. The native package must be stored in an immutable, controlled directory before its real application admission path is enabled. `Installed` and `ModelReady` are not inferred from discovery, manifest parsing or checksums.

## Version 1 manifest

Location: `<managed-native-root>/<model-id>/native-model.json`.

Fields:

| Field | Meaning |
| --- | --- |
| `schema_version` | Exact integer `1`; unknown versions rejected |
| `model_id` | Stable ASCII ID matching its directory basename |
| `display_name` | User-visible name (metadata, not authority) |
| `package_version` | Explicit package revision string (not `latest`) |
| `os`, `arch` | OS/architecture supported by this concrete artifact |
| `protocol_version` | Exact integer `1`, matching the current native JSONL protocol |
| `backends` | Nonempty, duplicate-free subset of `cpu`, `cuda`, `wgpu`, `onnx`, only those the executable actually supports |
| `executable` | Relative path, exact byte size and 64-character hex SHA-256 |
| `assets` | Nonempty list of model weights/configs/tokenizer/index files, each pinned by size and SHA-256 |
| `runtime_files` | Optional per-backend dependencies in the **separately supplied compatible runtime directory**, each pinned by size and SHA-256 |

For `executable` and `assets`, the file reference object is `{ "path": "relative/path", "size_bytes": 123, "sha256": "..." }`. Each `runtime_files` entry wraps `{ "backends": ["cuda"], "file": <file-reference> }`; only dependencies of the selected backend are required. A single runtime file can be declared for multiple supported backends. No absolute paths, parent navigation, empty path segments or symbolic links are permitted. Assets and executable are rooted under the package directory; runtime files are rooted under the explicit shared runtime directory. All paths are validated as regular files, and the whole managed path chain must be free of symlinks. File SHA-256 is computed with bounded streaming IO, not by loading model weights into memory. Manifest is capped at 128 KiB, package count at 128 and referenced file count at 128; each referenced file must have a declared nonzero size. Unknown fields are rejected. Names are sorted for deterministic display.

A package may be listed on another OS, but `prepare_launch` returns `Incompatible` there. The package listing is not a promise that CUDA or ONNX works on that machine. NVIDIA `nvcuda.dll` remains driver-owned, not an app-distributed runtime. WGPU driver support requires separate runtime probing. Some model-specific native libraries may not be redistributable and need explicit license checks before distribution.

## Verified scenarios

An isolated Rust 1.98.1 vendored/offline harness compiles the exact `native_registry.rs`/`native_model.rs` modules and tests a native Rust fixture worker through the **same** process owner. Tests include real two-request same-PID IPC, changed weight, changed shared runtime, stale manifest, wrong backend, unapproved digest, path traversal, duplicate paths, mismatched directory ID, invalid schema and symlinked asset/registry entry. The `weights.bin` test fixture has no RVC neural network weights and makes no audio. Its success is **not** RVC/Fish Speech inference parity.

## Remaining acceptance criteria

- Acquire the actual RVC source + authentic HuBERT, F0 and Synthesizer weights (and applicable index), with reproducible license/source/cryptographic checksums. No such model was accessible in this cycle.
- Implement a separate real native RVC executable, prove `load → warmup → two waveform conversions` with one PID, verify output audio and performance.
- Bind this package registration to a **trusted** approval/installation authority, durable signed or otherwise authenticated receipts, immutable publication and revision leases, without repurposing Python installation receipts.
- Prove platform ABI/runtime DLL loading and process-tree cleanup on Windows; verify CUDA/WGPU/ONNX choices on real hardware. Add appropriate audio data-plane framing only after an actual model establishes requirements.
- Connect an application-owned native supervisor to the existing AI Hub presenter. Do not manufacture ready state or progress percentages.
