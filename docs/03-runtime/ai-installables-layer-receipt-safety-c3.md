# Installed layer receipt validation (Agent C / C3)

The shared `LayerInstaller::installed()` is used before installation and when deciding whether to reuse an immutable backend or overlay revision. It must not classify a layer as installed when its receipt points to an external file or an unbounded payload.

`layer::reject_unsafe_layer_receipt()` is now the single internal receipt-type and size policy for the installer and both direct readers (`load_installed_backend()` and `load_installed_overlay()`). It checks the marker using `symlink_metadata`, rejects symlinks, non-regular files and files over 128 KiB, and leaves missing markers to the reader's existing error mapping. The managed revision and parent symlink policy is still enforced separately by `reject_symlinked_layer_components()`.

A Unix regression installs an empty-lock overlay, replaces its managed receipt with a symlink to an otherwise valid external JSON file, and checks that both `installed()` and `install()` report a conflict rather than treating the foreign JSON as proof of an installed layer. It then tests an oversized but syntactically valid JSON file and confirms that restoring a regular receipt makes the revision available again.

This is a path/size check on the receipt, not a cryptographic integrity guarantee for installed packages or a defense against filesystem mutation between validation and read. Crash-orphan staging cleanup, concurrent publication, shared-package removal and complete voice-asset validation remain separate C3 work.
