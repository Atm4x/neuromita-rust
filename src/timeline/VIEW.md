# MitaView — предметная проекция и read-side

## Источники

- Историческая концепция: https://github.com/NeuroMita/NeuroMita-Unity/issues/366 (2026-09-18).
- Python reference: `NeuroMita/session-timeline`, `domain/timeline/projections.py` и `services/timeline_history_queries.py` на `5f3d061f`.
- Текущий источник истины — Rust `TimelineRepository` с immutable `TimelineFact`, а не Python/Unity history.

Issue #366 определяет **смысловые события**, границы знаний персонажа, причинность и сохраняемые ветви. Его пример маршрута Unity → Python → Timeline — исторический, а не обязательный runtime-топологический контракт Rust.

## Границы ответственности

```text
Unity / Sandbox / Games ──(typed semantic inputs)──> application operations
                                                      │
                                                      ▼
                                               TimelineRepository
                                                      │
                         ┌────────────────────────────┴────────────────────────┐
                         ▼                                                     ▼
                domain::CharacterMessageProjection                   other domain projections
                         │
                         ▼
                timeline::view::MitaView
                         │
                         ▼
                 provider/UI document adapter
```

- `domain::CharacterMessageProjection` — deterministic fold по `KnownEvent`. Домен отвечает за знание, reset, видимость, ревизии, роль и идентичность сообщения. Не знает про SQLite, HTTP, Python-сервисы или формат ChatAPI.
- `TimelineMessageQueries` — read-side orchestrator поверх одного `TimelineRepository`: фиксирует branch/head, читает только direct witnesses небольшими страницами, адресно разрешает ссылки `EventObserved`, передаёт домену знания в порядке получения. Не записывает events или mutable CharacterState.
- `MitaView` — независимый snapshot через `branch` + `through`, с `messages`, `provider_messages`, `page(limit, offset)`, `count`, `contains`. Копии результатов detached; чтение исторического `at` не привязывает результат к текущему head.
- Provider/UI map — **адаптер** `timeline::view`, а не модель доменного события. Сохранены произвольные вложенные поля `payload.message` и типизированный JSON `content`, включая мультимодальные массивы.

## Контракты и ограничения

1. Visibility одного персонажа не раскрывается другому. Поздняя disclosure использует исходный immutable факт, но `learned_at` ставит временем раскрытия. Повторное знание не дублирует сообщение.
2. События `MessageHidden` / `MessageRevised` / `MessageRestored` воздействуют только на уже известные данному персонажу сообщения. Reset очищает read-проекцию, но не историю мира.
3. Сортировка сообщений по `(learned_at, sequence)`, `page` — последние `limit` видимых сообщений с пропуском `offset` с конца, результат по возрастанию. Страница `0` — пустая. Изменения старых событий не переписывают source facts.
4. `through > head` — ошибка; legacy `HistoryView` продолжает работать отдельно до согласованной миграции всех consumers. Снимки через `capture` detached, последующий append их не меняет.
5. Read-side **не кеширует** свёрнутый view между вызовами и пока воспроизводит все видимые по истории direct-witness факты на каждый capture. Поэтому bounded storage reads **не равны** O(page size) общей стоимости, особенно для страниц из конца истории.
6. Полного равенства Python provider schema пока нет: Python вычисляет локальный строковый `time` из исходного ISO datetime. Rust хранит UTC Unix milliseconds и не подставляет отсутствующее поле `time`. Изменение поведения времени — явная отдельная задача адаптера, не доменной модели.
7. `capture` фиксирует immutable prefix, но **не** включает snapshot mutable CharacterState. Его согласованность с состоянием персонажа должна обеспечиваться отдельной операцией захвата контекста.

## Тесты

`cargo test --locked --offline --test timeline_mitaview`

`cargo test --locked --offline --features sqlite-system --test timeline_mitaview`

Проверяются reset и relearn, приватность, поздняя disclosure, edit/hide/restore и metadata, мультимодальный content, несколько страниц источника, fork cutoff и исторические снимки на обоих backends.

## Следующие изменения

1. Согласованный `ContextCapture`: read-side snapshot + immutable CharacterState revision, explicit context policy по request cause и наблюдаемым событиям.
2. Версионная инвалидация и ограниченный кеш для live MitaView, без второго источника истины.
3. Python/UI schema parity, время, page/resizing и адаптеры потребителей (ChatAPI, Unity, Launcher); отделить UI history от provider context.
4. Отдельная ранняя адресная отмена при reset/внешней смене мира; это не обязанность read-проекции.
