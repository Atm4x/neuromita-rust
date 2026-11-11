# Native inference process boundary — first executable contract slice (2026-11-09)

## Requirement pivot

The user's `NEUROMITA_NATIVE_INFERENCE_GOALS_2026-11-08` makes independent Rust model executables the preferred inference boundary. Model implementations, their tensor operations, and backend selection stay in the model EXE. The Core only selects, validates, launches, supervises and transports requests. Optional shared native runtime libraries are passed as a directory path, **not** injected into global PATH. WGPU code can live inside the EXE; host graphics drivers are external. A model **must** remain resident across consecutive inference requests.

The earlier Python production installer and durable journal (#331–#344) remain useful for installed Python plugins. They are **not** repurposed into a pretend native installer or native inference supervisor; an actual native artifact/asset/runtime receipt owner is not implemented yet. `AiInstallOperationPresenter` still reads that journal. It must not be treated as a source of native `Ready` state.

## Scope of this slice

`ai_runtime::native_model::NativeModelProcess` owns one process, child stdin/stdout, correlation, bounded frames and a local runtime lifecycle. The caller explicitly supplies an absolute native executable, a **pinned SHA-256** for that executable, a model directory, a compatible runtime directory, a backend and a timeout. Launch rejects absent, non-regular or symlinked paths, a changed binary hash, unsupported backend claims and malformed protocol replies. It never installs packages, updates PATH, imports Python or loads a DLL into the Core process.

The model is started with `--ipc-stdio --model-dir <...> --runtime-dir <...> --backend <cpu|cuda|wgpu|onnx>`. Protocol v1 uses one bounded JSON object per line, containing a version, request UUID, command and command payload. The reply must correlate with the request ID and protocol version. The supported control commands are `capabilities`, `load`, `warmup`, `status`, `infer`, `unload`, `shutdown`. Audio/large tensor data-plane design is **not** selected here: this bounded JSON control channel is not a universal tensor API. `cancel` of an in-flight inference is not claimed yet.

The process is launched once and retained until an explicit `shutdown`, an error or owner destruction. `load` must acknowledge weights loading, `warmup` must acknowledge warmup, and `status` must confirm the selected model as `ready` before the owner's state can become `Ready`. The `NativeModelPresenter::project` is pure: it translates that **live owner's** snapshot to a read-only model without storing another truth. `Started`, `Loaded`, and `Warm` are explicitly **not** inference-ready; `Ready` may only originate from this process after status confirmation. An IPC timeout, unexpected exit, invalid frame, mismatched correlation or worker error invalidates the session and kills the child. The process is never restarted implicitly and weights are not reloaded after each infer.

This first boundary does not yet implement native model directory discovery, verified artifact installation, model-asset receipts, version negotiation beyond v1, safe OS sandboxing, process-tree cleanup/Job Objects, in-flight cancellation, dynamic compatible CUDA DLL resolution, GPU checks, or application/Slint startup registration. Executable hash verification before spawn does not close all path-swapping TOCTOU races; installer-owned immutable directories and platform-specific handle-based launch verification need further work. Runtime and model directories are checked for symlinks but not yet cryptographically pinned. The child inherits its parent's OS privileges/environment; it is not a sandbox.

## Executable tests and limitations

`tests/native_model_process.rs` compiles a tiny standalone **Rust** worker from `tests/fixtures/native_protocol_worker.rs`, starts it via the same process API, performs load→warmup→status→multiple infer in the **same PID and UUID**, unload/reload, and shutdown. It also injects unsupported backend, executable hash change, symlinked directory, wrong request correlation, worker crash, timeout, and oversized response. This is real OS process/IPC evidence, **not** functional RVC, Fish Speech, ONNX/CUDA, DLL loading or actual waveform inference. No Python is used by the native worker test.

## Next vertical

Implement a minimal **native RVC EXE** in its own repository or package (HuBERT, pitch, synthesizer, index and pinned weights must be validated), then use the above protocol with two real waveform conversions without reloading. Introduce Core discovery and immutable receipts only when real artifact/weights/runtime catalogs are pinned, and join their status to the existing AI Hub projection. Do not replace model-ready with `installed` or fabricate UI progress. Explicit compatibility/versioning and licensing checks are needed for any redistributable CUDA or ONNX Runtime libraries.
