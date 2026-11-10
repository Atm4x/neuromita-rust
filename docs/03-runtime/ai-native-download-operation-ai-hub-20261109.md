# Native download operation and AI Hub boundary

Agent C — integration only. The independent model agent owns EXE/model inference; this component never implements neural operations.

## Data ownership

`NativeHttpsPackageTransfer` remains the sole byte-transfer and integrity-checking implementation. `NativeDownloadOperation` owns one asynchronous transfer task, UUID, cancellation token, and typed progress/completion events. The Core-owned immutable `NativeRevisionStore` is the only publication authority. UI state exists only for rendering the current operation and never becomes a second persistent model catalog.

The desktop UI accepts a *lookup tuple* (model ID, exact signed-manifest SHA-256, backend). It never accepts a runtime URL or trusts a digest because the user typed it. The **HTTPS origin** is available only from the application's compile-time `NEUROMITA_NATIVE_PACKAGE_ORIGIN`, validated by `NativeHttpsOrigin`. Independent issuer keys and a whole-policy pin must also be embedded/provisioned via the previously implemented `NEUROMITA_NATIVE_PUBLISHER_POLICY_SHA256` contract; in the absence of either build-time value, the Download action is disabled. No publisher key or default origin is fabricated. The application checks `NativeReleaseTrust::verify_hex` before it creates any native storage directories or starts an HTTP request. The installer revalidates the approval before atomic publication.

Managed roots: `AIData/NativeModels/revisions`, `AIData/NativeRuntime`; disposable downloads: `AIData/NativeDownloads`. The latter is separate from either managed root. Filesystem directory access remains subject to OS access rights; this slice doesn't claim same-user attacker race-free sandboxing.

## State and cancellation

`NativeTransferProgress` exposes `Manifest`, `Downloading`, `Publishing`, bytes received and optional planned bytes. The Core generates observations at file boundaries and at most about four intermediate observations per second; failed or canceled downloads do not emit a spurious `Publishing` state. `NativeDownloadPresenter` caps the transfer progress at **99%**, even when all payload bytes have arrived: a complete transfer is **not** a successfully published package, a trusted worker or an inference-ready model.

A single native operation may be active in the desktop runtime. Every completion event carries its immutable UUID; other events are discarded. The user may cancel the transfer while waiting for headers or bytes. Cancellation just before the synchronous installer begins is checked once more within that task; after atomic publication begins, it is allowed to complete. Shutting down the desktop runtime cancels and **joins** the download before releasing the owner's state. Refreshing the inventory after a successful publication reuses the existing workflow if it has a resident worker, so a running PID is never implicitly switched.

## Verification

- Unit tests cover the exact progress sequence for a signed fixture, byte totals, absence of `Publishing` on cancellation, unapproved operation preflight, operation UUID identity and presenter percent bounds.
- Existing native approval, revisions, controller, process, package installer and registry tests remain unchanged.
- Slint UI is compile-only checked; no claim of Windows interactive E2E, real HTTPS/TLS public server or real RVC/Fish Speech waveform inference.

## Explicit remaining work

A production release must provision and safeguard its *actual* issuer policy, build-time policy digest and approved HTTPS origin. Native download crash recovery, external TLS/redirect acceptance, per-release disk quota/OS sandbox, signed receipts, Windows installation acceptance, and genuine worker inference all require separate verification. Do not infer package or worker readiness from the UI progress bar.
