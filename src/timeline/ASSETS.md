# Timeline binary attachments: current contract

`timeline::storage::assets::TimelineAssetStore` stores binary objects by SHA-256 of `media_type + NUL + bytes`. The digest binds a declared MIME type to the bytes; it does not sniff or decode the file. This is deliberately different from the AI model artifact store. The application supplies an explicit **absolute** directory and maximum per-object size; the store rejects relative or empty directory paths rather than implicitly using `cwd`, AppData or XDG. Until the shared portable-root composition is available, no parallel path resolver is introduced here.

A canonical message stores ordered JSON content parts containing `{"type":"image"|"audio"|"file", "asset": {"digest":"...", "size": n, "media_type":"..."}}`. Raw bytes never go into Timeline events or receipts. `append_checked` validates every binary reference and its integrity before asking `TimelineRepository` to append. Timeline remains the sole authority for identity, observers, branch history and receipts.

Staging uses bounded streaming and random private filenames, followed by a hard link that cannot replace an existing content-addressed object. A concurrent duplicate is accepted only after checking the existing object's integrity. Failure while writing deletes the staged file. Successful asset import followed by failed Timeline append may leave an unreferenced object, **not** a falsely accepted event. A future reachability sweep must consult all branches, snapshots and portable archives; deleting on message hide/reset or a single-branch scan is unsafe. Accordingly the store intentionally has no eager destructive GC API.

`read_visible` requires a `MitaView` fixed at the requested branch/context prefix and an event ID whose current visible content contains that exact attachment reference. A wrong observer, hidden message, revised-away attachment, or an asset in an unrelated branch returns `NotVisible`, without touching the file. Once visibility is proved, the read verifies size and digest. `verify` checks presence/corruption without exposing the bytes and is intended for controlled ingress/archive maintenance, not for application UI or provider authorization. Integrity-only checks now hash the file through a fixed 64 KiB buffer without allocating its full content; authorized `read_visible` and archive export still materialize verified bytes because their callers need the payload. These operations share the same size, media-type-bound digest, and filesystem checks.

Open boundaries for #7 and #8: the production chat ingress must adopt `append_checked`; the provider layer must receive a current `MitaView` and injected, authorized asset reader and must reject unsupported MIME types before remote calls. There is no general multi-store commit: an OS file and an SQLite transaction cannot commit atomically by virtue of sharing a method. Until archive references are tracked, automatic orphan deletion is prohibited. Existing Timeline messages without binary content continue to work unchanged.

Tests in `tests/timeline_assets.rs` cover deduplication, concurrent import, bounded/failing I/O, corruption, missing files, invalid references, in-memory and SQLite visibility, fork/historical reads, hide, import-before-receipt, and SQLite restart. These are local fixtures and do not prove end-to-end chat attachment UI or provider uploads.

For newly published objects, Unix imports synchronize the parent AssetStore directory, the `objects/` directory, and the final shard after the hard-link publication. The same directory synchronization applies to deduplicated concurrent imports before returning success. This closes the gap where a blob file and shard were synced but the newly created shard's parent entry had not been persisted. It does **not** certify full power-loss resilience across every OS/filesystem or a newly created external installation-root ancestor, nor does it solve the documented hostile concurrent path-swap race. On non-Unix platforms the standard library offers no equivalent directory-sync guarantee through this implementation; platform-specific durability remains a separate task.

### Revision content identity

`MessageRevised` has exactly one replacement content source:
`payload.message.content`. A top-level `payload.content` on the same event
is rejected by domain validation, even if its bytes are identical. Asset
verification (`append_checked`) and SessionArchive reference collection use
the same canonical resolver, so a shadow field cannot mask an unavailable
replacement or cause the archive to omit the replacement's binary.

### Pinned on-disk archives (B42 read-only retention boundary)

`archive::PinnedExternalArchive` binds an explicit absolute archive path to the
SHA-256 of its **serialized archive bytes**. `inventory_pinned_memory` and
`inventory_pinned_sqlite` re-open every pin, reject symlinks and non-regular
files, bound the read to the existing native archive limit, recheck the byte
fingerprint, then validate its full `SessionArchive` structure and binary
closure before inventorying all canonical local branches and the pinned roots.
A missing or replaced pin fails the entire inventory; an archive that existed
when a prior inventory ran is **never** trusted from that prior result. These
APIs never load private assets into the user's MitaView, and never delete
anything. Pin paths are caller-owned; there is no implicit `cwd` lookup.

**GC remains blocked:** these checked pins do *not* certify that the caller
knows all external archives, that in-progress imports are settled, or that
other processes cannot append an asset after the read. A persistent
owner-controlled pin registry, complete external roots, a coordinated
revision/lease revalidation boundary and crash recovery are still needed
before any physical deletion can safely be enabled. The returned
`orphan_candidates` are inspection diagnostics, not a deletion plan.
