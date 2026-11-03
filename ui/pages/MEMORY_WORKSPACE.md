# Character Memory page — embedded Core workspace

`neuromita-gui --data-dir PATH` opens the normal launcher shell and lazily starts
an embedded `ApplicationLifecycle` on first visit to the Memory page. The data
path uses the same default rules as the terminal Core (`NEUROMITA_DATA_DIR`,
`APPDATA/NeuroMita`, or XDG/HOME config). Its SQLite database is the real
`timeline.sqlite3`, not a GUI copy or demo data.

The embedded workspace registers **no** generation provider and requires no
API token. It uses the existing Timeline owner, Character actors,
`GuiPresenter.characters`, and versioned memory snapshots. Slint emits typed
user intents into an asynchronous worker. Only the application layer resolves
and commits domain commands. No GUI code touches `SessionTimeline`.

Supported interactions: select a character, inspect fact/island records, add
facts, edit active facts and islands, set an island, forget, restore, toggle
protection, soft-delete, and explicitly refresh the selected character.
Hard-delete/physical purge and administrative backups are intentionally not
exposed in this local UI.

The embedded workspace is a **trusted local** editor, not a remote
administrator authorization service. Do not open the same data directory for
writes from both the terminal Core process and `neuromita-gui`: cross-process
write coordination is not part of this integration. If a different Core
instance owns the data, use an IPC control API once that contract is available.
The page refreshes on explicit user actions; a pushed external memory change
stream is not implemented. No AI Engine functionality or inference is needed.

Run with `--features gui` and `sqlite-system` (default). Building without
`sqlite-system` is supported, but persistent Memory initialization returns an
explicit unavailable state instead of silently substituting transient memory.
