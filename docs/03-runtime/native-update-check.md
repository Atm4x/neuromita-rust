# Read-only native Rust application update check

The Home launcher action **Check updates** now checks the **Rust application**, not the
historical Python launcher or external Unity game. The source is the `latest` GitHub
release of `Atm4x/neuromita-rust`; the existing `GithubUpdateCatalog` validates HTTPS
metadata, publication state, bounded metadata length, unique asset names and publisher
SHA-256 digests. An HTTP 404 for `latest` means **no published native release**, not
"the installed version is up to date". Other failures are reported as errors.

The currently installed version is the Cargo application version. A remote release
is considered comparable only when the tag is an exact `vMAJOR.MINOR.PATCH` or
`MAJOR.MINOR.PATCH` with non-negative, non-overflowing decimal components and no
pre-release/build suffix. No arbitrary string comparison or fallback to Python/Unity
release IDs takes place. A potential update requires a publisher-digested, exact-match
**raw executable** asset for the current supported OS/architecture:

- Windows x86_64: `neuromita-gui-windows-x86_64.exe`
- Linux x86_64: `neuromita-gui-linux-x86_64`
- Linux aarch64: `neuromita-gui-linux-aarch64`

The naming scheme is an **explicit compatibility contract for future releases**;
this document does not claim any such binaries are already published. ZIP/7z/RAR,
PythonBuild, UnityBuild, unrecognized architectures, unverifiable tags, missing
publisher digests and mismatched filenames are not silently treated as installable.

A background, 18-second-bounded read-only check is independent of AI Core state.
Duplicate requests are rejected while in flight. The existing GUI notice reports
not-published, current, older, candidate asset, unsupported platform, unverifiable
or network failure distinctly. A candidate is **not automatically applied or staged**.
Download, file replacement, restart, rollback, actual installed binary identity,
archive support and UserData preservation remain separate acceptance work for #44.

## Explicit verified native download (no installation)

Home → More → **Download and verify update** performs a fresh lookup of the latest
`Atm4x/neuromita-rust` release, repeats exact-platform asset and strict version
selection, then calls the existing `UpdateStager` on its publisher-declared raw
executable. No previous "available" banner is trusted as an authorization to
download. Assets from the Python or Unity release feed are never eligible.

The result is stored only in the selected portable `UserData/updates/staged`
directory after SHA-256 and length checks. A previously cached file is
re-hashed before reporting success. A corrupt cache, truncated or oversized
download, publisher digest mismatch, cancellation and other I/O failures are
reported as failures, not success. While verification is active the GUI keeps
only one native update operation in flight and does not block the Slint event
loop. On GUI controller teardown the cancellation token aborts staging.

**Verified download is not installation.** No archive extraction, replacement of
a live executable, updater helper handoff, restart, rollback or mutation of
settings is attempted. The publisher must first ship a release with the exact
raw native artifact name and GitHub asset SHA-256 metadata.
