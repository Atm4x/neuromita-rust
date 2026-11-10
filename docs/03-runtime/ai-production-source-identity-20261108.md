# C3 — preflight source identity and process-kill journal proof (2026-11-08)

## Production boundary

`AiProductionInstaller::prepare` now captures a bounded SHA-256 identity of every selected Python plugin's regular files, along with the managed Python executable and pinned uv executable, **before** executing the first selected dynamic `install_plan`. This source digest is incorporated into the existing whole-selection operation fingerprint. A source change across restart cannot accidentally reuse an unfinished operation UUID for different source bytes.

After each dynamic plan/preflight and before **and after** every backend/overlay/asset installation stage, the same immutable source identity is rechecked. Source changes fail closed before a stage is acknowledged `verified`; only authoritative backend/overlay/model receipts permit that acknowledgement. No new secondary receipt database or installer is introduced. Multiple services share the same captured identity for this operation.

The scanner hashes regular-file contents and relative paths in sorted order, rejects plugin-directory symlinks/special entries and excessive recursion, and uses conservative limits: at most 1024 files, 1024 directories, depth 24, 64 MiB per file, 128 MiB total including Python/uv binaries. These bounds deliberately reject oversized or nonstandard plugin layouts instead of silently omitting files. Hashes are not logged as raw source text: the derived fingerprint remains the only journal identity. Source paths and plugin code never enter the journal payload.

## Kill/restart test

`tests/ai_install_operation.rs` now launches a **real child test process**, waits for its first durable verified stage receipt and for the second stage to enter `started`, forcibly kills the process, then reopens the same journal from a fresh file descriptor. The test checks that the UUID persists, the first stage is still `verified`, the interrupted second stage is `started`, the final stage is pending, and `finish` cannot fabricate success. The test is an operation-journal process-kill proof; it **does not** kill a real uv installer, download, asset hook, GUI, or inference worker.

`tests/ai_production_installer.rs` includes a source-drift test where a dynamically executed Python install-plan writes an additional file under its own plugin directory. Whole-selection preflight must reject the resulting mutation before publishing any backend or overlay. A standalone source-identity test detects changed plugin bytes and uv executable content; symlink/oversize admission tests fail closed.

## Deliberate limits / further work

- This is **not** an atomic transaction across uv execution, download staging and multiple shared immutable layers. A failed step may leave a valid independently shareable immutable layer; no unsafe rollback/deletion is performed.
- Boundary rechecks reduce race windows but **do not** freeze an arbitrary Python process, imported modules outside the selected plugin directories, CPython shared libraries, installed package trees or the entire OS. A concurrent adversarial rewrite between check and execution remains a TOCTOU risk. Custom Python install-plan code executes unsandboxed.
- Operation journal ID/stages are not yet exposed through a Core-owned read model for AI Hub. No live model-ready claim is added.
- Real kill-during-uv, model download recovery, concurrent malicious source mutation, Windows process/job kill and one actual pinned CPU ASR inference remain unverified. Triton GPU/JIT has a separate acceptance gate.

The test harness and production compiler checks must be reported separately from executable integration and end-to-end verification.
