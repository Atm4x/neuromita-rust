# Session Timeline: границы хранения (SQLite → CFMD)

Статус 2026-11-01: SQLite-адаптер реализован в feature branch; эта заметка отделяет **реальные гарантии** от плана оптимизации и будущих миграций.

## Архитектура

```text
ApplicationLifecycle ── TimelineConfig ──────────────┐
                                                    ▼
MessageHandler → CharacterHandler → SessionTimeline (compatibility view)
                                                    │
                                                    ▼
                                     dyn TimelineRepository + Send
                                         │                   │
                                         ▼                   ▼
                                    TimelineStore    SqliteTimelineRepository
                                   [in-memory]         [sqlite-system]
                                                            │
                                                    timeline_* tables
                                                    (WAL + transactions)

SessionWorkspace -> TimelineSelectionRepository -> durable CAS (sqlite-system)

SettingsLifecycle -> SettingsRepository -> [Memory / JSON / future SQLite / future CFMD]
```

`TimelineRepository` — **не универсальная база**. Это набор предметных операций `create_session`, `branch`, `event`, `world`, `read`, `append`, `fork`, `view`, `knows`. `TimelineSelectionRepository` отдельно хранит выбранную ветку и CAS-generation. Доменные правила один раз реализованы в `TimelineStore`; SQLite адаптер под транзакцией `BEGIN IMMEDIATE` читает подтверждённое состояние, применяет те же правила и атомарно публикует изменившуюся ветку, события и receipt. Идемпотентный повтор проверяется до требования свежего `expected_head`.

Для `timeline` собственная структура таблиц `timeline_branches`, `timeline_events`, `timeline_observers`, `timeline_receipts`, `timeline_active_selection`, `timeline_schema`. Выбор ветки по поколениям **не** кодируется посредством SQLite rowid / data_version / транзакционного revision — это отдельная семантика Core.

При явной `TimelineConfig::Sqlite { database, session, root }` рабочий Core пишет непосредственно в единственный SQLite repository; при повторном запуске с теми же IDs история сохраняется. `TimelineConfig::Transient` соответствует старому режиму и **не** подменяет собой SQLite при ошибках. `ApplicationLifecycle::start` возвращает ошибку, если база недоступна или имеет неподдерживаемую версию схемы. По умолчанию `ApplicationConfig::minimal()` задаёт Transient для старых тестовых сценариев.

## Settings и общая физическая БД

Часть Settings **может** жить в том же файле SQLite, если её собственный `SettingsRepository` получит отдельный адаптер и отдельную `settings_*` схему. Это не делает Timeline владельцем настроек, не объединяет lifecycle и не даёт двум доменам общий глобальный SQL-менеджер. В идеале один файл — разные логические репозитории, собственные инварианты и явные транзакционные области. Когда появится реальная операция, требующая одновременного commit настроек и timeline, сначала утверждается её *доменная* атомарность, и только затем проектируется координация транзакций.

## Условия замены SQLite на CFMD

CFMD-адаптер реализует **те же** предметные контракты и обязан подтвердить: атомарные batch/receipt/head, точную повторную выдачу receipt после продвижения head, причино-следственную допустимость в унаследованном префиксе, изоляцию несовместимых будущих веток, snapshot-consistent read, CAS selection, durability и недопущение кросс-веточного раскрытия приватных фактов. CFMD revision не заменяет `BranchId`/`EventId`/head sequence/selection generation. Settings отдельно перейдут через свой repository контракт. Перенос обеих частей в один физический движок не означает соединения их предметных API.

## Осознанные ограничения текущего адаптера

- `sqlite-system` **opt-in**. Он использует системную SQLite 3 C-библиотеку через изолированный FFI в `storage/sqlite.rs`, без добавления случайных сетевых зависимостей в offline-vendor. Для Windows-release потребуется поддерживаемый способ поставки SQLite DLL или замена FFI на аудированный vendored binding. Сборка здесь подтверждена только на Linux с системной SQLite; сборка Windows не проверена.
- Путь файла и IDs root/session задаются явно. Вложенные директории не создаются скрыто.
- `read` для прямых фактов SQL-индексированный и постраничный, но `view`/`knows` пока реконструируют состояние целиком. `append` при каждом вызове загружает доменное состояние хранилища, чтобы **не дублировать** семантические правила в SQLite. Накладные расходы растут с историей; для production этот путь следует заменить на доказанную локальную валидацию/инкрементальную materialization, сохраняя один семантический контракт.
- Сохраняются timeline-события и receipts, **не** mutable Character state / эффектов LLM, не assets, не SessionArchive, не внешнее состояние Unity. Общая атомарность Timeline + Character state и crash recovery пока открыты.
- Порт `TimelineSelectionRepository` есть, но живые `MessageHandler` ещё не получают pin. Ручное переключение ветки во время запроса пока не должно считаться поддержанным runtime-сценарием.
- Включение SQLite не означает миграции пользовательской Python `Histories/world.db`: схемы Python и Rust различаются. Нужен отдельный проверяемый импорт или transfer archive.

## Проверки

`cargo test --locked --offline --features sqlite-system` и `cargo clippy --locked --offline --all-targets --features sqlite-system -- -D warnings` на Rust 1.98.1. Плюс базовый тест без feature. Проверяются воспроизводимость семантики на in-memory и SQLite, SQL rollback в середине записи, два независимых writer-соединения, долговременный selection CAS, постраничные чтения через ветки и фактический shutdown/restart Core с сохранённой историей.
