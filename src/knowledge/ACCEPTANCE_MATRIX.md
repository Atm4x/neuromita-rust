# Agent A — проверяемая карта приёмки (#21 / #22 / #23 / #50)

**Статус:** рабочая реконструкция 26 сегментов на основе Cycle 15–25 и актуальных GitHub issues, **не официальный список GitHub**. Исходные шкалы `5/7`, `4/7`, `4/7`, `3/5` в прежних handoff **не имели опубликованного соответствия «номер сегмента → критерий»**. Поэтому эта таблица задаёт прозрачную интерпретацию *впредь*, а не притворяется историческим источником. Изменение формулировки/веса критерия требует отдельной записи в отчёте; число PR/LOC/зелёных unit-тестов не является доказательством закрытия макрокритерия.

Обозначения: **PASS** — подтверждённый функциональный срез; **PARTIAL** — работает не во всех требуемых пользовательских сценариях; **OPEN** — нет полной проверяемой реализации. Итог 16 PASS, 3 PARTIAL, 7 OPEN. Все 10 не полностью закрытых сегментов ещё требуют работы. Эти сегменты различны по стоимости и могут иметь общие зависимости.

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

## #50 Data Maintenance — 3 PASS, 0 PARTIAL, 2 OPEN

| Сегмент | Проверяемое поведение | Статус | Ограничение/доказательство |
|---|---|---|---|
| D1 | Выбор/просмотр/редактирование канонической памяти, stale state | PASS | Memory Editor/Presenter/API |
| D2 | Preview + подтверждённая очистка derived embedding cache | PASS | #198, bounded cache preview/revision checks |
| D3 | Preview + подтверждённая очистка source-scoped graph relations; reindex capability path | PASS | #213/#220/#225/#229. Реальная AI-инференция отдельно G5 |
| D4 | User-scoped export/import с версиями формата, duplicate IDs, mapping, preview и повтором | OPEN | Cycle26: **retained MemoryRecord-only** v1 exact-ID перенос, preview/confirmed CLI/API и SQLite reopen; **не переносит** Timeline history, variables или ручной Graph; ограничение 256 записей/1 MiB. Полный `world-db-export`/SessionArchive не поддержан, см. `MEMORY_TRANSFER.md` |
| D5 | Recovery/partial failure/cancellation/reopen пользовательского набора, включая malformed archives | OPEN | Cycle26 подтверждает cancellation, bad JSON, conflicting IDs и SQLite reopen **только MemoryRecord**. Общая матрица всех таблиц, частичных IO сбоев и широкого reindex отсутствует |

## Приоритет по зависимостям (не по числу незакрытых клеток)

- **G5** — критический внешний AI/Generation contract от Agent C; затем G6/G7 и соответствующий user-facing reindex.
- **D4 → D5** — независимый от AI, но крупный пользовательский перенос данных; сначала versioned scoped portable format и dry-run без разрушительных операций, затем подтверждённый atomic apply/reopen/errors.
- **R6 → R5/R7** — Documents и production mode/reranking parity, их failure contracts.
- **M6/M7** — полноценная поведенческая и recovery parity; согласовывать с Agent B, если изменение затрагивает canonical SessionTimeline.

Следующий отчёт обязан показывать *конкретные G/M/R/D идентификаторы*, а не только дробь. Изменение любого статуса требует ссылки на функциональный PR, тест запуска/результата и проверенное пользовательское поведение; `cargo check` без исполнения GUI callback-тестов не является доказательством полного GUI acceptance.
