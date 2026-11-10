# C3 — interrupted uv and asset publication recovery (2026-11-08)

## Scope

Production Core already journals the selected operation and revalidates published receipts. This change completes the missing **owned asset staging cleanup** after an abnormal process exit, and proves the existing layer/uv staging cleanup with a real killed installer parent. It does not introduce a second installer or a second receipt catalog.

`ModelAssetStore::publish` now serializes writers of one immutable model revision using the existing `RevisionLease::exclusive`. Under that lease, it rechecks the installed receipt before any new transfer. It scans the plugin's staging directory and reclaims only `.staging-<UUID>` directories with an existing, regular, empty sibling `.lock` marker whose OS lock can be obtained. Active owners are retained; unfamiliar/unowned directories, symlinks and invalid markers are not deleted. On Unix it additionally checks that the opened lock and path metadata identify the same inode. The new staging owner holds the lock for the entire transfer. Normal failures/cancellation drop its guard and remove the partial directory; abnormal process exits release its OS lock so the next installation can reclaim it. Successful publication atomically renames the validated stage into the immutable revision and disarms cleanup.

No previously published revision is removed during recovery. The authoritative asset receipt and SHA-256 content check still determine whether a revision is usable. The journal's `verified` event is **not** sufficient proof. A corrupt published revision fails closed and is retained for explicit repair rather than silently overwritten.

## Executable recovery evidence

`tests/ai_asset_crash_recovery.rs` performs two distinct OS process-kill scenarios on Linux:

1. A child runs the actual `ModelAssetStore::publish` on a 256-MiB pinned local asset. The parent observes a partially written staging file and kills the child mid-transfer. A reopened `InstallOperation` retains the original UUID and `Started` state and refuses completion. A second publish removes the abandoned staging only, validates the installed receipt and then advances the journal. A currently locked foreign stage and an unowned directory survive cleanup. A later publish reclaims the foreign stage only after its lock is released. Corruption of the published weights is rejected without deletion.
2. A child runs the real `OverlayInstaller`/`LayerInstaller` command path with an executable mock uv program. The parent kills the Rust process after uv starts. The surviving uv child inherits staging ownership; an unrelated layer publish does not remove its staging. After that child exits, a repeat install reclaims the orphan stage, re-executes uv, verifies actual distribution metadata and the overlay receipt, and completes the reopened operation with its original UUID. Corrupting the published overlay receipt subsequently fails closed; another immutable revision remains untouched.

These are **real OS process, staging, package metadata and journal tests**, not full AI-model inference tests. The uv binary is a local programmable fixture, not a downloaded official uv. The asset source is local, not an interrupted HTTPS transport. Production `AiProductionInstaller::prepare` reuses these same underlying installers and receipt checks, but this cycle does not certify a killed full multi-service production preparation with live inference.

## Boundaries

- Not an atomic multi-service installation transaction. Published layers may be shared and must not be rolled back on failure of a later step.
- Locking is advisory across cooperating Core processes. It cannot sandbox arbitrary Python install-plan hooks or defend against a hostile process actively rewriting disk content between verified checks.
- Cross-platform code uses standard Rust file locks. The explicit process-kill scenarios currently execute on Unix/Linux; Windows power-loss, handle/job semantics, and HTTPS transfer interruption need separate native verification.
- No automatic cancellation of orphan uv subprocesses on Linux after a hard parent crash; their inherited stage lock prevents unsafe reclaim until they exit. No resumable HTTP range download: the next attempt starts a new verified stage.
- No claim of model readiness from stage receipts or installed wheels. A real pinned CPU ASR inference and GPU/Triton JIT smoke remain distinct acceptance gates.
