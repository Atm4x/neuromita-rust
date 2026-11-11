# Native publisher trust in the AI Hub (Agent C, 2026-11-09)

## What changed

The Native AI Hub no longer treats a SHA-256 from a discovered `native-model.json` as a publisher approval. The actual GUI command uses `NativeModelWorkflow::select_trusted`, which resolves the exact digest against an independently provisioned trust policy and a signed release document. `NativeModelWorkflow::activate_selected` re-checks the current publisher approval **even for a selection written before the trust policy existed**. It does this before full artifact hashing and again against the exact controller selection before starting the worker. A new authorization does not auto-start the EXE. A live worker's last IPC phase is still distinct from publisher approval and file-integrity checks.

Low-level `NativeModelWorkflow::select` / `NativeRevisionStore::select` remain for internal trusted test/API callers; desktop GUI no longer calls those methods. `NativeModelWorkflow::with_trust` is mandatory in the desktop runtime. Without provisioned keys/approvals, the Native tab displays revisions as untrusted and disallows Select/Start; there is no TOFU, implicit key enrollment, or fallback to another model/backend.

## Out-of-band provisioning contract

At the application portable root (`directory` passed to `DesktopRuntime`), a user or administrator must **independently** provision the publisher trust file:

`Config/native-publisher-keys.json`

```json
{
  "schema_version": 1,
  "publishers": [
    {
      "publisher_id": "publisher-id",
      "ed25519_public_key_base64": "<32-byte Ed25519 PUBLIC KEY, canonical padded base64>"
    }
  ]
}
```

The placeholder is illustrative, **not** a usable key. No key is shipped by this PR, generated implicitly, fetched from a model, or accepted because a manifest contains a matching hash. Keys must be obtained and verified through a separate authoritative channel. The application currently reads this file; it does not provide an enrollment UI, keys rotation mechanism or privileged ACL provisioner. Changing/removing keys on disk takes effect on the next selection, refresh or activation. Do not grant an untrusted model installer write access to this policy file.

Signed release documents may be copied from any transport, including an untrusted one, into:

`AIData/NativeApprovals/<model-id>/<manifest-sha256>.json`

Each document is the `SignedNativeRelease` schema from `native_approval.rs`, with a 4 KiB size bound, and is Ed25519-verified against the **currently pinned** independent public key. Its signed message exactly covers `publisher_id`, `model_id` and the lowercase manifest digest with `neuromita.native-release.v1` domain separation. Folder/file names are lookup hints only: the verified contents must match the requested model ID and digest. Missing files, invalid signatures, unknown keys, symlinks, non-canonical base64 or altered identity block admission. Only small policy and approval JSON are read for inventory refresh; file-size and integrity verification is still done during actual selection/activation.

Trust metadata is a read-only observation, never a replacement for the selected-revision store, installer receipts, live worker status or source of truth for model lists. Revoking a key prevents **new activation** but does not forcibly kill a previously started worker, whose PID remains owned by the Core session until stopped.

## Verification and remaining security boundary

Test cases cover no preprovisioned key, valid independently signed approval, wrong key, swapped digest/model, malformed documents and symlinks, and key/approval revocation after an older selection. Native GUI compilation and full native integration regression are separate checks; neither is an interactive Windows E2E.

**Threat-model limit:** a writable local policy file is not an OS-level identity/permission boundary. A hostile same-user process able to replace `Config/native-publisher-keys.json` can enroll its own key. Production delivery therefore still needs OS-protected/app-signed trust-anchor distribution (or an explicit authenticated administrator enrollment flow), signature-key rotation/revocation policy, installer receipt with publisher evidence, verified HTTPS download and Windows acceptance. This PR does not claim any of those are finished.

The independent model agent still owns all RVC/Fish Speech/HuBERT/RMVPE/Synthesizer math and native EXE implementation. This change only gates Neuromita's application-owned native workflow.
