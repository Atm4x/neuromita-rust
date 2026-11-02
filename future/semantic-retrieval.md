# Semantic retrieval: рабочий план

- [x] Объединить PR #55–58 с актуальным сервером и schema 2; исправить первый config launcher.
- [ ] Проверить интеграцию и отправить объединённый main; подтвердить merged для всех PR.
- [ ] Независимый EmbeddingProvider и реальный Gemini HTTP adapter с явным протоколом/моделью.
- [ ] Производный индекс памяти из зафиксированного CharacterContext/Timeline prefix, без второго владельца памяти.
- [ ] Корректность Update/Delete/Merge, fork/historical reads, rebuild и отмены; ограниченный cache без stale results.
- [ ] Подключить semantic ranking к генерации, сохранить явный lexical режим и лимиты контекста.
- [ ] Сравнить качество и стоимость с lexical baseline; тесты, документация, финальный commit/push.

Источник истины — принятые факты Timeline. Индекс получает точный набор воспоминаний персонажа на выбранном префиксе; membership не определяется содержимым cache. Изменение текста требует нового embedding, удалённые записи не возвращаются в результат, исторический запрос использует свои версии записей. Внешний backend не вызывается под lock Timeline/Character/cache. Ошибка embeddings не включает скрытый fallback.
