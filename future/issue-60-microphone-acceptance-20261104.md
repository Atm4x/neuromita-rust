# #60: последний микрофонный acceptance, 2026-11-04

CLI `examples/native_vad_probe.rs` собран и проверен на конечных источниках FFmpeg.
См. [результаты](issue-60-capture-gate-20261104.md). Реальный микрофон пока не открывался.

Probe использует существующие `FfmpegCapture`, `SpeechSegmenter`, `ScoreGate` и
`CaptureBackend`. Python, ASR, Core, GUI и сеть не нужны. Проверка реальной цепочки
распознавания с ASR выполняется отдельно: корректный VAD не доказывает качество ASR.

## Запуск

Все команды выполнять из `C:/Development/NeuromitaRust/.worktrees/issue-60-current`.

```powershell
cargo test --locked --offline --example native_vad_probe
cargo clippy --locked --offline --example native_vad_probe -- -D warnings
cargo run --locked --offline --example native_vad_probe -- C:/Temp/vad-auto.json
cargo run --locked --offline --features vad-wavekat --example native_vad_probe -- C:/Temp/vad-wavekat.json
cargo run --locked --offline --features vad-silero --example native_vad_probe -- C:/Temp/vad-silero.json
```

Последние два JSON должны явно содержать соответственно `"provider":"wavekat"`
и `"provider":"silero"`. Feature сама не выбирает provider. Для Silero задать:

```powershell
$env:SILERO_MODEL_PATH = 'C:/Models/silero_vad.onnx'
$env:NEUROMITA_SILERO_MODEL = 'C:/Models/silero_vad.onnx'
$env:NEUROMITA_ORT_DLL = 'C:/Models/onnxruntime.dll'
```

Это существующие локальные файлы с абсолютными путями. Применяются проверки
размера/SHA-256 существующего score API; произвольная модель/DLL не принимается.
Автопоиска, скачивания, смены provider и подстройки threshold нет.

## Строгий JSON

Путь JSON — единственный CLI-аргумент, обязательно абсолютный. Пример полного
`C:/Temp/vad-auto.json` для **синтетики**, без открытия микрофона (путь FFmpeg заменить
своим существующим абсолютным путём):

```json
{
  "ffmpeg_binary": "C:/Tools/ffmpeg/bin/ffmpeg.exe",
  "backend": "lavfi",
  "device": "anullsrc=r=16000:cl=mono,atrim=end_sample=16384",
  "provider": "direct",
  "threshold": 0.5,
  "duration_secs": 3,
  "mode": "auto",
  "ptt_events": [],
  "stop_at_ms": null,
  "segment": {
    "pre_roll_samples": 3072,
    "start_voice_samples": 768,
    "end_silence_samples": 7168,
    "min_segment_samples": 2560,
    "max_segment_samples": 160000
  },
  "output_dir": null,
  "ptt_policy": null
}
```

Обязательны все поля, кроме nullable `stop_at_ms`, `output_dir` и `ptt_policy`.
Для режима PTT `ptt_policy` обязателен. Неизвестные поля запрещены.
`duration_secs` — целое 1..120. Threshold проверяет существующий `VoiceThreshold`
(конечное число в 0..1). Политика задана явно в samples при mono PCM16 16000 Hz;
её проверяет существующий segmenter, максимум сегмента — 160000 samples / 10 s.
Никаких скрытых policy defaults нет.

Backend строго один из `pulse|alsa|dshow|avfoundation|lavfi`, затем явно преобразуется
в существующий enum. Device — точная строка выбранного backend, не файловый путь:
например, для Windows `"backend":"dshow"`, `"device":"audio=@device_cm_..."`.
FFmpeg запускается с отдельными аргументами без shell. Device не угадывается и
не заменяется системным default. Используйте Alternative name нужного аудиоустройства
из `ffmpeg -hide_banner -list_devices true -f dshow -i dummy` с префиксом `audio=`.

Для синтетического sine заменить только device на
`"sine=frequency=440:sample_rate=16000:duration=1.024"`.
Оба источника конечные: 16384 samples, кратно 256; произвольная длительность может
закончиться неполным кадром, что capture корректно объявляет ошибкой, без padding.
Ожидание для anullsrc/auto: `finish=eof`, 16384 samples, peak 0, без сегментов.
Sine не является доказательством речи и не обязан проходить VAD в auto.

Для проверки PTT на синтетике задать `"mode":"ptt"` и:

```json
"ptt_policy": {"pre_roll_samples": 3200, "tail_samples": 2048, "min_voiced_samples": 5600},
"ptt_events": [
  {"time_ms": 0, "pressed": true},
  {"time_ms": 800, "pressed": false}
]
```

Тишина не выдаёт сегмент: policy требует минимум 350 мс кадров, признанных голосом
во время удержания или хвоста. Pre-roll до 200 мс округляется вверх до кадра захвата
(208 мс), хвост — 128 мс. После maximum удержание исчерпано до release и нового
press. Порог при этом не меняется. События строго возрастают,
чередуются начиная с press, исходное состояние released. Все события раньше duration;
auto требует пустой список, ptt — непустой. Можно оставить последнюю кнопку pressed,
чтобы проверить flush на завершении. Таймер stop_at_ms, если задан, строго между 0
и duration в миллисекундах; он и Ctrl+C дают `finish=stop`. Терминальные окна для
press/release не нужны.

## Время, ограничения и выход

