# Installed backend receipt path containment (Agent C / C3)

`load_installed_backend()` is used by installed speech composition and hardware backend selection. It must enforce the same managed-revision path boundary as `BackendInstaller::installed()`, not just validate the JSON contents of a receipt.

The reader rejects symlinks in the selected backend revision, backend-kind directory and injected managed backends root **before** reading `.neuromita-backend.json`. It also rejects the receipt itself if it is a symlink, a non-file or larger than 128 KiB, following the existing overlay receipt policy. A regular receipt still has to match the exact requested backend kind and revision and pass the pinned package validation.

A Unix regression starts with an externally stored valid backend receipt and verifies that symlinking the managed revision, kind parent or root never makes it selectable. A receipt-file symlink is also rejected. The same test confirms that a valid regular managed receipt remains readable.

This hardening addresses the direct read path; it does **not** prove TOCTOU-safe filesystem traversal under a concurrently hostile filesystem, validate every file installed by uv, or replace permission checks on the injected parent path. Interrupted stages, concurrent revision publication and removal/shared-package ownership are separate C3 acceptance work.
