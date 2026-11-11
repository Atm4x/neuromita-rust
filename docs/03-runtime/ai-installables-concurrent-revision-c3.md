# Concurrent immutable layer publication (Agent C / C3)

Two installers can prepare identical pinned backend/overlay revisions concurrently, each in its own UUID staging directory. Their package lock and receipt describe the same immutable destination; the first successful publication must not turn the second install into a false conflict.

The shared `LayerInstaller::publish` now checks the **already installed revision** when another installer has published the destination during its uv operation, or when the final filesystem rename rejects an already-present directory. An exact verified receipt, locked package inventory and approved action markers permit returning the existing path. A different plan, corrupted receipt or incomplete package inventory remains a conflict/error. Each attempt still owns its private staging directory through cancellation-safe cleanup.

A Unix test uses two concurrent actual mock-uv child processes that block until both entered installation. Both can then publish/observe the same backend revision. The test verifies both operations resolve to the one installed directory with valid locked distribution metadata. A separate existing sequential regression asserts a changed plan for the same revision remains a conflict.

This guarantees the idempotent **observed race** with an existing nonempty immutable revision; it does not constitute an atomic no-overwrite primitive against malicious concurrent filesystem mutation or promise multi-process orphan cleanup after a whole-app crash. Those remain separate installation hardening concerns.
