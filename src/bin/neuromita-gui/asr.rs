use std::rc::Rc;

use neuromita::{
    ai_runtime::speech_catalog::SpeechInventory, audio::input_catalog::AudioInputDevice,
    settings::AsrPreferences,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{
    runtime::Command,
    view::{AppWindow, AsrUi, SettingsUi},
};

fn model_id(label: &str) -> String {
    label.split_once(" · ").map_or(label, |(id, _)| id).into()
}

fn millis(value: &str, field: &str) -> Result<u32, String> {
    let number: f64 = value
        .trim()
        .replace(',', ".")
        .parse()
        .map_err(|_| format!("Некорректное значение: {field}"))?;
    if !number.is_finite() || !(0.0..=600.0).contains(&number) {
        return Err(format!("Значение {field} вне диапазона"));
    }
    Ok((number * 1000.0).round() as u32)
}

fn language_id(name: &str) -> String {
    match name {
        "Русский" => "ru",
        "English" => "en",
        "Автоматически" => "auto",
        other => other,
    }
    .into()
}
fn language_name(id: &str) -> String {
    match id {
        "ru" => "Русский",
        "en" => "English",
        "auto" => "Автоматически",
        other => other,
    }
    .into()
}
fn language_capability(id: &str) -> (&'static [&'static str], &'static str, &'static str) {
    match id {
        "gigaam" | "gigaam_onnx" => (
            &["Русский"],
            "ru",
            "Для GigaAM доступен только русский язык",
        ),
        "mock_asr" => (
            &["Русский", "English"],
            "ru",
            "Языки определены схемой mock ASR",
        ),
        "google" => (
            &[],
            "ru-RU",
            "Выбор языка недоступен: схема Google ASR не содержит вариантов",
        ),
        "whisper" | "whisper_onnx" => (
            &[],
            "ru",
            "Варианты языка этой модели настраиваются в AI Engine",
        ),
        _ => (
            &[],
            "",
            "У выбранной модели нет объявленных вариантов языка",
        ),
    }
}

fn mode_id(name: &str) -> String {
    match name {
        "Рация — включать кнопкой" => "radio",
        "Слушать постоянно" => "continuous",
        "Нажать для разговора" => "push_to_talk",
        other => other,
    }
    .into()
}
fn mode_name(id: &str) -> String {
    match id {
        "radio" => "Рация — включать кнопкой",
        "continuous" => "Слушать постоянно",
        "push_to_talk" => "Нажать для разговора",
        other => other,
    }
    .into()
}

fn read(window: &AppWindow) -> Result<(AsrPreferences, String), String> {
    let ui = window.global::<AsrUi>();
    let model = model_id(&ui.get_recognizer());
    let options = AsrPreferences {
        recognizer_id: model,
        device_id: ui.get_device().into(),
        audio_backend: ui.get_audio_backend().into(),
        instant_send: ui.get_instant_send(),
        delayed_send: ui.get_delayed_send(),
        merge_chat: ui.get_merge_chat(),
        pause_while_speaking: ui.get_pause_while_speaking(),
        send_delay_ms: millis(&ui.get_send_delay(), "Пауза отправки")?,
        input_mode: mode_id(&ui.get_input_mode()),
        model_languages: Default::default(),
        chunk_size: ui
            .get_chunk_size()
            .parse()
            .map_err(|_| "Некорректный размер чанка")?,
        vad_threshold_milli: {
            let value = millis(&ui.get_vad_threshold(), "Порог VAD")?;
            value.try_into().map_err(|_| "Некорректный порог VAD")?
        },
        silence_ms: millis(&ui.get_silence_timeout(), "Порог тишины")?,
        pre_buffer_ms: millis(&ui.get_pre_buffer(), "Предбуфер")?,
        max_speech_ms: millis(&ui.get_max_speech(), "Максимальная речь")?,
        min_speech_ms: millis(&ui.get_min_speech(), "Минимальная речь")?,
    };
    Ok((options, language_id(&ui.get_language())))
}

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>) {
    let ui = window.global::<AsrUi>();
    ui.set_draft_enabled(true);
    ui.set_runtime_ready(false);
    ui.set_active(false);
    ui.set_status("Настройки доступны; микрофон не запущен".into());
    ui.set_languages(ModelRc::new(VecModel::from(vec![
        "Русский".into(),
        "English".into(),
        "Автоматически".into(),
    ])));
    ui.set_input_modes(ModelRc::new(VecModel::from(vec![
        "Рация — включать кнопкой".into(),
        "Слушать постоянно".into(),
        "Нажать для разговора".into(),
    ])));
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_persist(move || {
        if let Some(window) = weak.upgrade() {
            match read(&window) {
                Ok((settings, language)) => send(Command::SaveAsr(settings, language)),
                Err(error) => window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into()),
            }
        }
    });
    let send = dispatch.clone();
    ui.on_recognizer_changed(move |value| send(Command::SelectAsrRecognizer(model_id(&value))));
    let send = dispatch.clone();
    ui.on_refresh_models(move || send(Command::RefreshSpeechInventory));
    ui.on_refresh_devices(move || dispatch(Command::RefreshSpeechInventory));
}