Время PTT/stop/duration отсчитывается монотонно перед запуском capture. Чтение
дозируется по 256 samples / 16 ms, в том числе Lavfi; источник не прогоняется
ускоренно. Таймеры и Ctrl+C прерывают ожидание кадра, частично прочитанный кадр
сохраняется внутри capture. При совпадении duration имеет приоритет, затем stop,
Ctrl+C, PTT, кадр. PTT применяется по wall time; offsets относятся к действительно
полученному PCM, поэтому задержка/потеря устройства может разойтись с wall time.
Это не обещание жёсткой real-time точности ОС или прерывания зависшего native VAD.

Успешный EOF конечного Lavfi имеет отдельный тип `Finish::Eof`. EOF живого устройства
считается потерей входа, даже если FFmpeg завершился с кодом 0. Ошибка spawn,
чтения, неполный кадр или ненулевой FFmpeg exit дают `finish=capture_error`.
Ошибка score/segmenter даёт `provider_error`. На ошибке активный незавершённый сегмент
отбрасывается; уже выданные остаются. Автоматического restart нет. При duration/stop/
успешном EOF вызывается core `flush()`: последний сегмент получает core reason `eof`.
Это причина flush сегмента, а причину остановки всей сессии показывает `finish`.
Слишком короткие сегменты отбрасываются существующей min-policy.

Stdout содержит один JSON summary: finish/error, elapsed_ms, sample_count всего
полученного потока, peak_amplitude (абсолютная амплитуда PCM16 0..32768), число
применённых PTT-событий и список сегментов. Для каждого сегмента: sample offsets
(end exclusive), onset/emitted offsets, end_reason, sample_count и peak_amplitude.
PCM в JSON отсутствует. FFmpeg diagnostics идут в stderr. Ошибка конфигурации,
отсутствующей feature/asset или подготовки output — ненулевой exit со stderr до
запуска; capture/provider errors дают JSON и ненулевой exit. Успешный EOF/stop/duration
дают exit 0. Ошибка сохранения WAV также даёт ненулевой exit; файлы могут быть частичными.

Парсер сначала проверяет regular file и размер, затем читает максимум 65537 bytes,
отклоняя всё больше 64 KiB даже при росте файла. Этим ограничен и список событий.
Поток ограничен одновременно 120 s wall time и 1920000 принятых samples; сохраняются
только полные кадры, последние <256 samples перед deadline могут не попасть в summary.
Каждый active segment ограничен core до 10 s; число emitted сегментов ограничено
числом кадров (не более 7500), PCM хранится только до формирования отчёта. В auto
pre-roll способен повторить часть samples между сегментами, но остаётся bounded
числом кадров и max_segment_samples. В JSON не хранится покадровый лог.

По умолчанию файлы аудио не создаются. Для последнего ручного прослушивания явно
указать `"output_dir":"C:/Temp/vad-user-run-01"`: абсолютный **новый** дочерний
каталог существующего parent, без `..`. Existing path, включая symlink, отклоняется;
корень диска и несуществующий parent отклоняются. Создание без рекурсии, каталог
не переиспользуется, WAV открываются с create_new без перезаписи. Сохраняются только
emitted `segment-0000.wav` и т.д., mono PCM16 16000 Hz; сырой полный capture не пишется.
Память PCM и объём WAV ограничены duration/кадрами/политикой, а не свободным местом
диска. Запись выполняется после остановки capture. Удаление записей — вручную;
probe ничего не чистит. Summary при необходимости перенаправить отдельно.

## Ручной acceptance на реальном микрофоне

Использовать отдельный явный device config, заранее зафиксировать provider, threshold
и segment policy; одинаковые значения во всей серии. Не адаптировать threshold
по результату отдельной фразы. В этой работе реальный микрофон не открывался.

1. Обычная RU речь: несколько фраз с паузами и одним длинным высказыванием.
   Прослушать начало/конец WAV, сверить silence/maximum, пропуски и лишние сегменты.
2. Обычная EN речь: повторить, зафиксировать фактические пропуски/обрезания отдельно.
3. Тихая речь RU/EN при неизменных расстоянии и усилении: отметить услышанное
   человеком и результат gate; тишину не засчитывать как успешно распознанную речь.
4. Бытовой шум без речи: клавиатура, вентилятор, движения/стук. Зафиксировать ложные
   сегменты и уровень peak. Затем речь на таком фоне при тех же параметрах.
5. PTT: заранее задать press/release timeline, произнести фразы внутри и вне окон,
   проверить отсутствие PCM за пределами разрешённого окна с pre-roll и хвостом,
   reason release и offsets. Удержать >10 s для maximum, убедиться, что сегмент
   не повторяется до release и нового press. Отдельно проверить PTT на тишине.
6. Stop: stop_at_ms внутри теста и отдельный Ctrl+C, если есть терминал. Проверить
   finish stop, отсутствие дальнейшего capture и корректное завершение отчёта.
7. Unplug во время capture: сохранить фактический результат capture_error/eof либо
   duration (если драйвер удерживает pipe). Не объявлять duration доказательством
   детекции unplug. Убедиться, что probe завершился и устройство не перезапускается
   само. Подключить устройство и сделать **новый явный запуск** с новым output_dir;
   отдельно подтвердить восстановление sample_count/peak/сегментов.

Для каждого пункта сохранить config, команду/features, stdout/stderr, exit code,
субъективную разметку услышанных интервалов и verdict pass/fail/not run. До этой
проверки все реальные микрофонные пункты имеют статус **not run**. Синтетический
Lavfi проверяет механику capture/timeline/termination, но не качество микрофона,
RU/EN speech detection или итоговую ASR-цепочку.
