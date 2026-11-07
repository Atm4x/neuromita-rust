# D22 — verified updater handoff after application shutdown

`neuromita-updater` is an independent Rust executable built from `src/bin/neuromita-updater.rs`. Distributors **must install it beside** `neuromita-gui[.exe]` (or `neuromita[.exe]`); no runtime extraction, exe self-copy, arbitrary target/URL CLI flags, or fallback to a profile directory is implemented.

The application obtains an `UpdateAsset` from the existing release catalogue and a matching `StagedArtifact` from `UpdateStager`. `SingleFileActivator::prepare(&asset, &artifact)` validates the exact release-asset name, length, and SHA-256 before touching the installation. The caller can then invoke `handoff::schedule_after_shutdown(&activator)` **only from that actual installed target process** and must perform its normal graceful shutdown. The library does not automatically wire the placeholder launcher "Check updates" action or implement distribution package extraction.

The handoff writes a one-time `handoff.json` (parent PID, process-start identity, random nonce and product-owned target name) into the existing `.neuromita-update` workspace, then launches the fixed sibling `neuromita-updater` process. The helper accepts only the internal command, numeric PID and start identity, and UUID nonce; it reads and validates the on-disk intent against the same transaction journal. It cannot choose arbitrary paths, files or URLs through arguments. It verifies the parent identity and **waits until that particular process exits**, using Linux `/proc/<pid>/stat` start time or a Windows process handle with creation time and `WaitForSingleObject`. It never terminates processes. Unsupported platforms fail closed.

After the parent exits, the helper removes its handoff marker, acquires the install-scoped OS file lock via the activation API, calls atomic `recover → apply → verify` via `apply_or_resume`, and restarts the GUI only after byte verification succeeds. CLI targets are activated but not automatically launched because they may need user-supplied arguments. An atomic `UserData/updates/last-activation.json` records the result. All activation, preparation and recovery operations share the installation lock; the lock file remains at `.neuromita-update.lock` so file identity is stable and OS lock ownership automatically expires after process exit. Candidate permission bits are copied from the previously installed executable and synchronized, preserving Linux executable mode. Unknown conflicting installation bytes are never overwritten.

## Safety boundaries and incomplete acceptance

- Only the standard installation-owned `UserData` root is supported by this helper. A custom `--data-dir` is explicitly rejected; the helper never silently recreates a missing `UserData` directory. The installed helper itself is **not** included in the updated artifact: distribution packaging must ship it ahead of time.
- A raw, verified, single product-owned executable is required. Release ZIP/7z extraction, full multi-file installs, release publisher code-signing/authenticity beyond the verified GitHub metadata and HTTPS trust, and selection in the graphical update action are **not implemented**. A release ZIP is *not* equivalent to a verified executable.
- Linux unit tests exercise identity matching, lock contention, malformed journals, Unix mode preservation, bounded paths, prepared / interrupted states and status writing. Windows file-lock, process-wait, permissions and GUI restart require actual Windows CI / E2E verification.
- Power-loss durability is **not** established on Windows (directory sync remains a no-op). The helper crash between successful activation and restart can leave a correct new executable without auto-restarting; restarting manually remains possible, and the next normal activation recovery is fail-closed. A crash before marker consumption leaves a deliberate fail-closed handoff marker requiring controlled recovery, not silent deletion.
- The `.neuromita-update.lock` protects cooperating application updater operations, not arbitrary modifications by a malicious actor with write access to the installation. `UserData` remains untouched by installation-file replacement except for the explicit status JSON.

**Issue #44 remains OPEN.** This is an independently testable helper lifecycle slice, not complete Python updater parity or a Windows installation certificate.

## D23: read-only graphical release check

