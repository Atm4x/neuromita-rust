# Session Timeline — перенос Python-контрактов в Rust

**Feature branch:** `feature/session-timeline-port` (основана на `audit/acyclic-dependencies`).
**Python reference:** `Atm4x/NeuroMita`, `session-timeline`, `5f3d061f523d9abefa6a264bb2953048a935fa65`.
**Rust starting point:** `8a4922b2a8cd19e757525030d57dea4bf6b35528` (содержимое опубликованной ациклической ветки).

## Цель

Перенести **семантику**, а не структуру Python-сервисов: канонические неизменяемые факты, независимые субъективные знания Миты, причинность, сохранение веток и восстановление состояния. Оставить один Rust Core, один активный пользовательский контекст, отдельные источники событий Unity и Sandbox. Не подменять Unity-мир моделью Rust. Не внедрять EventBus, общий service locator или альтернативное хранилище «истины» в MitaView.

## Этап A — чистое математическое / доменное ядро (`src/timeline/domain/`)

- [x] Типизированные `SessionId` / `BranchId` и устойчивые `EventId` / `CharacterId`.
- [x] Набор `EventKind`, типизированная причинность `reply_to`/`react_to`/`caused_by`/`observes`.
- [x] Immutable `TimelineFact`, event drafts, явная последовательность фактов, сохранение источникового времени (не concurrency-token).
- [x] Атомарный append в **in-memory** store по `expected_head` (без частично записанных batch).
- [x] Идемпотентность по ключу + сохранение receipt: повторный идентичный intent получает тот же результат после продвижения головы; конфликтующие намерения отклоняются; пустые операции не создают событий.
- [x] Запрет ссылок на ещё не существующие события, чужие ветки и недоступные персонажу цели реакции.
- [x] Private observations, явное позднее раскрытие `EventObserved` и разделение времени события от времени получения знания.
- [x] `CharacterContextReset` обнуляет проекцию знаний без удаления событий.
- [x] Presence как fold `CharacterJoined` / `CharacterLeft` без ретроактивного раскрытия фактов.
- [x] Fork только от checkpoint; prefix cutoff + отдельное продолжение родителя, включая nested forks.
- [x] Правки/скрытие/восстановление сообщений как независимые факты; базовая per-character `character_messages()` проекция.
- [ ] Полная Python `CharacterMessageProjection` (UI metadata, bounded paging, cache, resize semantics).
- [ ] Полная валидация versioned JSON-envelope, UTC-ISO и всех Python-правил; текущий Rust домен хранит UTC Unix milliseconds.
- [ ] Унификация внешних `string` identities с Python при импорте архивов (сейчас доменные IDs UUID).

## Этап B — постоянное хранилище и перенос между хранилищами

