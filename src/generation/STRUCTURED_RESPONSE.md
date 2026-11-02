# Structured response v3 — текущий срез

**Референс:** Python `src/schemas/structured_response.py`, `RESPONSE_PROTOCOL_VERSION = 3`, ревизия `5f3d061f523d9abefa6a264bb2953048a935fa65`. Реализован первый этап #19; **полная parity ещё не достигнута**.

## Контракт

- Один `StructuredResponse` (v3) и упорядоченные `ResponseSegment`. Схема содержит поля segments, reasoning, image_description, изменения состояния, памяти, напоминаний, knowledge graph, tool_call, custom_fields, working_state. Интенты/аргументы принимают объект или JSON-строку, как в Python Gemini.
- `GenerationProvider` теперь возвращает **типизированный** `StructuredResponse`, а не сырой текст. Общий `ResponseGenerator` проверяет ответ и формирует `ProposedResponse { structured, text, effects }`.
- Gemini отправляет `responseMimeType: application/json` и `responseSchema` в `generationConfig`; декодер исключает thought-parts, требует finishReason=STOP и строго разбирает JSON в v3.
- `structured_message_gemini.json` — срез **разрешённой сегодня** схемы, выведенный через Python `StructuredResponse.gemini_schema_dict(exclude_fields=..., exclude_segment_fields=...)`. В нём доступны `segments[].text` и `segments[].emotions`. Эмоции — данные сегмента, не выполнение анимаций или других действий. Протокол данных v3 и урезанная схема конкретного запроса — разные понятия.
- Контракт ошибок намеренно строже терпимого Python-парсера: никакой автоматической починки JSON, markdown-парсинга или превращения невалидного ответа в текст. Неподдерживаемые поля/команды отклоняются **до** принятия ответа в Timeline и до TTS.
- `reasoning` допускается схемой *входного* DTO для совместимости, но **не запрашивается** у Gemini и не выводится пользователю/Timeline. Нельзя включать его в пользовательские логи.

## Что ещё предстоит

- Не применять действия до реализации последовательных стадий proposed → accepted → applied и их владельцев; отдельная доставка `segments` с позиционными командами, адресатом и аудио.
- Полная capability-dependent генерация схем для доступных исполнителей и кастомных параметров; Python/Rust golden fixture parity.
- Sparse response, flexible Python parser, работа с tool calls, мемори, внутренним состоянием и UI, безопасные retries; всё это остаётся в #19 и соответствующих Issues.
- В Timeline `CharacterMessage.content` теперь содержит массив типизированных сегментов. Provider/UI-проекция сохраняет JSON-массив `content`, `ProjectedMessage.content` и legacy history выводят производный текст. AcceptedResponse также несёт сегменты; `reasoning`, инструменты и изменения состояния не становятся содержимым сообщения. Не считать это исполнением всех полей Python v3.

## Проверки

Unit tests Rust DTO, Gemini wire, запрет неисполняемых действий; vertical_slice HTTP fake Gemini + отсутствие принятия event при нежелательных commands; базовые SQLite/без SQLite suites и Clippy.