pub fn render(window: &AppWindow, settings: &AsrPreferences) {
    let ui = window.global::<AsrUi>();
    let chosen = ui
        .get_recognizers()
        .iter()
        .find(|entry| model_id(entry) == settings.recognizer_id);
    ui.set_recognizer(chosen.unwrap_or_else(|| settings.recognizer_id.clone().into()));
    ui.set_device(settings.device_id.clone().into());
    ui.set_audio_backend(settings.audio_backend.clone().into());
    ui.set_instant_send(settings.instant_send);
    ui.set_delayed_send(settings.delayed_send);
    ui.set_merge_chat(settings.merge_chat);
    ui.set_pause_while_speaking(settings.pause_while_speaking);
    ui.set_send_delay(format!("{:.2}", settings.send_delay_ms as f64 / 1000.0).into());
    ui.set_input_mode(mode_name(&settings.input_mode).into());
    let (languages, default_language, hint) = language_capability(&settings.recognizer_id);
    ui.set_languages(ModelRc::new(VecModel::from(
        languages
            .iter()
            .map(|value| (*value).into())
            .collect::<Vec<_>>(),
    )));
    ui.set_language(
        language_name(
            settings
                .model_languages
                .get(&settings.recognizer_id)
                .map(String::as_str)
                .unwrap_or(default_language),
        )
        .into(),
    );
    ui.set_language_hint(hint.into());
    ui.set_sample_rate("16000".into());
    ui.set_chunk_size(settings.chunk_size.to_string().into());
    ui.set_vad_threshold(format!("{:.2}", f64::from(settings.vad_threshold_milli) / 1000.0).into());
    ui.set_silence_timeout(format!("{:.2}", settings.silence_ms as f64 / 1000.0).into());
    ui.set_pre_buffer(format!("{:.2}", settings.pre_buffer_ms as f64 / 1000.0).into());
    ui.set_max_speech(format!("{:.2}", settings.max_speech_ms as f64 / 1000.0).into());
    ui.set_min_speech(format!("{:.2}", settings.min_speech_ms as f64 / 1000.0).into());
    ui.set_runtime_ready(false);
    ui.set_active(false);
}

pub fn inventory(window: &AppWindow, inventory: &SpeechInventory, devices: &[AudioInputDevice]) {
    let ui = window.global::<AsrUi>();
    ui.set_recognizers(ModelRc::new(VecModel::from(
        inventory
            .asr
            .iter()
            .map(|entry| {
                format!(
                    "{} · {} ({})",
                    entry.id,
                    entry.name,
                    entry.installation_label()
                )
                .into()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_devices(ModelRc::new(VecModel::from(
        devices
            .iter()
            .map(|device| device.id.clone().into())
            .collect::<Vec<_>>(),
    )));
    let mut backends = devices
        .iter()
        .map(|device| device.backend.as_str())
        .collect::<Vec<_>>();
    backends.sort_unstable();
    backends.dedup();
    if backends.is_empty() {
        #[cfg(target_os = "windows")]
        backends.push("dshow");
        #[cfg(target_os = "linux")]
        backends.extend(["pulse", "alsa"]);
        #[cfg(target_os = "macos")]
        backends.push("avfoundation");
    }
    ui.set_audio_backends(ModelRc::new(VecModel::from(
        backends.into_iter().map(Into::into).collect::<Vec<_>>(),
    )));
    ui.set_status("Настройки доступны; микрофон не запущен".into());
    ui.set_monitored_device(if devices.is_empty() {
        "Доступные устройства не обнаружены".into()
    } else {
        "Выберите устройство ввода".into()
    });
    ui.set_draft_enabled(true);
    ui.set_runtime_ready(false);
}

pub fn preview(window: &AppWindow) {
    window.global::<SettingsUi>().set_section(5);
    let ui = window.global::<AsrUi>();
    ui.set_draft_enabled(true);
    ui.set_runtime_ready(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asr_languages_follow_original_model_schemas() {
        assert_eq!(language_capability("gigaam").0, &["Русский"]);
        assert_eq!(language_capability("google").0, &[] as &[&str]);
        assert_eq!(language_capability("google").1, "ru-RU");
        assert_eq!(language_capability("mock_asr").0, &["Русский", "English"]);
    }

    #[test]
    fn seconds_are_converted_to_milliseconds() {
        assert_eq!(millis("0,50", "VAD").unwrap(), 500);
        assert!(millis("NaN", "VAD").is_err());
        assert!(millis("-1", "VAD").is_err());
    }
}
