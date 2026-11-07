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
