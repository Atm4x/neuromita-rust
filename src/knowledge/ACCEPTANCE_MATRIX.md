# Agent A — проверяемая карта приёмки (#21 / #22 / #23 / #50)

**Статус:** рабочая реконструкция 26 сегментов на основе Cycle 15–25 и актуальных GitHub issues, **не официальный список GitHub**. Исходные шкалы `5/7`, `4/7`, `4/7`, `3/5` в прежних handoff **не имели опубликованного соответствия «номер сегмента → критерий»**. Поэтому эта таблица задаёт прозрачную интерпретацию *впредь*, а не притворяется историческим источником. Изменение формулировки/веса критерия требует отдельной записи в отчёте; число PR/LOC/зелёных unit-тестов не является доказательством закрытия макрокритерия.

Обозначения: **PASS** — подтверждённый функциональный срез; **PARTIAL** — работает не во всех требуемых пользовательских сценариях; **OPEN** — нет полной проверяемой реализации. Итог 16 PASS, 4 PARTIAL, 6 OPEN после Cycle29; до него было 16 PASS, 3 PARTIAL, 7 OPEN. Все 10 не полностью закрытых сегментов ещё требуют работы. D4 переоценён из OPEN в PARTIAL по уже работающему single-transaction memory+variables, а не объявлен завершённым. Эти сегменты различны по стоимости и могут иметь общие зависимости.

## #21 Memory — 5 PASS, 1 PARTIAL, 1 OPEN

| Сегмент | Проверяемое поведение | Статус | Ограничение/доказательство |
|---|---|---|---|
| M1 | Предметные Fact CRUD, scope персонажа, Timeline-владелец | PASS | `character::memory_access`, memory editor/live tests |
| M2 | Forget/restore/soft-delete, TTL, lifetime of known data | PASS | memory lifecycle + SQLite/reopen tests |
| M3 | Protected, islands и правила ограничения capacity | PASS | `character::memory`, dedicated memory tests |
| M4 | Dedup/merge повторного сохранения без второй canonical DB | PASS | deterministic commands + presenter regression |
| M5 | Reset/switch/reopen, invalidation старого редактора | PASS | Timeline/Presenter stale tests |
| M6 | Полный user-facing parity с **активным** Python-путём | PARTIAL | Нужны поточные Python fixtures, различия по приоритетам, capacity, merge, UI/ASR operations |
| M7 | Полные recoverable import/restore + частичные ошибки и сохранность | OPEN | Нет сквозной подтверждённой restore-приёмки для пользовательских данных |

## #22 RAG — 4 PASS, 1 PARTIAL, 2 OPEN

| Сегмент | Проверяемое поведение | Статус | Ограничение/доказательство |
|---|---|---|---|
| R1 | Изолированный Timeline History lexical/real FTS5 source | PASS | `history_retrieval`, History API, CLI fixtures |
| R2 | Embeddings, cache invalidation и полное embedding-space identity | PASS | `embedding`, `semantic_memory`, mixed-space tests |
| R3 | Источники, bounded fusion и проверяемый provenance | PASS | `retrieval_fusion`, `graph_retrieval` fixtures |
| R4 | Opt-in History generation, active/archive window и atomic acceptance | PASS | History generation tests, OpenAI/Gemini typed window |
| R5 | Реальная семантика Python source selection, ranking/reranking/FTS modes | PARTIAL | Rolling-window Rust ≠ Python `history.is_active`; нет полного cross-source reranker |
| R6 | Documents source, scoped indexing/reindex, query integration | OPEN | Нет полноценного Documents production-retrieval path |
| R7 | User-facing full failure matrix: missing/corrupt index, timeouts, partial source, provider/model failures | OPEN | Некоторые ошибки типизированы, но все пользовательские сценарии не подтверждены end-to-end |

## #23 Knowledge Graph — 4 PASS, 1 PARTIAL, 2 OPEN

| Сегмент | Проверяемое поведение | Статус | Ограничение/доказательство |
|---|---|---|---|
| G1 | Канонические source records + только derived relations | PASS | `CharacterMemories`, `ReplaceExtractedRelations` |
| G2 | Source revision/provenance, character isolation и stale/TTL exclusion | PASS | Graph provenance fixtures, Timeline acceptance |
| G3 | Bounded graph query и явный vector/graph retrieval fusion | PASS | `graph_retrieval` tests |
| G4 | Атомарное preview/confirmed clear и **инъецируемый** typed reindex | PASS | #213/#225/#229, `graph_maintenance_api` tests; **не** означает наличие stock AI backend |
| G5 | **Реальный model-backed GraphExtractor** с pinned character/model/preset и extraction DTO | PARTIAL | Composition готов; штатный AI adapter **отсутствует**. `StructuredResponse` нельзя обходить |
| G6 | Полное подтверждённое Python ingestion/GC/type backfill/merge lifecycle | OPEN | Нет end-to-end AI processing и приёмки обработки entity aliases/GC |
| G7 | Наблюдаемый пользовательский graph browser, полный provenance/accepted-output и обработка ошибок | OPEN | Cycle 25 добавляет источник-ориентированный inspection в Slint/CLI, но это не полный графовый canvas/UI/accepted-output parity |

