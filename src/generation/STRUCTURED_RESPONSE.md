# Structured response v3 — текущий срез

**Референс:** Python `src/schemas/structured_response.py`, `RESPONSE_PROTOCOL_VERSION = 3`, ревизия `5f3d061f523d9abefa6a264bb2953048a935fa65`. Реализован первый этап #19; **полная parity ещё не достигнута**.

## Контракт

- Один `StructuredResponse` (v3) и упорядоченные `ResponseSegment`. Схема содержит поля segments, reasoning, image_description, изменения состояния, памяти, напоминаний, knowledge graph, tool_call, custom_fields, working_state. Интенты/аргументы принимают объект или JSON-строку, как в Python Gemini.
- `GenerationProvider` теперь возвращает **типизированный** `StructuredResponse`, а не сырой текст. Общий `ResponseGenerator` проверяет ответ и формирует `ProposedResponse { structured, text, effects }`.
- Gemini отправляет `responseMimeType: application/json` и `responseSchema` в `generationConfig`; декодер исключает thought-parts, требует finishReason=STOP и строго разбирает JSON в v3.
- `structured_message_gemini.json` — срез **разрешённой сегодня** схемы, выведенный через Python `StructuredResponse.gemini_schema_dict(exclude_fields=..., exclude_segment_fields=...)`. В нём доступны `segments[].text`, `segments[].emotions` и опциональные `attitude_change` / `boredom_change` / `stress_change`. Эмоции — данные сегмента, не выполнение анимаций или других действий. Протокол данных v3 и урезанная схема конкретного запроса — разные понятия.
- Контракт ошибок намеренно строже терпимого Python-парсера: никакой автоматической починки JSON, markdown-парсинга или превращения невалидного ответа в текст. Неподдерживаемые поля/команды (memory, tool calls, game actions и др.) отклоняются **до** принятия ответа в Timeline и до TTS.
- `reasoning` допускается схемой *входного* DTO для совместимости, но **не запрашивается** у Gemini и не выводится пользователю/Timeline. Нельзя включать его в пользовательские логи.

## Что ещё предстоит

- Следующий набор вариантов `ResponseEffect` пока не реализован: memory, tool calls, game actions, напоминания, рабочее состояние, отдельная доставка позиционных команд и адресатов. Для них нужен собственный исполняющий контракт, а не общий event bus.
- Полная capability-dependent генерация схем для доступных исполнителей и кастомных параметров; Python/Rust golden fixture parity.
- Sparse response, flexible Python parser, работа с tool calls, мемори, внутренним состоянием и UI, безопасные retries; всё это остаётся в #19 и соответствующих Issues.
- Первый типизированный эффект: `ResponseEffect::ChangeCharacterState(CharacterStateDelta)` формируется только из валидированного `StructuredResponse`; это **предложение**, не мутация. `CharacterHandler` при приёмке материализует `AppliedEffect::CharacterStateChanged(CharacterBehavior)` и передаёт вместе с Message в один `append_guarded` batch. Отдельный `CharacterStateChanged` содержит **абсолютное принятое состояние** и ссылку `CausedBy` на сообщение. В SQLite это одна транзакция. `AcceptedResponse.effects` содержит принятые значения, `state_after` — подтверждённое состояние. Нет второго mutable state store.
- Состояния `Working → Accepting → Applied/Discarded` принадлежат **целому пакету запроса**, а не каждому типу `ResponseEffect`. Отмена побеждает только до `begin_accepting()`. После фиксации текста и эффектов `mark_applied()` вызывается до TTS; отмена во время TTS отвечает `TooLate`. Ошибки/остановка TTS отражаются в `VoiceGenerationFailed` с уже принятым `AcceptedResponse`; не изменяют `EffectPhase::Applied`. При отмене до приёмки или stale turn нет ни сообщения, ни эффекта.
- В первом срезе применяются default шкалы Python: исходное состояние (60/10/5), per-turn hard limit ±6, округление до 2 знаков и итоговые границы [0,100]. **Конфигурируемые пределы из Python пока не перенесены**, их следует хранить вместе с принятой операцией, не переписывая исторические значения; текущие абсолютные факты уже защищают от ретроактивного пересчёта.
- В Timeline `CharacterMessage.content` теперь содержит массив типизированных сегментов. Provider/UI-проекция сохраняет JSON-массив `content`, `ProjectedMessage.content` и legacy history выводят производный текст. AcceptedResponse также несёт сегменты; `reasoning`, инструменты и изменения состояния не становятся содержимым сообщения. Не считать это исполнением всех полей Python v3.

## Проверки

Unit tests Rust DTO и bounded state policy, Gemini wire, запрет неисполняемых действий; вертикальные HTTP-тесты типизированных segments и первого применяемого эффекта, stale-turn rollback, reset, historical state и SQLite reopen. Базовые наборы с SQLite/без SQLite, Rustfmt и Clippy.
