# PromptCatalog → DEBUG_DISPLAY → Characters GUI

Дата: 2026-11-05.

`SettingsUi.character-prompt-set` уже связан с функциональным выбором `PromptSetId` в `settings::characters`, а `SelectPromptSet` проверяет выбор через каталог. Кнопки Reload / Open Folder работают через Presenter/Runtime. Новая секция «Переменные персонажа» показывает реальные текущие значения, а её подписанные поля берёт из `DEBUG_DISPLAY` выбранного PostDSL. При отсутствии секции используются Python fallback-поля Attitude/Boredom/Stress, а для Crazy дополнительно Secret Exposed/FSM State.

Цепочка: `SessionTimeline` (read-only prefix) → `CharacterHandle.debug_variables` → `CharactersPresenter.debug_display` + `PostDslProgram.debug_values` → `DesktopRuntime::CharacterDebug` → `SettingsUi.character-debug-values`. Обычное отображение не исполняет PostDSL, не делает LLM-запросов и не пишет в Timeline. Переход на другого персонажа инициирует отдельный UI command; изменения state и settings обновляют read-model. Ошибки выбранного набора показываются в секции.

## Файловая согласованность

`main_template.txt` остаётся оркестратором: LOAD/RUN читают включения из того же принятого `PromptSetSnapshot`. Снимок набора и PostDSL фиксируются при admission, а `CharacterContext` вычисляется при execution. Если внешний редактор перезаписывает файлы **непосредственно во время** захвата снимка, чтение нескольких файлов не является файловой транзакцией. Для строгой атомарности сторонний редактор должен публиковать завершённую версию директории атомарным rename/swap или каталог должен поддерживать явно публикуемые версии/manifest. Простая проверка двух проходов по файлам уменьшает вероятность смешивания, но не обеспечивает строгого гарантированного snapshot. Пока это документированное ограничение ручного редактирования файлов; формат Prompt Set не менялся и от пользователя не требуется manifest.