# C3: installed overlay receipts use the managed path boundary

A selected plugin overlay is read by `load_installed_overlay` during speech composition. Previously the direct receipt reader rejected symlinks only at the revision directory and receipt file. A symlink at the plugin's parent or at `AIData/overlays` still allowed a valid-looking receipt from an external directory to pass, even though `OverlayInstaller::installed()` already rejected that same layer. Therefore inspection and activation disagreed.

The receipt reader now calls the shared `reject_symlinked_layer_components` guard before reading its marker, covering the selected revision, immediate plugin parent and managed overlays root. It retains receipt size, strict plan identity and marker symlink checks. A Unix regression constructs a valid external receipt and verifies both symlink-parent variants are rejected.

The trusted injected data root is unchanged; this is a read-path parity fix, not a new filesystem resolver. Nested package files, concurrent symlink replacement and package-distribution verification during direct receipt loading remain separate concerns for issue #42. The immutable staging publication rules are described in [cancellation handling](ai-installables-cancellation-c3.md).
