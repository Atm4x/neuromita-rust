# Agent C — Publisher-signed remote native release index v1

## Responsibility

This Core protocol discovers **independently implemented native model executables**. It does not implement neural libraries, infer audio, enroll publisher keys, or silently choose any installed/native worker revision. The AI Hub continues to read the Core-owned `NativeReleaseTrust::available_releases()` approval catalog; the remote index is an **untrusted transport for independently signed approval documents**, not a second mutable catalog.

## HTTPS endpoints

An independently configured `NativeHttpsOrigin` (HTTPS only, no credentials, fragments, URL-encoded path or redirects) resolves these immutable paths:

- `indexes/<publisher_id>/index.json` — bounded, publisher-signed index;
- `packages/<model_id>/<manifest_sha256>/native-model.json` — byte-exact manifest;
- `packages/<model_id>/<manifest_sha256>/<asset_relative_path>` — manifest-listed EXE and assets;
- `runtime/<runtime_file_sha256>/<runtime_relative_path>` — content-addressed shared runtime binary.

All digest path components are 64 lowercase hex characters. Relative artifact paths are constrained by the existing manifest validator. The downloader still hashes the manifest and every file, then delegates publication to the preexisting immutable Core installer. No redirect or fallback to an unversioned path is attempted. A server must retain older immutable revisions for old signed approvals to remain downloadable.

## Signed index wire format

A `SignedNativeReleaseIndex` JSON v1 contains only `schema_version`, `publisher_id`, `generation`, `expires_unix_secs`, `releases`, and `signature_base64`. Each `releases[]` value is a complete **separately Ed25519-signed** `SignedNativeRelease` v1 for the *same* publisher. No URL, key enrollment, arbitrary display metadata, backend choice, or executable command comes from the index.

The *outer* Ed25519 signature covers UTF-8 bytes of the following exact domain-separated LF-delimited preimage:

```text
neuromita.native-index.v1
<publisher_id>
<generation>
<expires_unix_secs>
<release_count>
<model_id_1>
<manifest_sha256_1>
<release_signature_base64_1>
...
```

(Each line is terminated by one actual LF byte, including the last entry.) Generation and Unix expiry are unsigned decimal integers without leading zeros as emitted by Rust formatting. All identifiers and digests are constrained ASCII. Each nested `SignedNativeRelease` uses its *existing independent* `neuromita.native-release.v1` signing preimage. The order of release entries is included in the index signature. Invalid signatures, unknown publishers, duplicate `(model_id,digest)`, publisher mismatch, expired documents and ambiguous encodings fail closed. Max index payload: 256 KiB, max releases: 256, max expiry horizon: 180 days. Verification uses the binary-pinned application publisher key policy; no trust-on-first-use.

## Monotonic acceptance / crash behavior

`NativeReleaseIndexStore` owns a separate existing app-private directory for publisher receipts. A `ManagedLock` serializes acceptance per store; a receipt records publisher, latest accepted generation and SHA-256 of the *raw index bytes*. Lower generation is rejected; same generation is idempotent **only with identical bytes**, otherwise rejected as equivocation. Older accepted signed approvals are **not deleted** just because a new index omits them. Receipt is persisted atomically, with fsync, **after** each individually signed approval has been published to `AIData/NativeApprovals/<model_id>/<digest>.json`. A crash before the receipt may expose a subset of independently valid approvals; it cannot produce an unsigned approved release or a receipt claiming files that have not yet been written. An interrupted staging file is ignored by approval enumeration and cannot grant trust. No installed revisions or resident workers are touched.

**Explicit limitations:** Anti-rollback depends on durable and externally protected local receipts. Deleting/replacing receipt, replacing an old application binary, changing the system clock, or compromising the signed application/publisher key are out of scope. Absence from a newer index does **not** revoke an old signed approval: full revocation needs a separately signed revocation/epoch policy, a new app trust build or a contract extension. Same-user filesystem TOCTOU and directory ownership remain OS-level risks. No claimed real external TLS/Windows acceptance, no live publisher server, no model inference evidence. The HTTP loopback tests are deliberately restricted to the private test-only URL path; production API admits HTTPS origins only.

## Tests and rollout

Unit coverage verifies nested and outer signatures, publisher identity, expiry/size bounds, duplicate releases, no TOFU, generation rollback/equivocation, canceled refresh, symlink safety, HTTP redirects and streamed loopback import. Existing HTTPS installer fixtures now use digest-addressed package/runtime routes; these tests provide no external TLS proof. Production AI Hub does **not yet invoke remote refresh**: a future application workflow should explicitly trigger it from a separately pinned origin with progress, cancellation and a verified receipt, then refresh the existing Core catalog. Keep issue #37 open until Windows native-model execution and real audio inference are proven by the separate model agent.
