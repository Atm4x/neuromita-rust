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

- [ ] Реализовать `TimelineRepository` на стороне Rust с атомарной транзакционной семантикой.
- [ ] SQLite repository, WAL, индексы, batch/receipt, durable selection, reopen/recovery.
- [ ] Перенести вложения `TimelineAssetStore` и контентно-адресуемые ссылки с верификацией.
- [ ] Версионированный `SessionArchive`: export/import, строгая проверка дерева, ссылок, receipts и assets.
- [ ] Параметризованные тесты одного доменного контракта на памяти и SQLite, включая отказ в середине транзакции.

## Этап C — MitaView / история / наблюдения

- [x] Базовый **беспобочный** `character_view`: события + момент получения знания, соблюдение reset/disclosure.
- [x] Семантические сообщения, actor/role, правки, скрытие и восстановление без изменения исходного события.
- [ ] Постраничное чтение и эквивалентность полному Python-формату провайдерских сообщений.
- [ ] Context summaries, инвалидация по branch / head / epoch / версии правил без попадания скрытых фактов в prompts.
- [ ] Историческая реконструкция `at`; выборка большого объёма без полного сканирования для каждого запроса.

## Этап D — связи с живым ядром и средами

- [x] Подключить in-memory доменный `TimelineStore` как единственный источник фактов в действующем `SessionTimeline` (`timeline/mod.rs`); старые `HistoryView`/`EventContent` стали вычисляемыми compatibility-проекциями.
- [ ] Сменить in-memory backend на транзакционный durable repository, сохранив эту единственность истории.
- [ ] Завершить согласованный `CharacterHandler.prepare`/`accept` и транзакционную приёмку state/effects.
- [x] Локальный контракт `SessionWorkspace`: selection CAS, generation, pin и validation для reset/смены ветки; **пока не подключён** к работающему Request/CharacterHandler.
- [ ] `SessionTimelineRuntime`: durable selection, pin в реальном MessageHandler, отмена токенов, полноценное восстановление перед активацией.
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
- [x] Domain parity tests на ветвление, receipts, causal, знание, reset, presentation edits (16 integration + 3 workspace unit).
- [ ] Повторно проверить граф импортов без циклов после всех изменений (Cargo сам по себе этого не доказывает).

## Следующий пакет изменений

Довести SQLite repository до той же транзакционной семантики, что и базовый доменный store, затем подключить selection/turn pins и restore через реальный `CharacterHandler`. **CharacterHandler уже использует новый in-memory store через совместимый фасад, но не обладает всем контрактом Python TimelineRuntime.** До завершения SQLite, восстановления, архивов и Unity/Sandbox adapters **нельзя** считать Python SessionTimeline полностью перенесённым.