- [x] `TimelineRepository`: независимый объектный порт доменной семантики. CFMD должен реализовать этот же контракт без изменения событий/веток/знаний.
- [x] SQLite (`sqlite-system`): WAL, нормализованные ветки / события / свидетели / receipts; `BEGIN IMMEDIATE` для атомарных операций и проверка `expected_head` под блокировкой записи; открытие после завершения Core.
- [x] Отдельный порт `TimelineSelectionRepository`: durable CAS выбора ветки; повторный выбор инкрементирует generation.
- [x] SQL-индексированное ограниченное чтение прямых фактов по ветке, позиции, количеству, непосредственному наблюдателю и inherited prefix; это ещё не готовый кеш MitaView.
- [x] Тесты одного контракта на in-memory / SQLite: receipt после reopen, атомарный batch и SQL fault injection после обновления head, конкурентные отдельные соединения, совпадение страниц через ветвление.
- [x] Тёплый `append`: кеш подтверждённого доменного store по транзакционной `timeline_revision`, без повторного SQL-чтения всей истории при последовательных записях; при другом писателе, неуспехе SQL или изменениях через `create_session`/`fork` кеш инвалидируется. Холодная загрузка ограничена **родовой цепочкой ветки** (без сестринских веток и других сессий), а кеш привязан к BranchId. Все правила остаются в одном доменном `TimelineStore`.
- [x] SQLite `branch()`, durable selection CAS и повторное `create_session` для уже существующего root выполняются без восстановления всей истории или изменения revision при повторном открытии.
- [x] SQLite `world`, `view` и `knows` читают только мир запрашиваемой ветки (с унаследованными префиксами) и используют **ту же** доменную функцию проекции знания; другие сессии не загружаются.
- [x] **Инкрементальная проверка на тёплом пути**: `TimelineStore` поддерживает принадлежащий домену `WorldIndex` по ветке (event identities, external message identities, knowledge after reset/disclosure). Batch сначала проверяется на overlay без изменения основного индекса; при ошибке не изменяются ни факты, ни индекс. После успешного append индекс расширяется на новые факты. На тёплом пути проверки зависят от размера batch и ссылок, а не длины всей истории. [Измерения](PERFORMANCE.md).
- [ ] **Холодный append независимо от размера истории**: после перезапуска либо инвалидации SQLite кеша `load_lineage()` по-прежнему восстанавливает предков и факты ветки; первичное построение индекса O(история). Нужны persistent/исходные индексные query-контракты без параллельной SQL-реализации правил. `WorldIndex` — производные данные, а не второй источник истины.
- [ ] Перенести вложения `TimelineAssetStore` и контентно-адресуемые ссылки с верификацией.
- [ ] Версионированный `SessionArchive`: export/import, строгая проверка дерева, ссылок, receipts и assets.
- [ ] Полный crash recovery для сквозных операций Python state participants / Unity / Character — SQLite транзакция только для timeline не решает распределённое восстановление.

## Этап C — MitaView / история / наблюдения

- [x] Базовый **беспобочный** `character_view`: события + момент получения знания, соблюдение reset/disclosure; поздние раскрытия разрешаются через индекс уже встреченных фактов, без квадратичного поиска по всей истории.
- [x] Семантические сообщения, actor/role, правки, скрытие и восстановление без изменения исходного события.
- [ ] Постраничное чтение и эквивалентность полному Python-формату провайдерских сообщений.
- [ ] Context summaries, инвалидация по branch / head / epoch / версии правил без попадания скрытых фактов в prompts.
- [ ] Историческая реконструкция `at`; выборка большого объёма без полного сканирования для каждого запроса.

## Этап D — связи с живым ядром и средами

- [x] Подключить in-memory доменный `TimelineStore` как единственный источник фактов в действующем `SessionTimeline` (`timeline/mod.rs`); старые `HistoryView`/`EventContent` стали вычисляемыми compatibility-проекциями.
- [x] В `ApplicationConfig.timeline` можно **явно** выбрать SQLite repository по пути/SessionId/BranchId (опциональная Cargo feature `sqlite-system`), единственное хранилище событий для работающего Core. Старый transient-режим сохранён как явно выбранная конфигурация.
- [x] Живой `MessageHandler` переносит `TimelineTurn` из `CharacterHandler.prepare` через генерацию в `CharacterHandler.accept`; выбор другой ветки/повторный выбор, сброс знаний и исчезновение права знать trigger отклоняют устаревшее принятие.
- [x] **Локальная адресная отмена in-flight запросов** при `select_branch`: наблюдение `watch` за текущей `TimelineSelection` прерывает ожидание LLM или TTS в `MessageHandler` и переводит устаревший запрос в `Cancelled`. Запросы, начатые с новой selection, не отменяются. `CharacterHandler.accept` сохраняет окончательную проверку. Нет общего EventBus.
- [ ] **Внешняя отмена и reset:** смена selection другим процессом через SQLite фиксируется при окончательной проверке, но не уведомляет немедленно локальный `watch`; reset знаний персонажа также пока инвалидирует при приёмке, без ранней targeted cancellation. Ещё нужны явные межпроцессные/контекстные контракты, а не периодический опрос всего Core.
- [ ] Завершить **сквозную транзакционную приёмку** состояния персонажа и Timeline. Сейчас приёмка проверяет актуальность selection/epoch и согласует собственный `CharacterState` в памяти с проекцией истории, но durable commit mutable state/effects не реализован.
- [x] Локальный контракт `SessionWorkspace`: selection CAS, generation, pin и validation для reset/смены ветки; **пока не подключён** к работающему Request/CharacterHandler.
- [ ] `SessionTimelineRuntime` целиком: есть durable selection CAS, pin в реальном MessageHandler и проверка перед accept, но ещё нет целевой отмены активных токенов, восстановления владельцев состояния до активации или сквозной атомарности.
- [ ] `ConversationTimeline`: message identities, reply/react/initiate; отдельные факты речи и наблюдений.
- [ ] Unity ingress: сведения о мире, видимость, приватность, игровые события, idempotent receipts.
- [ ] Launcher Sandbox: самостоятельный messenger context (удалённое общение), без симуляции Unity.
- [ ] `SessionRestoration`: state participants, journal, компенсирующие переходы и возобновление после сбоя.

