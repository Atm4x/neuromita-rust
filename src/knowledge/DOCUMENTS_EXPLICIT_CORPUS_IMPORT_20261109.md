# R6 — Explicit, no-clobber import into an existing character Documents corpus (Cycle69)

The canonical Documents owner remains the operator-chosen **existing** absolute character directory. A derived index is not the owner of any document. `neuromita documents import` takes one explicit absolute regular UTF-8 source file and a new `.md` / `.txt` filename, performs a bounded copy into that directory, and prints a SHA-256 receipt with `requires_reindex=true`. It never modifies SettingsApi, rewrites an existing corpus file, generates an index, activates a revision or invokes an LLM.

Example:

```text
neuromita documents import --root /absolute/character-docs --character Kind --source /absolute/incoming-note.md --name garden.md
neuromita documents reindex --root /absolute/character-docs --character Kind --revisions-dir /absolute/immutable-indices
```

The operator may then save the `reindex` JSON receipt and activate the new index in the running application's `/documents activate Kind EXPECTED_SETTINGS_REVISION /absolute/receipt.json` command. Existing pinned messages never switch revisions. Once the canonical directory gains a new `.md` or `.txt`, older indexes become **stale by design**: generation fails closed until an explicitly reindexed and activated revision is admitted; there is no lexical/semantic fallback or automatic retargeting.

Import admission rejects non-absolute or noncanonical paths, direct and intermediate source symlinks, source files inside the destination directory, target collisions (including symlinks), unsupported names and extensions, invalid UTF-8 and empty text, and violations of the same 48-file/128-KiB-single/512-KiB-total/1024-chunk corpus limits as the index. Source metadata is compared before and after bounded read. Immediately before publication the source bytes and existing corpus snapshot are reverified; the destination is installed via a unique private staging file and `hard_link` no-clobber publication. On Unix both staged data and directory entry are synced; a directory sync failure after link yields explicit `PublicationUncertain` **without deleting** possibly published data. A failed pre-publication import cleans up staging. No silent copy-overwrite fallback.

This is optimistic, not a filesystem transaction spanning the source and an external concurrent writer: another process may modify the source after the final comparison or alter the corpus after publication. It does not synchronize with Agent B/SessionArchive, models, external assets, live GUI processes, or active generation. An operator must coordinate writers. Destination parent must support hard links for this publication mode; no alternate mode is silently selected. SHA-256 is content evidence, not authenticated author provenance. R6 and R7 remain OPEN pending complete user-facing workflow and semantic/format requirements.
