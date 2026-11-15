# NeuroMita MCP ↔ GUI parity: source-based audit (Cycle 9)

## Метод
Источник: `src/bin/neuromita-gui/runtime.rs`, enum `Command` из локального SHA `8f1b6688975ffdf01ad5e21574ed98226f872b37` (PR #446). Извлечены 68 уникальных вариантов enum. Каждый рассмотрен по имеющимся инструментам `src/bin/neuromita-agent-mcp.rs` и `src/presentation/ipc*.rs`.

Правило **полностью**: основному агенту доступно функционально эквивалентное действие MCP/IPC через реальный Presenter или Core с той же наблюдаемой семантикой. **Частично**: покрыт лишь элемент поведения, либо обход без эквивалентной валидации/повторной операции. **Нет**: эквивалентной команды MCP не обнаружено. Готовность инструмента **не** является прохождением Unity E2E.

## Измерение: 25/68 = 36.8% полного покрытия GUI Command surface
Частично: 11/68 (16.2%). Отсутствует: 32/68 (47.1%).

| Контур | Всего | Готово | Частично | Нет |
|---|---:|---:|---:|---:|
| Chat/Conversation | 11 | 7 | 3 | 1 |
| Memory/Knowledge | 6 | 2 | 0 | 4 |
| Presets/Characters | 13 | 5 | 6 | 2 |
| Audio/Voice/ASR/TTS | 14 | 5 | 0 | 9 |
| AI Hub / Native | 8 | 0 | 0 | 8 |
| Telegram/RemoteVoice | 5 | 0 | 0 | 5 |
| Settings/Launcher | 11 | 6 | 2 | 3 |

## Полная инвентаризация 68 GUI-команд

| GUI Command | Область | MCP-покрытие |
|---|---|---|
| `SelectSpeechModel` | Audio/Voice/ASR/TTS | нет |
| `RefreshReleases` | Settings/Launcher | полностью |
| `MemorySelect` | Memory/Knowledge | полностью |
| `MemoryEdit` | Memory/Knowledge | полностью |
| `MemoryRecall` | Memory/Knowledge | нет |
| `GraphPreview` | Memory/Knowledge | нет |
| `GraphApply` | Memory/Knowledge | нет |
| `GraphAbort` | Memory/Knowledge | нет |
| `Preview` | Chat/Conversation | полностью |
| `Confirm` | Chat/Conversation | полностью |
| `Abort` | Chat/Conversation | полностью |
| `Diagnostics` | Chat/Conversation | частично |
| `NewPreset` | Presets/Characters | полностью |
| `UpdatePreset` | Presets/Characters | нет |
| `DeletePreset` | Presets/Characters | полностью |
| `MovePreset` | Presets/Characters | полностью |
| `ActivePreset` | Presets/Characters | полностью |
| `AssignPreset` | Presets/Characters | частично |
| `AssignAllPresets` | Presets/Characters | частично |
| `SelectPromptSet` | Presets/Characters | частично |
| `DebugSelect` | Presets/Characters | частично |
| `SaveCharacterProfile` | Presets/Characters | полностью |
| `RetryCharacterProfileSave` | Presets/Characters | частично |
| `RefreshPromptCatalog` | Presets/Characters | частично |
| `OpenPromptFolder` | Presets/Characters | нет |
| `VoiceInLauncher` | Audio/Voice/ASR/TTS | полностью |
| `VoiceInGame` | Audio/Voice/ASR/TTS | полностью |
| `VoiceName` | Audio/Voice/ASR/TTS | полностью |
| `SaveTts` | Audio/Voice/ASR/TTS | нет |
| `TelegramConnect` | Telegram/RemoteVoice | нет |
| `TelegramCode` | Telegram/RemoteVoice | нет |
| `TelegramPassword` | Telegram/RemoteVoice | нет |
| `TelegramDisconnect` | Telegram/RemoteVoice | нет |
| `RemoteVoice` | Telegram/RemoteVoice | нет |
| `SaveCharacterVoice` | Audio/Voice/ASR/TTS | нет |
| `SaveAsr` | Audio/Voice/ASR/TTS | нет |
| `SelectAsrRecognizer` | Audio/Voice/ASR/TTS | нет |
| `RefreshSpeechInventory` | Audio/Voice/ASR/TTS | нет |
| `RefreshNativeModels` | AI Hub / Native | нет |
| `SelectNativeRevision` | AI Hub / Native | нет |
| `ActivateNativeModel` | AI Hub / Native | нет |
| `StopNativeModel` | AI Hub / Native | нет |
| `InstallNativePackage` | AI Hub / Native | нет |
| `CancelNativePackageInstall` | AI Hub / Native | нет |
| `PreviewAiHubInstall` | AI Hub / Native | нет |
| `ConfirmAiHubInstall` | AI Hub / Native | нет |
| `RefreshHardware` | Settings/Launcher | частично |
| `SectionVisibility` | Settings/Launcher | частично |
| `HidePrivate` | Settings/Launcher | полностью |
| `StructuredDisplay` | Settings/Launcher | полностью |
| `ChangeGuide` | Settings/Launcher | полностью |
| `RetryGuideChange` | Settings/Launcher | нет |
| `SetToolOption` | Settings/Launcher | нет |
| `SetToolDepth` | Settings/Launcher | нет |
| `SetMemoryDedupEnabled` | Settings/Launcher | полностью |
| `SetMemoryDedupThreshold` | Settings/Launcher | полностью |
| `Select` | Chat/Conversation | частично |
| `Send` | Chat/Conversation | полностью |
| `AttachFile` | Chat/Conversation | нет |
| `Cancel` | Chat/Conversation | полностью |
| `Retry` | Chat/Conversation | полностью |
| `Refresh` | Chat/Conversation | частично |
| `LoadOlder` | Chat/Conversation | полностью |
| `Volume` | Audio/Voice/ASR/TTS | полностью |
| `SpeechSessionEnabled` | Audio/Voice/ASR/TTS | нет |
| `ChatSpeechToggle` | Audio/Voice/ASR/TTS | нет |
| `SpeechPushToTalk` | Audio/Voice/ASR/TTS | нет |
| `MicrophoneTest` | Audio/Voice/ASR/TTS | полностью |

## Дополнительные оговорки
1. Это знаменатель **68 команд GUI runtime**, а не всех публичных Presenter/GUI методов. Для полной parity нужна следующая инвентаризация `GuiPresenter`, `src/presentation/*.rs`, binders, Slint callbacks и Launcher.
2. `UpdatePreset` отсутствует: имеющиеся MCP tools могут создать/переместить/удалить запись, но не настроить соединение/API credential через `PresetsPresenter::update_connection`. Приоритет №1.
3. Native AI Hub имеет 8 явных GUI-команд в этом списке и пока не эквивалентен MCP; приоритет №2 — подключить существующий `AiHubNativeService` как shared owner, а не делать второй worker manager.
4. ASR/TTS и Telegram/RemoteVoice из 68 команд составляют большой остаток; приоритет №3 — строгие операции через те же owners, без фиктивной готовности модели.
5. `SectionVisibility`, `SelectPromptSet` и точечные назначения пресетов отнесены к «частично», поскольку общий профиль/настройки не доказывают ту же валидацию, что штатная GUI-команда. `Abort` засчитан как эквивалент отказа от применения preview, а не отдельная операция изменения Core.
6. **Аудит не влит в PR и сам не добавляет MCP-инструментов**. Это проверяемая измерительная база для следующих циклов.