# #60: приоритетная приёмка Rust-native VAD

Статус: задача занята VinerX; реализация и приёмка продолжаются. Этот документ фиксирует резервирование, а не готовность issue.

База: upstream/main15710db73adea787e9ddb23a3ea2550ded96d260. Актуальный issue #60 прочитан 2026-11-03.

Уже имеется прямой Earshot1.2.2, независимый VoiceGate и Rust SpeechSegmenter, FFmpeg PCM capture adapter, PTT и SpeechInputHandler. Python нужен для ASR inference, а не для текущего Rust VAD. Приёмка существующего кода не заменяет запрошенное сравнение wavekat-vad Earshot/Silero.

План:
- Сверить единственного владельца stream/device selection, PCM, PTT, speaking gate, prebuffer, hangover, utterance identity и маршрутизации.
- Проверить reset/cancel/stop, очередь сегментов и late ASR: старый сегмент после смены stream generation не может породить реплику.
- Проверить sample rates/frame sizes и buffering score providers; Earshot/Silero остаются поставщиками score, границы фраз принадлежат Rust domain.
- Запустить собственное воспроизводимое A/B на небольших лицензированных RU/EN записях, с шумом/паузами, фиксированными параметрами и provenance. Измерить ошибки/latency, не выдавать upstream benchmark за наш.
- Optional Silero использует проверенный local model asset; TEN-VAD не включается по умолчанию. Существующие vendor bytes/licenses сохраняются.
- Детерминированные segment/gate/transcript binding tests, обе offline suites, Clippy/fmt. Реальная микрофонная и hotplug приёмка учитывается отдельным уровнем доказательств.

На Windows проверено только перечисление DirectShow устройств: видны USB и виртуальные микрофоны. Записи не делались. В repository assets/tests аудиокорпуса не найдено. Реальные RU/EN результаты, качество микрофона/GUI и optional Silero inference пока не подтверждены.

Найденный кандидат на regression: Reset/PTT отменяет active turn, но очередь SpeechSegment не имеет stream generation; уже поставленный старый сегмент может обрабатываться после reset. Требуется тест и исправление, а не утверждение о готовности по существующим mocks.
