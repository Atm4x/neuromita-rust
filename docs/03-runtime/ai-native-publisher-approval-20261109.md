# Native publisher approval — Agent C, 2026-11-09

## Scope

Agent C integrates already-built independent model EXEs. Neural model algorithms, weights, CUDA kernels, and their model-specific inference tests belong to the model-development agent. This checkpoint adds a **cryptographically verifiable publisher authorization primitive**; it does not ship or silently enroll a publisher key.

The existing native registry can compute a SHA-256 digest from a local `native-model.json`. **That digest alone is not an approval**. Anyone who can place arbitrary files in a local model directory can also compute a matching digest. The new `NativePublisherVerifier` accepts a release document only when its Ed25519 signature validates under a public key independently pinned by the application/user trust policy.

## Wire contract, version 1

The complete `SignedNativeRelease` document is strict JSON with the following required fields:

- `schema_version`: integer `1`;
- `publisher_id`: ASCII identifier, 1–64 characters, letters/digits/underscore/hyphen;
- `model_id`: ASCII identifier, 1–128 characters, letters/digits/underscore/hyphen;
- `manifest_sha256`: **64 lowercase hexadecimal** SHA-256 characters representing the exact raw bytes of `native-model.json`;
- `signature_base64`: canonical padded RFC 4648 base64 of a 64-byte Ed25519 signature.

No unknown fields, duplicate fields, oversized documents (more than 4096 bytes), untrusted embedded public keys, default schema versions, or noncanonical digests are accepted. One digest binds an entire existing native manifest (including OS, architecture, protocol, executable, assets and backend-scoped runtime dependencies).

The external signer signs the exact UTF-8 bytes of four LF-terminated lines:

```text
neuromita.native-release.v1
<publisher_id>
<model_id>
<manifest_sha256>
```

Each line ends with a line-feed character (0x0A), including the final digest line. Identifiers cannot contain LF. A signature for one model, digest, or publisher identity cannot authorize another. The Ed25519 public key is 32 bytes and must come from an **independently trusted anchor**, not the downloaded release JSON, `native-model.json`, or mutable model directory.

## Core ownership

`NativePublisherVerifier::new` takes explicit `NativePublisherKey` anchors from its caller. A missing or unknown anchor fails closed; it never silently imports keys, trusts the first seen key or downgrades to a bare local digest. Verification yields a `VerifiedNativeRelease` whose fields are private to the verifier module and cannot be constructed by an ordinary external caller.

`NativeRevisionStore::install_verified_local` consumes the verified approval and reuses the existing local native package installer. It still checks the manifest and every selected package/runtime file's SHA-256 and length before atomic publication. `NativeModelWorkflow::select_verified` uses the same opaque approval when making an explicit durable choice. Neither successful signature checking nor an installed package represents a **loaded/ready model**: that requires actual IPC `load → warmup → status` from the independently running EXE.

The earlier `install_local` and `select` methods remain for explicitly preapproved lower-level integration/tests, which use an external digest parameter. Their existence does **not** imply that the current AI Hub GUI is provenance-safe: its existing digest-based selection path must be switched to the verified approval path after an application trust anchor and signed release-delivery source are explicitly configured. Do not treat this checkpoint as end-to-end trusted distribution.

## Explicit limits and remaining tasks

- Release key enrollment must be explicit and independent of the model download; the repository contains no production publisher keys.
- Publisher expiration/revocation, key rotation, offline signed index updates, signed release root-of-trust provisioning, HTTPS/resume and durable download receipts are not implemented here.
- A signature witnesses publisher authorization for one manifest digest, not the publisher's identity in the real world, safety of executing arbitrary native code, race-free executable launch, OS sandboxing, or Windows DLL/CUDA compatibility.
- No GUI controls are introduced in this cycle. A future UI flow should display publisher trust identity and verified provenance distinctly from package-file verification and live worker readiness, and must refuse unsigned packages without auto-approving their locally calculated digests.
