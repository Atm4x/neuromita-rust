# Native HTTPS package transfer — application integration boundary

Agent C implements transport and publication only. Independent model agents own the inference executable, weights, GPU/runtime requirements and real audio correctness. This slice does not use Python, pip or uv and does not run downloaded code during installation.

## Authorization and source ownership

`NativeHttpsPackageTransfer::download_and_install` requires `NativeReleaseTrust`, the exact model ID and raw manifest SHA-256, a selected `NativeBackend`, a separately chosen `NativeHttpsOrigin`, an application-owned existing staging directory and a cancellation token. The Core trust service validates the Ed25519 release against the application-pinned publisher policy **before the first network request** and **again immediately before local publication**. The downloaded files and their server cannot introduce trusted keys. An unsigned manifest digest does not authorize a native EXE.

The caller must configure the HTTPS origin independently of the package. The manifest contains only relative file paths and exact SHA-256/length references; it never contains an executable download URL or an arbitrary redirect target. An origin is an HTTPS base URL ending in `/`, without credentials, query or fragment. The layout is:

- `<base>/packages/<model-id>/native-model.json`
- `<base>/packages/<model-id>/<executable-or-weight-relative-path>`
- `<base>/runtime/<selected-backend-runtime-relative-path>`

URLs are assembled as path segments from validated identifiers. Requests require HTTP 200, reject redirects (including cross-host and downgrade redirects), request identity encoding and enforce per-file deadlines, exact content length when provided, exact streamed byte count, SHA-256 and a total 256 GiB bound. The manifest itself is limited to 128 KiB and must match the publisher-signed digest before file URLs are used. Backends, host OS, architecture and manifest structure are checked before fetching EXE/assets. Unselected backend runtime files are never downloaded.

## Publication and cancellation

Downloads go into an application-supplied private staging root under a fresh UUID directory. The stage is disjoint from the managed model and shared-runtime roots, and is not a discoverable model. Failure, cancellation, bad network response or hash mismatch removes this owned stage; unrelated entries are not cleaned up. File bytes are synced and existing native `NativeRevisionStore::install_verified_local` performs its own complete hash verification before publishing. That installer owns locks, shared-runtime conflict policy, immutable revision directories and crash-recovery semantics; there is no second mutable catalog or package-readiness authority. The worker becomes Ready only after process IPC load/warmup/status, never after download.

Cancellation is honored until the synchronous, atomic local publication starts. Once entered, the installer finishes or fails as a unit even if an awaiting task is dropped. The staging owner is moved into that publication task to prevent early cleanup while files are still being read. An unexpected OS crash during download may leave an inert UUID directory under the caller's staging root; automatic orphan reclamation of this separate download staging is not implemented.

## Security limits and follow-up

TLS gives transport confidentiality and server authentication via the operating system's trust store; a publisher signature + exact hashes establishes release identity independently. **This code does not pin TLS certificate/SPKI, enforce DNS/IP allowlists, protect against DNS rebinding or compromise of the caller-selected source configuration**. The calling app must restrict eligible origins. Validation of private IP addresses/SSRF protection is not claimed. Transport and local installer still have a path-TOCTOU risk against a hostile process that can mutate the same-user staging directory while downloading. Do not use a public-writable staging root. No downloader button, publisher receipt persistence, resume/range requests, bandwidth throttling, multi-mirror policy, real Windows native package launch, CUDA DLL proof, or actual neural inference in this slice.

Network tests use a local plain-HTTP fixture **only through a private unit-test entrypoint**, never through the public HTTPS origin constructor. This tests streaming, redirects, size/digest verification and native publication but does not constitute external HTTPS/TLS E2E. Production requires actual trusted HTTPS endpoint configuration and real package acceptance testing.
