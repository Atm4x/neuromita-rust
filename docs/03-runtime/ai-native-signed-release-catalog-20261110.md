# Native signed release catalog — Agent C (2026-11-10)

## Scope and boundaries

Agent C integrates independently built native worker EXEs. This change does **not** implement RVC, Fish Speech, HuBERT, RMVPE, synthesizers, F0 or neural inference. It does not create a second writable model catalog, import arbitrary Internet listings, or alter Python/uv installs.

Native AI Hub previously required users to type both a model identifier and a 64-character manifest digest. The new UI enumerates **only** pre-provisioned signed approvals, from `AIData/NativeApprovals/<model-id>/<manifest-sha256>.json`, checked with the exact independently **binary-pinned** publisher policy `Config/native-publisher-keys.json` and the Ed25519 verifier. A digest discovered from a local manifest or typed by a user is never an authorization.

## Core contract

`NativeReleaseTrust::available_releases()` returns a sorted collection of `NativeAuthorizedRelease { model_id, manifest_sha256, publisher_id }` after loading the exact pinned key policy and validating **each** signed approval. All directory and file components must be regular directories/files, not symlinks. Paths must be absolute, identities restricted, lowercase digest file names canonical, signing identities must match parent/name and signature must verify. Limits are 128 model directories, 256 signed releases total, 4 KiB per signed document and 16 KiB for the pinned key policy. Malformed and noncanonical entries fail closed, not silently omitted.

The collection conveys only publisher authorization of a manifest digest. It provides **no** package display name, remote OS/backend availability, price, asset integrity, installation receipt, load status or Ready state. Those facts are not included in the publisher approval schema. The installer remains the source of file-integrity verification; installed metadata comes from `NativeRevisionStore::overview()`, and live Ready from the running worker's IPC. There is no auto-selection, implicit version fallback or unverified remote catalog.

`NativeReleaseCatalogPresenter::project()` joins the verified approvals to current installed revisions strictly by `(model_id, manifest_sha256)`. A signed release can be shown as not installed without downloading its manifest or hashing model weights. The UI displays publisher, model ID and digest, with optional display name/version from an already installed package. A present local manifest is labelled "На диске" rather than proving verified weights. Every available uninstalled row has a Download action; user selects a backend independently, and the existing download operation validates compatibility before installation. An unavailable backend is a strict installation error, never an automatic fallback.

## Application ownership

Desktop runtime loads the catalog on startup, explicit refresh and successful package publication. It sends a typed read-only snapshot to Slint. The Native panel replaces manual model-ID and SHA entry with signer-verified release rows. The selected row supplies `(model ID, SHA)` to the **existing** `NativeDownloadOperation`, which revalidates current publisher authorization before the first request. HTTPS origin and publisher policy pin are compile-time app configuration; no URL from an approval or manifest is accepted.

When no independently pinned policy exists, no releases are deemed authorized and Download remains disabled. Invalid approval/policy or installed metadata produces an error rather than untrusted choices. The release view does not change an already running process or imply readiness.

## Trust and delivery limitations

- Approval documents must currently be provisioned by a separate trusted mechanism; this change does **not** create a signed, remotely retrieved release index or a publisher-listing service. It does not introduce any official publisher keys.
- `NativeHttpsPackageTransfer` currently fetches a model manifest at its existing model-specific route. A listed older signed digest may not be served by that route; the operation will then fail manifest SHA verification rather than load another version. Versioned remote artifact routing requires a future contract with the publisher.
- Local same-user filesystem races remain outside the strict sandbox guarantee; symlink checks are defense-in-depth. An attacker replacing the trusted application binary or its build artifacts can bypass its policy pin.
- No real external TLS, Windows interactive GUI or neural waveform acceptance is proven by local synthetic fixtures.

## Verification targets

- `cargo test --locked --offline --test native_approval`: signed/uninstalled metadata, forged signature, filename/identity mismatches, mutated policy, symlinks and bounds.
- `cargo test --locked --offline --lib native_release_catalog`: exact identity-to-installed join, never a false installed status.
- `cargo check --locked --offline --lib` and `cargo check --locked --offline --features gui --bin neuromita-gui`, Rust 1.98.1.
- Existing native IPC, registry, revisions, installer, trust and transfer regression tests must remain green.
