use super::view::{AppWindow, AsrUi, SettingsUi};
use slint::{ComponentHandle, ModelRc, VecModel};

pub fn bind(window: &AppWindow) {
    let ui = window.global::<AsrUi>();
    ui.set_runtime_ready(false);
    ui.set_draft_enabled(false);
}

pub fn preview(window: &AppWindow) {
    window.global::<SettingsUi>().set_section(5);
    let ui = window.global::<AsrUi>();
    ui.set_draft_enabled(true);
    ui.set_runtime_ready(false);
    ui.set_active(false);
    ui.set_recognizers(ModelRc::new(VecModel::from(vec![
        "Google Speech Recognition".into(),
        "GigaAM (demo)".into(),
        "Vosk (demo)".into(),
    ])));
    ui.set_recognizer("Google Speech Recognition".into());
    ui.set_devices(ModelRc::new(VecModel::from(vec![
        "Микрофон (демонстрация)".into(),
        "Виртуальный вход (демонстрация)".into(),
    ])));
    ui.set_device("Микрофон (демонстрация)".into());
    ui.set_audio_backends(ModelRc::new(VecModel::from(vec![
        "PortAudio".into(),
        "WASAPI".into(),
    ])));
    ui.set_audio_backend("PortAudio".into());
    ui.set_status("Не инициализировано (демо)".into());
    ui.set_instant_send(false);
    ui.set_delayed_send(true);
    ui.set_merge_chat(true);
    ui.set_pause_while_speaking(false);
    ui.set_send_delay("3.0".into());
    ui.set_input_modes(ModelRc::new(VecModel::from(vec![
        "Рация — включать кнопкой".into(),
        "Слушать постоянно".into(),
        "Нажать для разговора".into(),
    ])));
    ui.set_input_mode("Рация — включать кнопкой".into());
    ui.set_languages(ModelRc::new(VecModel::from(vec![
        "Русский".into(),
        "English".into(),
        "Автоматически".into(),
    ])));
    ui.set_language("Русский".into());
    ui.set_language_hint("Язык выбран для демонстрационной модели".into());
    ui.set_sample_rate("16000".into());
    ui.set_chunk_size("512".into());
    ui.set_vad_threshold("0.50".into());
    ui.set_silence_timeout("0.60".into());
    ui.set_pre_buffer("0.40".into());
    ui.set_max_speech("30.0".into());
    ui.set_min_speech("0.35".into());
    ui.set_mic_level(0);
    ui.set_monitored_device("Микрофон (демонстрация) · тест не запущен".into());
}
