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

## Runtime ContextCapture (следующий функциональный пакет)

- `SessionTimeline::capture_context(character, position)` фиксирует единый `ContextAnchor` до публикации нового входа. `MitaView` и временная legacy `HistoryView` вычисляются над одним и тем же immutable prefix. Снимки не смещаются автоматически при будущих append.
- `CharacterHandler::context` берёт снимок под собственным mailbox и локальным Timeline mutex, вычисляет `CharacterState` для того же prefix, после чего публикует вход и получает `TimelineTurn`. Текущий вход подаётся в генерацию отдельно и не дублируется в истории.
- `CharacterContext.view` — предметная read-model для провайдера; `history` временно оставлена только для существующих API/тестов. Счётчик принятых ответов теперь читается из канонических CharacterMessage с валидным request_id независимо от reset, hide или потери персонажем знания; это ветвевой snapshot, а не отдельный изменяемый источник истины. `CharacterView.view` предоставляет такую же проекцию при чтении исторического состояния через API.
- Gemini формирует контекст из `view.messages()`, а не из `HistoryView`. `User/Assistant` соответствуют `user/model`; `SessionNote` отправляется как **user-level** сведение, а не повышается до привилегированной `systemInstruction`. Лишь доверенные description/prompt и счётчик формируют системную инструкцию.
- В Gemini поддерживаются текстовые сегменты массива `content`; изображения и иные медиа-поля пока не кодируются в provider parts. Схема мультимодальности всё ещё требует отдельного контракта.
- Защита от внешней смены selection/reset на приёмке остаётся за `append_guarded`; локальная блокировка **не** является транзакцией между SQLite, внешними писателями и mutable CharacterState. Durable CharacterState и межпроцессная ранняя отмена остаются открытыми.

## Следующие изменения

1. Завершить `ContextCapture`: durable revision mutable CharacterState и explicit context policy по request cause/очереди; локальный prefix-capture уже подключён к `CharacterHandler` и Gemini.
2. Версионная инвалидация и ограниченный кеш для live MitaView, без второго источника истины.
3. Python/UI schema parity, время, page/resizing и адаптеры потребителей (ChatAPI, Unity, Launcher); отделить UI history от provider context.
4. Отдельная ранняя адресная отмена при reset/внешней смене мира; это не обязанность read-проекции.

## Контекст реакции и производное состояние (2026-11-01)

- Для обычной генерации (`ContextPosition::Latest`) `CharacterHandler` захватывает знания персонажа непосредственно в начале обработки работы из очереди. `reply`, `react`, `initiate` используют **всю** доступную на этот момент историю: причина реакции определяет causal link, а не фильтр истории. Поздние сообщения и наблюдения не скрываются лишь потому, что появились после `react_to`. При явном `ContextPosition::At` ограничение задаётся самим запрошенным историческим снимком.
- `SessionTimeline::validate_reaction_context` проверяет, что причина `react(event_id)` реально существовала и была известна персонажу на **захваченном** prefix. При несовпадении подготовка отклоняется **до** публикации нового входа. Эта проверка не меняет read-side.
- Входные `PlayerMessage` из ожидающих ChatAPI-запросов сейчас публикуются только при исполнении `CharacterHandler::context`, а не в момент admission. Поэтому ожидающий запрос не автоматически становится известным другим генерациям; для полного сценария с несколькими ожидающими входами потребуется отдельный ingress контракта сообщений.
- `CharacterState.accepted_replies/revision` для текущей узкой модели являются производными от канонических принятых `CharacterMessage` (actor совпадает, request_id валиден), а не от `HistoryView`. Reset, hide и чужие персонажи не изменяют счётчик. Значение сохраняется при SQLite reopen и относится к конкретной ветви/позиции; перед accept локальный владелец повторно читает его.
- Счётчик по-прежнему вычисляется проходом по каноническим событиям, а не индексированной агрегатной проекцией. **Не** реализован durable mutable `CharacterState`, межпроцессный единый read transaction MitaView+state и атомарность изменений внешних participants. Эти пункты остаются открытыми.
- Тесты проверяют обе реализации хранилища, приватные знания, реакцию на старую причину со всеми известными последующими репликами, мировые сведения, исторический cutoff, отказ неизвестной в указанной позиции причины, reset/reopen и раздельность состояний персонажей.
