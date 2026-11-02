# NeuroMita Rust — текущая цель

Обновление после интеграции PR #55–58: все четыре merged в main, вместе с исправлениями ядра и системой миграций. Semantic retrieval реализован как отдельный opt-in срез: Gemini 001/2 embedding HTTP, bounded derived cache, точное cosine-ранжирование и синтетические fixture-тесты. Живое качество модели и тарифная стоимость не подтверждены; см. [semantic-retrieval](future/semantic-retrieval.md).

**Актуальный срез:** 2026-11-02. **Базовая ревизия перед текущим проходом:** `8994f8d` (`codex/typed-long-term-memory-20261102`); текущий проход — ограниченный отбор воспоминаний и точная дедупликация. Этот документ — рабочий план, а не утверждение о полной Python parity.

## Новая согласованная цель: отмена ответа во время TTS для всех клиентов

Уточнение пользователя от 2026-11-02 заменяет целевой порядок принятия до TTS. Ревизия `ae0f9ca` использовала старый порядок; нижний перечень реализованного описывает именно его. Ядро теперь реализует отмену во время TTS; прежний порядок ниже относится к ae0f9ca. Визуальный frontend ещё не реализован.

- [ ] Единый контракт генерационного запроса для Sandbox и внешних клиентов: RequestId, очередь, наблюдение этапов, предварительный текст, отмена и конечный исход. Sandbox — встроенный клиент через presenter, без обязательного сетевого транспорта; административная панель остаётся отдельной поверхностью.
- [ ] Показывать стриминг/готовый LLM-текст как предварительное сообщение. Во время подготовки озвучки — видимый индикатор по обводке сообщения; оно ещё не считается окончательным ответом.
- [ ] Разрешать отмену всего запроса во время TTS: остановить синтез, убрать предварительное сообщение с понятным визуальным завершением (например, распадом на частицы), не принимать сообщение и предложенные изменения состояния/памяти и не исполнять клиентские команды.
- [x] Перенести короткую атомарную границу принятия после подготовки озвучки, если она требуется. После победы acceptance отмена результата возвращает TooLate. Ошибка TTS с рабочим текстом и пользовательская отмена — разные исходы.
- [ ] Разделить принятие сообщения ядром, доставку клиенту и фактическое исполнение команд. Готовность аудио не подтверждает доставку; отправка команды не подтверждает её исполнение.
- [x] Проверить одинаковую семантику отмены в Sandbox и сетевом клиенте, гонку cancel с завершением TTS, отсутствие эффектов/команд отменённого ответа и сохранность уже принятого результата при ошибке доставки.

Подробный целевой контракт и граница с текущей реализацией: [предварительный ответ, TTS и отмена](future/request-delivery-and-cancellation.md).

## Текущий приоритет: самостоятельный Generation Core (без Unity)

1. [x] Каноническая SessionTimeline: общий `Event` / `Message`, MitaView и прямое знание наблюдённых событий, SQLite, `Reply` на Message / `React` на любой известный Event / `Initiate` без цели.
2. [x] Отдельная публикация Message, единая подготовка LLM-истории, типизированные настройки персонажей и presenter bindings.
3. [x] `ChatApi` без пресета; назначение `Active | Specific(PresetId)` по персонажу и операции; **единственное разрешение и материализация пресета перед генерацией**.
4. [x] **#16, базовый persistent catalog**: независимые определения, CRUD, постоянное хранение без API-ключей в JSON, presenter и проверка после restart. Полный пользовательский редактор/авторизация/другие протоколы остаются в #16.
5. [~] **#19: StructuredResponse v3**: типизированный DTO, Gemini JSON Schema (текст, эмоции, три delta-поля состояния), строгая проверка; сегменты сохраняются как JSON одного Message. Первое применяемое действие оформлено как `ResponseEffect::ChangeCharacterState` → `AppliedEffect::CharacterStateChanged`: validation → `EffectPhase::Accepting` → guarded atomic append Message + CharacterStateChanged → `EffectPhase::Applied` → TTS. `AcceptedResponse` содержит материализованные эффекты и производное состояние. Отмена после начала принятия — TooLate, сбой TTS не откатывает коммит. Рабочее состояние теперь поддерживает `ResponseEffect::ReplaceWorkingState` → `AppliedEffect::CharacterWorkingStateReplaced`: bounded snapshot атомарно пишется с ответом, воспроизводится по Timeline и явно очищается пустым объектом или CharacterContextReset. Базовые долгосрочные воспоминания теперь поддерживаются: `ResponseEffect::MutateMemory` разрешается в атомарные `AppliedEffect::CharacterMemoryChanged`, с устойчивыми UUID, приоритетом, Add/Update/Delete/Merge и исторической проекцией. Выбор для LLM-контекста ограничен 12 записями/6000 Unicode-символами, с детерминированным lexical ranking. Добавлена нормализация точных дубликатов Add без создания новых ID и запрет Update/Merge exact collisions. Полная Python memory parity (RAG/embeddings/islands/TTL/утилиты) и инструменты/игровые действия остаются открытыми.
6. [ ] #18: prompt sets / переменные / DSL; #16: transforms, альтернативные протоколы; #17: streaming/retry.
7. [ ] Функциональная проверка end-to-end генерации, настроек, сохранения и восстановления без Unity. Unity C# и выбор игровых свидетелей сейчас вне scope.