## #50 Data Maintenance — 3 PASS, 1 PARTIAL, 1 OPEN

| Сегмент | Проверяемое поведение | Статус | Ограничение/доказательство |
|---|---|---|---|
| D1 | Выбор/просмотр/редактирование канонической памяти, stale state | PASS | Memory Editor/Presenter/API |
| D2 | Preview + подтверждённая очистка derived embedding cache | PASS | #198, bounded cache preview/revision checks |
| D3 | Preview + подтверждённая очистка source-scoped graph relations; reindex capability path | PASS | #213/#220/#225/#229. Реальная AI-инференция отдельно G5 |
| D4 | User-scoped export/import с версиями формата, duplicate IDs, mapping, preview и повтором | PARTIAL | Cycles26–27: **retained MemoryRecord-only** v1/v2 exact-ID перенос, v2 SHA-256 checksum (legacy v1 accepted), crash-safe no-clobber file publication, preview/confirmed CLI/API и SQLite reopen; **не переносит** Timeline history, variables или ручной Graph; ограничение 256 записей/1 MiB. Cycle28: **отдельный** v1 перенос только user-defined canonical CharacterVariables (SHA-256, строгая валидация, preview, confirmed atomic append, SQLite reopen), см. `VARIABLE_TRANSFER.md`; Cycle29 объединил retained MemoryRecord + user variables в **один** проверяемый envelope с **одним** version-guarded `append_guarded` через CharacterHandle (атомарность двух доменов, SQLite reopen), см. `PORTABLE_TRANSFER.md`. История/SessionArchive и полный `world-db-export` **всё ещё не поддержаны**; D4 пока не PASS. |
| D5 | Recovery/partial failure/cancellation/reopen пользовательского набора, включая malformed archives | OPEN | Cycles26–27 подтверждают cancellation, bad JSON, conflicting IDs, checksum tampering, concurrent no-clobber and fail-before-publication cleanup, SQLite reopen **только MemoryRecord**. Cycle28 отдельно проверяет variable duplicate keys, corruption, foreign target, cancel, stale-write/SQLite reopen; с Cycle29 атомарность Memory + variables подтверждена одной транзакцией, но History/SessionArchive всё ещё отсутствуют. Cycle48 добавил явную атомарную перестройку четырёх derived SQLite Timeline lookup-таблиц из канонических событий с откатом при отказе и SQLite reopen (`SQLITE_DERIVED_RECOVERY_20261108.md`); пользовательский workflow ремонта, полная матрица IO и SessionArchive всё ещё отсутствуют |

## Приоритет по зависимостям (не по числу незакрытых клеток)

- **G5** — критический внешний AI/Generation contract от Agent C; затем G6/G7 и соответствующий user-facing reindex.
- **D4 → D5** — независимый от AI, но крупный пользовательский перенос данных; сначала versioned scoped portable format и dry-run без разрушительных операций, затем подтверждённый atomic apply/reopen/errors.
- **R6 → R5/R7** — Documents и production mode/reranking parity, их failure contracts.
- **M6/M7** — полноценная поведенческая и recovery parity; согласовывать с Agent B, если изменение затрагивает canonical SessionTimeline.

Следующий отчёт обязан показывать *конкретные G/M/R/D идентификаторы*, а не только дробь. Изменение любого статуса требует ссылки на функциональный PR, тест запуска/результата и проверенное пользовательское поведение; `cargo check` без исполнения GUI callback-тестов не является доказательством полного GUI acceptance.

Cycle49 D5/M7 addendum: a trusted-local `timeline-index inspect` / `repair --confirm` offline operator flow now previews canonical-versus-derived drift, includes a content-bound optimistic concurrency fingerprint, refuses stale/corrupt snapshots and applies the original four-index rebuild under one SQLite write transaction. Source code: `timeline::storage::sqlite::maintenance`, CLI `src/timeline_index_cli.rs`; supporting details in `SQLITE_DERIVED_RECOVERY_20261108.md`. This is not full History/SessionArchive or canonical-corruption recovery; D5 and M7 stay OPEN and totals remain 16 PASS / 4 PARTIAL / 6 OPEN.

