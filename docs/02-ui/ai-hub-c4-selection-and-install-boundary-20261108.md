# C4 AI Hub: refresh-safe model details and installation boundary

## Verified selection contract

A model is selected by one temporary UI identity, `(category, id)`, never by its position in a filtered list. On every replacement of `AiHubPage.entries`, the page requests the unique index from the active Slint presenter snapshot. No persistent UI catalog or additional Core model owner is introduced.

| Model in new snapshot | Draft | Detail behavior |
| --- | --- | --- |
| Same identity, unchanged data | Clean | Refresh selected metadata and form defaults |
| Same identity, changed data | Clean | Refresh metadata and defaults, preserving identity |
| Same identity, changed data | Dirty | Preserve user's fields and previous detail metadata; warn that catalog changed |
| Identity absent or ambiguous | Clean | Close stale details without writing anything |
| Identity absent or ambiguous | Dirty | Preserve draft, show stale selection warning and explicit discard controls; saving remains unavailable |

The catalog projection refuses to choose arbitrarily between duplicate `(category,id)` entries: **all** rows for a duplicated identity are withheld, the category count is adjusted, and the aggregate warning count increases. Identical IDs in distinct TTS/ASR categories remain independent. The warning count does not reveal plugin paths, raw errors or secrets.

The detail editor is still read-only with demo fields; this guard prevents wrong-model operations once Core-backed schemas and installation actions are connected, but does not itself implement persistence. No UI selection is proof of a model's installation or runtime readiness.

## Installation remains a separate end-to-end acceptance gate

The Rust Core already contains `AiProductionInstaller`, `PluginInstallHandler` and validated dynamic `PluginInstallPlan`, but this C4 presenter does not currently expose or invoke installation. An `SpeechInventory` entry is not an install plan. Inventing a backend, Python version, revision or asset sources to enable a button would violate the Core contract.

Before enabling Install, bind a single owning runtime operation that first resolves the exact selected `(category,id)` against the latest catalog, obtains a **validated plan for the user's effective settings**, and provides an explicit preflight summary (pinned Python/SDK, exact backend and package revision, required assets, permissions, write root and any required approvals). The user must approve that exact immutable plan. Once admitted, Core owns the operation ID, cancellation/safe lifetime, staged writes, progress and terminal outcome. Slint receives typed snapshots and never installs, deletes or claims completion by itself. Refresh or category changes must not redirect an in-flight operation to another model. Destructive removal stays disabled until C3 cleanup/lifecycle acceptance is complete.

Independent Draft PR #262 already contains speculative preflight/dialog/queue surfaces, but also keeps mutations disabled. It has a competing `ui/app.slint` and AI Hub implementation. Integrate a single presenter contract deliberately rather than merging both UI trees or silently activating demo controls.

## Verification

- Rust 1.98.1 and vendored/offline library tests for catalog duplicates, cross-service identities, runtime phase gating and manifest refresh.
- Production GUI Slint typecheck (when run) validates selection callback linkage and typed identity.
- Real Slint QA renders and exercises the clean/dirty refresh cases separately; a compiled UI alone does **not** prove interactions.
- Windows/macOS, complete production interaction E2E and actual installation remain independent, unverified criteria for issue #37.
