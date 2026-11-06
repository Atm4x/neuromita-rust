# Rust updater — verified release artifact staging (D3 partial)

Status: **implemented as a headless library boundary**, not yet an end-to-end updater. Issue #44 remains open. There is no user-facing install or restart control in this slice.

## API and ownership

- `updater::GithubUpdateCatalog::new("owner/repo")` fetches GitHub `releases/latest` or the exact specified tag, rejects draft/mismatched releases, duplicate asset names, malformed metadata and releases lacking SHA-256 for any supplied asset. `CheckedRelease` supplies the actual tag and typed verified `UpdateAsset` entries, not a claim that a newer app version exists. Release version comparison is not yet implemented.
- `UpdateStager::new(&PortableDataRoot)` receives the portable root from composition. `stage(&UpdateAsset, &CancellationToken)` downloads through HTTPS with a finite redirect policy, uses a declared size cap (8 GiB), hashes bytes during streaming with `ring` SHA-256 and checks exact byte count against GitHub publisher metadata.
- Verified bytes are synced and published without overwriting another file using `hard_link` under `<portable UserData>/updates/staged/sha256-<digest>.artifact`. Existing cached artifacts are fully rehashed before reuse. Untrusted asset names never influence output paths.
- A failed, cancelled or interrupted operation drops its private `.download-<uuid>.part` in normal process shutdown, never publishes a verified file and never mutates unrelated UserData. A process crash can leave inert `.part` files; crash garbage collection is not implemented.
- The updater does not extract archives, write executables, replace installed files, kill processes, delete user data or trigger restarts. Existing `ReleasesPresenter` is a separate read-only news/listing projection; these source/byte-validating interfaces do not introduce a second canonical settings or Timeline store.

## Integrity and platform limits

Publisher SHA-256 confirms transport/content integrity relative to GitHub metadata; it is not a code-signature or a guarantee that the publisher was uncompromised. Missing SHA-256 is an explicit blocker, not silently replaced by a locally computed hash. HTTPS downgrade redirects are refused. The generated HTTP-only loopback allowance exists only in `cfg(test)` builds.

Publishing uses an atomic hard link, so filesystems without hard-link support (some portable FAT/removable volumes) fail safely; no cross-volume non-atomic fallback is attempted. No directory fsync/power-loss durability or Windows installation apply/rollback is claimed. A stage directory symlink is rejected, but adversarial filesystem mutation after path checks, hostile concurrent processes and package trust-signature verification are outside this first slice.

`Atm4x/NeuroMita` currently publishes PythonBuild and UnityBuild archives with GitHub asset digests. `Atm4x/neuromita-rust` has no verified latest release endpoint as of this implementation check; do not infer that PythonBuild is a Rust self-update. **Do not wire an Apply button** based only on a successfully staged file.

## Required follow-on work before closing #44

Integrate real installation/channel/component identity into the Rust app and presenter; validate asset type and release compatibility; implement bounded retry/Range resume and stale partial cleanup with ownership; select a supported audited archive decoder/install package format; stage the complete install beside the target volume; explicit `apply -> verify -> recover/rollback` with locked-file and power-interruption tests; owned-process restart handoff; Windows and Linux end-to-end acceptance. Guarantee that an interrupted installation never claims success and that uninstall never silently deletes portable `UserData`.