The Home launcher menu's **Check Updates** action now performs a real asynchronous
`GithubUpdateCatalog::latest_release()` query against the existing Python/Unity
release repository `Atm4x/NeuroMita`. It reports the **published** tag and
presence of exact `PythonBuild-{tag}.zip` / `UnityBuild-{tag}.zip` asset names
without claiming that those archives are installed, newer than the local
installation, or directly applicable by the Rust single-file updater.
Concurrent requests are rejected while a check is in flight; the result remains
visible on Home and the network request never runs on the Slint event thread.
Failure is reported as failure, not an empty list of updates. Archive bytes are
**not downloaded, staged or applied** by this check.

The current Rust single-file activator accepts a verified raw executable that
matches the installed target. The historical release distribution provides
ZIP packages for distinct Python and Unity components instead. No GUI path
relabels a ZIP as an executable or launches the helper for these archives.
The core/Unity installed-version comparison, publisher trust and full ZIP
installer/rollback are still required for user-visible update eligibility.
Until those exist, the action is a release inspection, not an install button.

## D24: verified archive download (no automatic install)

The Home release inspector now exposes explicit Python/Unity ZIP download actions
only for archive names matching the verified published release tag exactly.
Each request is scoped to the current published release metadata and is handled
by `UpdateStager` with its existing size and SHA-256 checks, content-addressed
storage, cancellation and no-overwrite publication. The GUI displays staging
progress and result without blocking the Slint event thread. Download does
not unpack, install, restart the app, or invoke the single-file helper.

A pinned D24 archive-layout investigation of PythonBuild-v2026.11.01.zip
verified the publisher SHA-256 and found 2,442 Deflate entries (75,870,045
uncompressed bytes). It contains top-level `Settings` and `Prompts` directories:
a naive overlay extract would risk overwriting user content. The real Python ZIP
is not an eligible Rust binary; conversion to a safe multi-file transaction
requires explicit content ownership, ZIP parser/extractor review, and validated
rollback. These obligations remain open for #44.

## D26: crash-recoverable Python entry-file transaction (not ZIP installation)

`updater::python_core_activation::PythonCoreActivator` is a separate, narrowly
scoped transaction for **both existing** `Launcher.exe` and `NeuroMita.pyz`.
It does not update any `Settings`, `Prompts`, `UserData`, secondary library,
asset, or Unity file. The trusted caller must first enumerate and validate the
publisher-verified ZIP through `preflight_python_archive`, extract the two
regular files with a separately reviewed decoder, and supply independently
verified exact SHA-256 digests for the extracted bytes. This is an explicit
*future input contract*: the repository still has **no approved ZIP extraction
implementation**. Hashes handed in by an untrusted caller do not establish
publisher authenticity. **Neither the GUI nor `neuromita-updater` currently
calls this API**, so this work does not make Python ZIPs installable.

`prepare` validates both files and existing installation bytes, copies both
candidates into a dedicated workspace, preserves the old target permissions,
and durably writes a bounded `pending.json` journal. The shared install-wide
`.neuromita-update.lock` serializes operations with the raw Rust-executable
updater. `apply` is intended for a separate process after the old launcher has
stopped; it renames both old files to workspace backups before installing the
new bytes, verifies the resulting pair, then atomically promotes the journal
to `committed.json`. No switch to the new pair is considered committed before
both new file hashes match. `recover` on a fresh activator instance handles
crashes between *either* rename: an uncommitted transaction reverts **both**
files or refuses on conflicting unknown bytes; a committed transaction keeps
the verified new pair and finishes deleting backups. Unjournaled backups are
never automatically deleted. The currently installed file and its backup must
be regular files, and unexpected workspace entries abort before mutation.

This is **crash-recovery behavior under cooperative, idle filesystem access**,
not a global atomic filesystem transaction observable by concurrent readers.
The caller must guarantee no concurrent Python launcher is running; Windows
locked-file behavior and directory durability during power loss have not been
verified. Existing targets are required; creating/removing new installation
files and directories remains out of scope. Full 2,442-entry Python ZIP update,
protected user overrides, actual launch-helper handoff for Python, trusted ZIP
extraction, Windows installation E2E and any Unity installer require further
separately tested work. Issue #44 remains OPEN.
