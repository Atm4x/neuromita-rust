# D21 — verified single-executable activation and recovery

Scope: partial implementation of [Updater #44](https://github.com/Atm4x/neuromita-rust/issues/44). This is a **real, offline application-file replacement operation** for a prepared publisher-digest-verified **raw executable**; it is **not** archive installation or the integrated end-user update workflow.

## Ownership and invariants

- `GithubUpdateCatalog` owns remote release selection; `UpdateStager` owns download, SHA-256/size validation and content-addressed publication under `UserData/updates/staged`.
- `SingleFileActivator` (`src/updater/activation.rs`) is a separate installation-boundary operation. It accepts the exact `UpdateAsset` and its `StagedArtifact`, checks the name/size/hash agreement, rehashes staged bytes, and copies a verified candidate to a **fixed workspace adjacent to the installed executable**.
- Only product-owned Rust executable names `neuromita`, `neuromita-gui` and their `.exe` forms are accepted. It rejects ZIP/7z, traversal, symlinks to the target, a data root enclosing the target, unknown file identities and mismatched release metadata. This is intentionally narrower than the old Python product update's multi-file release archive.
- `prepare` only touches `.neuromita-update/` in the install directory; it cannot overwrite the current executable. It writes a fixed immutable JSON transaction intent and synchronizes file bytes before `apply`. The intent records target basename and **both** old and new SHA-256/size values. No intermediate Rust storage versions or migration chain are introduced.
- `apply` must be executed **by a separate updater process after the target application exits**; replacing one's own `current_exe` is rejected. Renames are performed on one filesystem (target → workspace backup, verified candidate → target). Installation verification uses a streaming SHA-256 pass before removing the backup. It never deletes or overlays `UserData`, settings, models, prompts, logs or arbitrary installation files.
- A preexisting transaction is not silently replaced. A failed mutation is not declared successful. `recover` reconciles the journal with actual target/backup bytes, using **only** independently recorded digests.

## Recovery truth table

| Target | Backup | Result |
|---|---|---|
| Original hash | Missing | Prepared, if candidate is intact; no target mutation |
| Missing | Original hash | Restore original by rename, clean transaction |
| New hash | Original hash | Confirm new bytes; remove old backup and journal |
| New hash | Missing | Confirm new bytes after an interrupted cleanup |
| Original hash | Missing, candidate missing | Original already restored; clean journal |
| Unknown hash | Original hash | **Conflict**, do not overwrite unrecognized target |
| Missing | Missing | **Conflict**, do not invent a recoverable state |
| Any | Corrupted backup | **Error**, preserve evidence, no destructive fallback |

Journal missing/malformed, unknown workspace entries and candidate corruption fail closed. `recover` is idempotent when the transaction has completed and no workspace remains.

## Evidence and deliberate omissions

The unit tests use isolated installation directories and include success, `UserData` preservation, journal preparation/reuse rejection, interruption after backup and after activation, missing candidate, corrupt staged source, corrupt candidate, external modifications to installed bytes, mismatched release identity, path traversal and symlinked executable. All original updater staging tests remain in `src/updater.rs`.

**Still required before claiming issue #44 completion:** distribution packaging/verified extraction for ZIP/7z (no decoder dependency added), a composition-root-to-external-helper handoff and restart coordination, real Windows locked executable and reboot behaviour, parent-process termination, cleanup after a crash during *prepare*, concurrent activation locking, injected I/O failure/power-loss verification, release candidate selection in the GUI, and platform fsync durability proof. The Windows directory sync helper is a no-op; on Windows the implementation is crash-recoverable at the recorded filesystem states but **not certified power-loss-durable**.

The `.neuromita-update` workspace is dedicated to an **owned** installation, not an untrusted directory shared with attacker-controlled processes. The code fails safely for most visible conflicts but cannot guarantee transactional isolation against an attacker concurrently modifying installation paths. No Windows acceptance, UI test or end-to-end release install is claimed.