## Инварианты и ограничения

- **Один источник истины:** Python `SessionTimeline` → Rust authoritative timeline; минимальный внешний контракт теперь является совместимым вычисляемым представлением над новым единственным in-memory store. Не объявлять данный этап полным переносом.
- **Causal links не выводятся из близости событий.** Субъективные знания не равны всей истории.
- **Rollback/fork не уничтожает альтернативное будущее.** Изменение ветки/контекста инвалидирует подготовленную работу, если она относится к старой реальности.
- **Не вводить новые Api/Lifecycle/Handler**, если достаточно чистой модели и адресной операции.
- **Ни один unchecked пункт не объявлять готовым.** Измерения Python не являются бенчмарками Rust.

## Проверка

- [x] `cargo test --offline` Rust 1.98.1 (локально, 2026-11-01; 1 live Gemini intentionally ignored).
- [x] `cargo clippy --offline --all-targets -- -D warnings`.
- [x] `cargo fmt --check`.
- [x] Domain parity tests на ветвление, receipts, causal, знание, reset, presentation edits (18 integration (including deterministic stress and concurrency) + 3 workspace unit + 2 compatibility unit).
- [x] SQLite integration (feature `sqlite-system`): domain parity, SQL fault injection на холодном и тёплом кеше, reopen, конкурентные записи, инвалидация кеша другим писателем, индексированные страницы, durable selection и реальный Core restart с сохранением выбранной ветки.
- [x] End-to-end тест: смена selection между `MessageHandler.prepare` и ответом провайдера **отменяет ожидание** старого ответа до разрешения провайдеру отвечать; свежий запрос успешно завершается, исходное событие остаётся без ответа.
- [x] Bench: `cargo run --offline --example timeline_append_bench`; 8k одиночных append до/после индекса ~556 / ~6.2 мкс на событие (локальный debug-профиль, не гарантия и не SQLite throughput).
- [x] Проверено, что новые timeline/domain и selection импортируют только identity/interaction и собственные низовые компоненты; инверсии `timeline -> character/request` не добавлены. Это ручная проверка направленности, не автоматическое доказательство компилятором.

## Следующий пакет изменений

Ускорить **холодный** SQLite append без полного загрузочного сканирования, сделать отдельные бенчмарки транзакционной производительности, обеспечить раннюю адресную отмену при reset и внешнем переключении, затем согласованное восстановление владельцев состояния. См. [STORAGE.md](STORAGE.md) — границы возможного общего файла SQLite для Settings и условия будущей миграции на CFMD. **CharacterHandler уже использует единый TimelineRepository (transient либо SQLite) через совместимый фасад, но не обладает всем контрактом Python TimelineRuntime.** До завершения SQLite, восстановления, архивов и Unity/Sandbox adapters **нельзя** считать Python SessionTimeline полностью перенесённым.
