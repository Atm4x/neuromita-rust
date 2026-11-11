# Cycle72 — Versioned Documents corpus addition

## Operator path

`neuromita documents fork-add --root ABSOLUTE_OLD_CORPUS --character Kind --name NEW_DOCUMENT.md --source ABSOLUTE_UTF8_INPUT --output ABSOLUTE_NEW_CORPUS`

The command creates a **new**, independently verified, character-scoped Documents corpus directory. It copies all `.md`/`.txt` files from the old canonical root, and adds exactly one new text document. The existing root and its already-selected derived index are not modified. A `ForkedCorpusAddition` JSON receipt reports `character`, `added_name`, `source_sha256`, `output_root`, `total_documents`, and `requires_reindex=true`. There is no automatic reindex, activation, update to `SettingsApi`, or fallback.

## One publication owner

`fork-add`, `fork-replace`, and `fork-remove` compose the same internal `ForkChange` publication workflow. Its public operations retain their existing signatures. The operator must provide an existing canonical absolute root; the added input is an independent regular file at a canonical absolute path outside the old root, and the output path must be a fresh absolute directory outside the old root with an already-existing canonical parent. Unknown files and symlinks in the original corpus are refused, not silently omitted. The added name must be unique, restricted to safe `.md`/`.txt`, and not begin with a dot.

The source file is read with optimistic identity/metadata checks, must be nonempty valid UTF-8, and is restricted to 128 KiB. The prospective corpus must fit 48 files, 512 KiB total, and the 1024-chunk bound. Original bytes are independently copied into a private staging directory, with file permissions 0600 and staging directory 0700 on Unix; files are synced. Before publication, the code rechecks the original corpus, including the absence of unowned entries added while staging, and the external input against the initially admitted bytes, and verifies the staged corpus against the independently reconstructed expected document entries, including chunk content. Linux publishes with `renameat2(RENAME_NOREPLACE)`; Windows uses no-replace `MoveFileExW`; unsupported platforms fail closed. Unix syncs the parent after publication. If post-publication sync fails, it returns `PublicationUncertain` and **does not remove the possibly published candidate**. Failed prepublication staging is cleaned.

A distinct operator command must next run `documents reindex` on the new root, and activate the resulting receipt through the existing trusted `DocumentRevisionApi` and `SettingsApi` CAS/flush path. Previously admitted requests continue to read the old pinned root/index. This is a derived candidate version, **not a second settings authority**.

## Executable acceptance

`tests/documents_cli_process.rs` invokes real CLI processes to check that selected old query/index remain valid while new root query requires a distinct index; rejects collisions, unsupported names/paths, malformed or oversized UTF-8, symlink roots/sources and unknown files; enforces max-file/corpus capacity; accepts an empty source corpus; checks simultaneous competing writers and no-clobber publication. `knowledge::document_index::fork_tests` has deterministic hooks to change the incoming source after staging, inject a foreign destination at publication, and force a post-publication parent fsync error. All require no mutation to the previous root, and there is no fallback.

## Limits

This workflow is **optimistic**; an uncooperative writer can mutate files after the final verification. No source pin is cryptographically signed. The command does not perform immediate active cutover, removal/pruning, automatic rollback or crash-proof Windows parent-directory durability. It cannot ingest PDF/Office or provide semantic embedding/reranking, user-facing GUI intake, or the full R6/R7 acceptance matrix. R6 and R7 remain formally **OPEN**, total **16 PASS / 4 PARTIAL / 6 OPEN**.