Cycle50 D5/M7 addendum: the `timeline-index` offline CLI is now covered by five real separate-process tests (`tests/timeline_index_cli_process.rs`) confirming damaged derived-index detection, explicit repair/reopen, canonical EventId/receipt/revision preservation and nonzero error exit codes. The optimistic token additionally binds Timeline SQLite schema/trigger definitions; post-repair drift is rejected with an atomic rollback. This does not confer application-process exclusion or canonical/SessionArchive recovery. **Formal D5/M7 remain OPEN**, and overall matrix stays 16 PASS / 4 PARTIAL / 6 OPEN.

Cycle51 D5/M7 addendum: `SqliteTimelineRepository::create_verified_backup()` and the offline `timeline-backup create` CLI provide an independently validated, no-overwrite, `VACUUM INTO` canonical SQLite snapshot including committed WAL entries, branch selection, receipts, logical revision, derived indexes and reset semantics. They refuse corrupt canonical/derived source, mismatched pre/post snapshots, missing paths and existing destinations. This is a single-Timeline-database backup **not** restore of the full user dataset or SessionArchive. **Formal D5/M7 remain OPEN**; totals remain 16 PASS / 4 PARTIAL / 6 OPEN. See `SQLITE_CANONICAL_BACKUP_20261108.md`.

Cycle52 D5/M7 addendum: explicit `SqliteTimelineRepository::restore_verified_backup_into_new_database` and trusted offline `neuromita timeline-restore into-new --backup ... --output ... --confirm FINGERPRINT` revalidate the supplied SQLite Timeline backup and publish a consistent standalone database to a **new** path, never replacing active data. The fingerprint binds the expected canonical/derived data and schema; native `VACUUM INTO`, SQLite integrity/FK checks and no-overwrite publication are reused from the proven backup pipeline, not duplicated. Real separate-process tests exercise EventId/receipt/branch/revision/reset preservation and wrong/corrupt/missing backup rejection. Fingerprint is not authentication, and the command performs **no active-database cutover, full-dataset restoration, SessionArchive import, or corrupt canonical salvage**. Formal M7/D5 remain OPEN; overall 16 PASS / 4 PARTIAL / 6 OPEN. See `SQLITE_CANONICAL_RESTORE_20261108.md`.

Cycle53 D5/M7 addendum: the shared SQLite backup/into-new-restore publication primitive now rejects Unix source-path inode replacement even when the canonical+derived+DDL fingerprint is identical; deterministic prepublication tests cover injected late failure, staging cleanup, an output-creation race with no overwrite, and non-UTF-8 SQLite sidecar cleanup. The verified single SQLite backup/restore remains separate from user-data-set recovery, SessionArchive and live cutover. **M7/D5 remain OPEN**; 16 PASS / 4 PARTIAL / 6 OPEN. See `SQLITE_SNAPSHOT_PUBLICATION_FAILURES_20261109.md`.

Cycle54 D5/M7 addendum: the shared SQLite `VACUUM INTO` snapshot publisher now returns typed `SnapshotPublicationUncertain` after successful destination hard-link creation if directory synchronization fails, **without deleting** the destination (which a competing actor may have replaced). Independently verifiable `SqliteTimelineRepository::inspect_verified_snapshot` and `neuromita timeline-backup inspect --database ... [--expect FINGERPRINT]` provide non-repairing, read-only canonical/derived/SQLite-integrity checks for an existing path, including committed WAL state. Native injected-failure and real subprocess tests cover postlink uncertainty, foreign-file protection, fingerprint comparison and malformed source rejection. No authenticated source provenance, non-Unix directory durability guarantee, live cutover or SessionArchive dataset recovery. **M7/D5 remain OPEN**, total **16 PASS / 4 PARTIAL / 6 OPEN**. See `SQLITE_SNAPSHOT_DURABILITY_INSPECTION_20261109.md`.

Cycle55 D5/M7 addendum: the independent read-only `neuromita recovery-inventory inspect/verify` CLI enumerates **only the known local application Timeline SQLite and four optional configuration JSON paths**, binding verified canonical/derived Timeline content to bounded per-file SHA-256 evidence and rechecking a strict versioned receipt. It explicitly reports absent optional paths and missing owner coverage, rejects damaged/symlinked/mutated sources and unsupported or altered receipts, never writes user data or claims a restorable dataset. The owner-specific SessionArchive exporter/importer, external assets, cross-owner atomic snapshot/restore and active process exclusion remain absent. **M7/D5 OPEN, D4 PARTIAL**, total **16 PASS / 4 PARTIAL / 6 OPEN**. See `RECOVERY_INVENTORY_20261109.md`.