## Границы и инварианты

- `SessionTimeline` — единственный источник событий, но **не** настроек, пресетов или секретов. Рассказ Y о событии X даёт слушателю знание Y, но не X.
- `Character` не владеет назначениями пресетов. `AppSettings.presets.active` содержит только активный ID; `AppSettings.characters` — назначения по операциям; **определения пресетов принадлежат отдельному каталогу**.
- Очередь хранит команду, а не выбранную модель. После выхода из очереди исполнитель ровно один раз материализует `GenerationPreset` и передаёт его в `GenerationRequest::new`. Уже выполняемый запрос не перечитывает каталог.
- `SettingsApi` предоставляет типизированные `snapshot/edit`; `SettingsPresenter` — свойства `get/set/observe`. Не вводить универсальный строковый путь.
- Ошибки конфигурации явные; нельзя молча подменять отсутствующий пользовательский PresetId активным. Секреты не сохраняются в файле с определениями пресетов.
- Не объявлять полную готовность на основании наличия структур или единичных happy-path тестов. SQLite/без SQLite, Rustfmt и Clippy проверять после изменений.

Техническое описание: [src/generation/PRESETS.md](src/generation/PRESETS.md). Исторический подробный план Timeline: [src/timeline/GOAL.md](src/timeline/GOAL.md). Бэклог: [#16](https://github.com/Atm4x/neuromita-rust/issues/16), [#19](https://github.com/Atm4x/neuromita-rust/issues/19).

### Реализация текущего прохода: каталог (частично)

- [x] Независимые типизированные `PresetRecord` / Gemini definition и стабильные IDs; CRUD + отдельно подключаемое JSON-хранилище без секретов.
- [x] `PresetApi` как отдельный API редактирования каталога, без зависимости от `ChatApi` и `Character`.
- [x] Включить постоянный каталог в обычный CLI bootstrap и предоставить CRUD/observe через presenter; проверить сохранение назначений и определений при перезапуске.
- [ ] Настоящий графический редактор пресетов, безопасное хранение/ввод credentials помимо env refs, модельный discovery и другие протоколы. Runtime-only bootstrap ID 0 сохраняется; обычный CLI восстанавливает Timeline из timeline.sqlite3 в data-dir.

Срез #19: [src/generation/STRUCTURED_RESPONSE.md](src/generation/STRUCTURED_RESPONSE.md). Отбор памяти: [src/generation/MEMORY_RETRIEVAL.md](src/generation/MEMORY_RETRIEVAL.md).

### Следующий приоритет после текущего среза

- [x] Типизированная замена рабочего когнитивного контекста (`working_state`) без второго mutable store, с общим acceptance и контекстом следующей генерации.
- [x] Базовая долгосрочная память: стабильные UUID, приоритет, Add/Update/Delete/Merge, атомарные эффекты, исторический branch-prefix read model и контекст следующей генерации. `CharacterContextReset` очищает knowledge/working state, но оставляет личные длительные воспоминания; полное забывание — явный Delete.
- [x] Ограниченное лексическое извлечение памяти для контекста (не семантический RAG), приоритеты + устойчивый tie-break, точная дедупликация Add и atomic collision rejection при Update/Merge. Полная проекция Timeline не обрезается.
- [ ] Долговременная память, расширенная parity: Python numeric IDs, entities/tags, island upserts, RAG/vector lookup, TTL/auto-forget, **semantic** duplicate policies и UI CRUD; сравнить lifecycle/reset с реальными требованиями приложения.
- [ ] Crash recovery фазы Accepting и подтверждение outcome для уже принятого RequestId; полные executor contracts для внешних действий.

## Исправления архитектурного аудита

См. [учёт изменений](future/architecture-fixes-20261102.md): общая валидация фактов/пакетов, закрытый TurnProposal, правила Character/Memory, чтение одного префикса без нового TimelineReadSnapshot, bundled SQLite и восстановление памяти при обычном запуске. Отмена TTS через presenter и TCP реализована; frontend-индикатор и анимация остаются планом.


## Semantic retrieval (новый срез)

- [x] Независимый EmbeddingProvider и Gemini 001/2 HTTP adapters; явные модели, task/protocol и cache-key space.
- [x] Асинхронный retrieval от зафиксированной проекции Timeline без второго canonical store и блокировок во время HTTP.
- [x] Корректное членство при изменении, удалении, merge, fork/history и пересоздании cache; lexical явно сохраняется.
- [x] Синтетическое сравнение качества и числа embedding-запросов; HTTP fixture tests в обоих профилях.
- [x] Опциональный live benchmark harness: единый lexical/semantic shortlist, проверка размеченного корпуса, Hit@1/Hit@3, latency, request/text/cache counters; синтетический offline fixture. См. `src/generation/SEMANTIC_BENCHMARK.md`.
- [ ] Реальные измерения на представительном корпусе с явно предоставленным Gemini API key, точные billing tokens/USD, GUI/пресеты и расширенные RAG/knowledge graph операции.

Технический контракт: [src/generation/SEMANTIC_RETRIEVAL.md](src/generation/SEMANTIC_RETRIEVAL.md).
