# VAD evaluation corpus

18 коротких клипов 16 кГц моно PCM16 для офлайн A/B (`examples/vad_ab.rs`).

- `ru-01..06.wav`, `en-01..06.wav` — google/fleurs, split `validation`, лицензия
  CC BY 4.0. По шесть клипов 4–10 секунд на язык; первые три в манифесте помечены
  `calibration`, остальные `heldout`.
- `noise-01..06.wav` — ESC-10 (подмножество karolpiczak/ESC-50), лицензия CC BY 3.0:
  dog, rooster, rain, sea waves, crackling fire, clock tick.

`sources.json` хранит исходные id, транскрипты, размер и SHA-256 каждого файла.

Интервалы речи в `manifest.json` получены энергетическим детектором и помечены
`provisional_intervals: true`. Это не ручная разметка: метрики границ и F1 по таким
меткам предварительные. Отсутствие речи в шумовых клипах прослушиванием не
подтверждалось.
