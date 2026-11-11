# Binary-pinned native publisher policy (Agent C, 2026-11-09)

## Threat boundary

A publisher's valid Ed25519 release signature is useful only if its public verification key is authentic. Before this change, the portable-root `Config/native-publisher-keys.json` was a syntactically independent file but could be rewritten by another process with the same user permissions. Such a process could replace the key with its own, sign a malicious model manifest, and pass the GUI admission check. `native-model.json` SHA-256 alone cannot establish publisher identity.

The production `neuromita-gui` now uses a **compile-time binary-pinned SHA-256 of the exact publisher-policy file bytes**. This pin must come from the trusted application build process, not from the model installer, network, the portable directory, or a runtime environment variable. At compilation, explicitly set `NEUROMITA_NATIVE_PUBLISHER_POLICY_SHA256` to 64 lowercase hex digits representing SHA-256 of the final, independently reviewed policy file. The Rust `option_env!` value is embedded in the compiled binary. The build does not invent or install keys. **If this value is absent**, Native AI Hub remains available for metadata but cannot authorize any publisher, even if someone later creates a valid-looking local policy and signed release. An invalid compile-time value fails Native workflow initialization rather than enabling unpinned trust.

On each metadata trust observation, selection, and activation, Core checks the bounded policy file bytes against the compiled digest before parsing any keys. A changed/missing policy, different whitespace, extra or removed key, symlink, malformed JSON, forged approval, or unknown publisher cannot be accepted under the older binary pin. After successful policy authentication, the existing Ed25519 release verification still binds publisher, model ID and exact manifest SHA-256; the installer still hashes EXE, model assets and chosen-backend native runtime files; only live IPC `status` may establish Ready.

The policy update process deliberately requires **a new trusted app build** with a new pinned policy digest. This enables an auditable, explicit root-key rotation or revocation in a future app release without silently enrolling keys through models or user-writable configuration. Package metadata and signed approvals remain independently provided. There are no sample trusted publishers in this change.

## Capabilities and limits

This protects the publisher-policy contents against an attacker who can modify **only the portable trust-policy and package files but cannot modify the trusted application executable or its build pipeline**. It does not protect against replacing the application binary, a compromised compiler/build system, process injection or privileged OS compromise. Old authentic binaries may still carry older authorizations; preventing **application downgrade** requires OS-level code-signing and update rollback protection beyond this slice. It does not provide emergency remote revocation, Windows Authenticode verification, HTTPS delivery, source provenance, cross-process lease protection, secure download receipts, or real neural-model inference.

The generic Core `NativeReleaseTrust::new` receives an **explicit optional independent policy pin**. `None` means deny all, not local auto-enrollment. Tests construct pins from their own in-memory synthetic trust fixtures; production obtains the digest exclusively at compile time. No runtime API in the GUI accepts a policy pin from user input or downloaded bytes.

## Verification

Integration tests cover an unauthorized same-user key swap and forged release, altered legitimate policy requiring app update, old-policy rollback relative to a new binary pin, no-pin denial even after local files are provisioned, malformed build pin and already-selected process activation after trust-policy modification. Existing Native integration and GUI compile-only checks should also be run on the exact final commit.
