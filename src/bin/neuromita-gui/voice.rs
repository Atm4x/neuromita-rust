use std::rc::Rc;

use super::runtime::Command;
use super::{
    sandbox,
    view::{AppWindow, SettingsUi, VoiceUi},
};
use neuromita::{
    ai_runtime::{
        AiService,
        runtime_status::RuntimeSnapshot,
        speech_catalog::{REMOTE_TTS_TEMPLATES, SpeechInventory},
    },
    character::character_catalog,
    remote_voice::{RemoteVoiceConfig, RemoteVoicePreset},
    settings::{TtsBackend, TtsPreferences},
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoicePreviewMode {
    Telegram,
    Local,
    Api,
}

impl VoicePreviewMode {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "telegram" => Ok(Self::Telegram),
            "local" => Ok(Self::Local),
            "api" => Ok(Self::Api),
            _ => Err("--voice-preview expects telegram, local or api".into()),
        }
    }

    const fn index(self) -> i32 {
        match self {
            Self::Telegram => 0,
            Self::Local => 1,
            Self::Api => 2,
        }
    }
}

fn model_id(label: &str) -> String {
    label
        .split_once(" · ")
        .map_or(label, |(id, _)| id)
        .to_owned()
}

fn api_profile_id(label: &str) -> &str {
    REMOTE_TTS_TEMPLATES
        .iter()
        .find(|template| template.name == label)
        .map_or(label, |template| template.id)
}

fn current_preset(ui: &VoiceUi) -> Result<RemoteVoicePreset, String> {
    let id: String = ui.get_api_preset_id().into();
    if id.is_empty() {
        return Err("Облачный профиль ещё не загружен".into());
    }
    let voices = ui.get_character_voice_ids().iter().collect::<Vec<_>>();
    let character_voice_ids = neuromita::character::CharacterId::ALL
        .into_iter()
        .enumerate()
        .filter_map(|(index, character)| {
            voices
                .get(index + 1)
                .map(|value| (character, value.to_string()))
        })
        .filter(|(_, value)| !value.is_empty())
        .collect();
    Ok(RemoteVoicePreset {
        id,
        name: ui.get_api_preset_name().into(),
        template_id: "fish_audio".into(),
        api_key: ui.get_api_key().into(),
        model: ui.get_api_model().into(),
        speed_percent: u16::try_from(ui.get_api_speed())
            .map_err(|_| "Некорректная скорость".to_owned())?,
        default_voice_id: voices.first().map(ToString::to_string).unwrap_or_default(),
        character_voice_ids,
    })
}

fn read(window: &AppWindow) -> Result<TtsPreferences, String> {
    let ui = window.global::<VoiceUi>();
    let mut value = TtsPreferences {
        enabled: ui.get_enabled(),
        backend: match ui.get_backend() {
            0 => TtsBackend::Telegram,
            1 => TtsBackend::Local,
            2 => TtsBackend::Api,
            _ => return Err("Неизвестный TTS backend".into()),
        },
        telegram_auto_connect: ui.get_telegram_auto_connect(),
        telegram_service: ui.get_telegram_service().into(),
        telegram_wait_seconds: ui
            .get_telegram_wait()
            .parse()
            .map_err(|_| "Некорректное время ожидания Telegram")?,
        telegram_interval_seconds: ui
            .get_telegram_interval()
            .parse()
            .map_err(|_| "Некорректный интервал Telegram")?,
        local_model_id: model_id(&ui.get_local_model()),
        local_language: ui.get_local_language().into(),
        local_auto_load: ui.get_local_auto_load(),
        local_lazy_init: ui.get_local_lazy_init(),
        api_profile: api_profile_id(&ui.get_api_profile()).to_owned(),
        api_model: ui.get_api_model().into(),
        api_speed_percent: ui
            .get_api_speed()
            .try_into()
            .map_err(|_| "Некорректная скорость TTS")?,
        default_voice_id: String::new(),
        character_voice_ids: Default::default(),
    };
    if value.local_model_id == "Выберите модель" {
        value.local_model_id.clear();
    }
    value.validate_for_ui()?;
    Ok(value)
}

trait ValidateTtsUi {
    fn validate_for_ui(&self) -> Result<(), String>;
}
impl ValidateTtsUi for TtsPreferences {
    fn validate_for_ui(&self) -> Result<(), String> {
        if self.telegram_wait_seconds == 0
            || self.telegram_wait_seconds > 600
            || self.telegram_interval_seconds > 600
            || !(50..=200).contains(&self.api_speed_percent)
        {
            return Err("Значения TTS вне допустимого диапазона".into());
        }
        Ok(())
    }
}

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>) {
    let ui = window.global::<VoiceUi>();
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_persist(move || {
        if let Some(window) = weak.upgrade() {
            match read(&window) {
                Ok(value) => {
                    send(Command::SaveTts(value));
                    let ui = window.global::<VoiceUi>();
                    if ui.get_backend() == 2 && !ui.get_api_preset_id().is_empty() {
                        match current_preset(&ui) {
                            Ok(preset) => send(Command::RemoteVoice(
                                super::runtime::RemoteVoiceAction::Save(preset),
                            )),
                            Err(error) => window
                                .global::<SettingsUi>()
                                .set_settings_error(error.into()),
                        }
                    }
                }
                Err(error) => window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into()),
            }
        }
    });
    let weak = window.as_weak();
    ui.on_toggle_enabled(move |_| {
        if let Some(window) = weak.upgrade() {
            window.global::<VoiceUi>().invoke_persist();
        }
    });
    let send = dispatch.clone();
    let voice_window = window.as_weak();
    ui.on_voice_id_changed(move |character, value| {
        let id = if character == -1 {
            Some(None)
        } else {
            usize::try_from(character)
                .ok()
                .and_then(|index| neuromita::character::CharacterId::ALL.get(index))
                .copied()
                .map(Some)
        };
        let Some(window) = voice_window.upgrade() else {
            return;
        };
        let Some(id) = id else {
            window
                .global::<SettingsUi>()
                .set_settings_error("Invalid character selection".into());
            return;
        };
        let ui = window.global::<VoiceUi>();
        if ui.get_backend() == 2 && !ui.get_api_preset_id().is_empty() {
            let mut voices = ui
                .get_character_voice_ids()
                .iter()
                .collect::<Vec<SharedString>>();
            if let Ok(row) = usize::try_from(character + 1) {
                if let Some(entry) = voices.get_mut(row) {
                    *entry = value.clone();
                }
            }
            ui.set_character_voice_ids(ModelRc::new(VecModel::from(voices)));
            ui.set_voice_id(value);
            match current_preset(&ui) {
                Ok(preset) => send(Command::RemoteVoice(
                    super::runtime::RemoteVoiceAction::Save(preset),
                )),
                Err(error) => window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into()),
            }
        } else {
            send(Command::SaveCharacterVoice(id, value.into()));
        }
    });
    let weak = window.as_weak();
    ui.on_select_character(move |character| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<VoiceUi>();
            let value = usize::try_from(character + 1)
                .ok()
                .and_then(|index| ui.get_character_voice_ids().row_data(index))
                .unwrap_or_default();
            ui.set_character(character);
            ui.set_voice_id(value);
        }
    });
    let weak = window.as_weak();
    let send = dispatch.clone();
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_api_profile_save(move || {
        if let Some(window) = weak.upgrade() {
            match current_preset(&window.global::<VoiceUi>()) {
                Ok(preset) => send(Command::RemoteVoice(
                    super::runtime::RemoteVoiceAction::Save(preset),
                )),
                Err(error) => window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into()),
            }
        }
    });
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_api_profile_select(move |label| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let ui = window.global::<VoiceUi>();
        let ids = ui.get_api_preset_ids();
        let index = ui.get_api_profiles().iter().position(|name| name == label);
        if let Some(id) = index.and_then(|index| ids.row_data(index)) {
            if let Ok(preset) = current_preset(&ui) {
                send(Command::RemoteVoice(
                    super::runtime::RemoteVoiceAction::Save(preset),
                ));
            }
            send(Command::RemoteVoice(
                super::runtime::RemoteVoiceAction::Select(id.into()),
            ));
        }
    });
    let send = dispatch.clone();
    ui.on_api_profile_add(move || {
        send(Command::RemoteVoice(super::runtime::RemoteVoiceAction::Add))
    });
    let send = dispatch.clone();
    ui.on_api_profile_delete(move || {
        send(Command::RemoteVoice(
            super::runtime::RemoteVoiceAction::Delete,
        ))
    });
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_voice_test(move |text, character| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        if let Ok(preset) = current_preset(&window.global::<VoiceUi>()) {
            send(Command::RemoteVoice(
                super::runtime::RemoteVoiceAction::Save(preset),
            ));
        }
        let target = usize::try_from(character)
            .ok()
            .and_then(|index| neuromita::character::CharacterId::ALL.get(index))
            .copied();
        send(Command::RemoteVoice(
            super::runtime::RemoteVoiceAction::Preview(text.into(), target),
        ));
    });
    let send = dispatch.clone();
    ui.on_voice_test_cancel(move || {
        send(Command::RemoteVoice(
            super::runtime::RemoteVoiceAction::Cancel,
        ))
    });
    ui.on_refresh_models(move || {
        if weak.upgrade().is_some() {
            send(Command::RefreshSpeechInventory);
        }
    });
    let weak = window.as_weak();
    ui.on_open_engine_settings(move || {
        if let Some(window) = weak.upgrade() {
            window.global::<SettingsUi>().set_section(6);
        }
    });
}

pub fn render(window: &AppWindow, settings: &TtsPreferences) {
    let ui = window.global::<VoiceUi>();
    ui.set_enabled(settings.enabled);
    ui.set_backend(match settings.backend {
        TtsBackend::Telegram => 0,
        TtsBackend::Local => 1,
        TtsBackend::Api => 2,
    });
    ui.set_telegram_auto_connect(settings.telegram_auto_connect);
    ui.set_telegram_service(settings.telegram_service.clone().into());
    ui.set_telegram_wait(settings.telegram_wait_seconds.to_string().into());
    ui.set_telegram_interval(settings.telegram_interval_seconds.to_string().into());
    let local = ui
        .get_local_models()
        .iter()
        .find(|entry| model_id(entry) == settings.local_model_id);
    if let Some(label) = &local {
        if let Some((_, status)) = label.rsplit_once(" (") {
            ui.set_local_status(status.trim_end_matches(')').into());
        }
    } else if !settings.local_model_id.is_empty() {
        ui.set_local_status("Модель не обнаружена в каталоге".into());
    } else {
        ui.set_local_status("Модель не выбрана".into());
    }
    ui.set_local_model(local.unwrap_or_else(|| settings.local_model_id.clone().into()));
    ui.set_local_language(settings.local_language.clone().into());
    ui.set_local_auto_load(settings.local_auto_load);
    ui.set_local_lazy_init(settings.local_lazy_init);
    if ui.get_api_preset_id().is_empty() {
        let ids = (-1..neuromita::character::CharacterId::ALL.len() as i32)
            .map(|id| {
                if id == -1 {
                    settings.default_voice_id.clone()
                } else {
                    settings
                        .character_voice_ids
                        .get(&neuromita::character::CharacterId::ALL[id as usize])
                        .cloned()
                        .unwrap_or_default()
                }
                .into()
            })
            .collect::<Vec<_>>();
        ui.set_character_voice_ids(ModelRc::new(VecModel::from(ids)));
        let current = ui.get_character();
        ui.set_voice_id(
            if current == -1 {
                settings.default_voice_id.clone()
            } else {
                usize::try_from(current)
                    .ok()
                    .and_then(|index| neuromita::character::CharacterId::ALL.get(index))
                    .and_then(|id| settings.character_voice_ids.get(id))
                    .cloned()
                    .unwrap_or_default()
            }
            .into(),
        );
    }
}

pub fn inventory(window: &AppWindow, models: &SpeechInventory) {
    let ui = window.global::<VoiceUi>();
    ui.set_local_models(ModelRc::new(VecModel::from(
        models
            .tts
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
    let chosen = model_id(&ui.get_local_model());
    ui.set_local_status(
        models
            .tts
            .iter()
            .find(|entry| entry.id == chosen)
            .map(|entry| entry.installation_label().to_owned())
            .unwrap_or_else(|| "Модель не выбрана".into())
            .into(),
    );
    ui.set_local_languages(ModelRc::new(VecModel::from(vec![
        "ru".into(),
        "en".into(),
        "auto".into(),
    ])));
    ui.set_telegram_status("Telegram не подключён".into());
    ui.set_preview_available(false);
}

pub fn preview(window: &AppWindow, mode: VoicePreviewMode) {
    sandbox::render_catalog(window, character_catalog().collect());
    window.global::<SettingsUi>().set_section(3);
    let ui = window.global::<VoiceUi>();
    ui.set_backend(mode.index());
    ui.set_enabled(false);
    ui.set_telegram_service("@silero_voice_bot".into());
    ui.set_telegram_wait("12".into());
    ui.set_telegram_interval("2".into());
    ui.set_telegram_status("Telegram не подключён".into());
    ui.set_local_models(ModelRc::new(VecModel::from(vec![
        "Edge-TTS + RVC (CUDA)".into(),
        "Edge-TTS".into(),
    ])));
    ui.set_local_model("Edge-TTS + RVC (CUDA)".into());
    ui.set_local_languages(ModelRc::new(VecModel::from(vec!["ru".into(), "en".into()])));
    ui.set_local_language("ru".into());
    ui.set_local_status("Требуется инициализация".into());
    ui.set_api_profiles(ModelRc::new(VecModel::from(vec!["Fish Audio".into()])));
    ui.set_api_profile("Fish Audio".into());
    ui.set_api_models(ModelRc::new(VecModel::from(vec!["s2.1-pro-free".into()])));
    ui.set_api_model("s2.1-pro-free".into());
    ui.set_api_speed(100);
    ui.set_character(0);
    ui.set_voice_id("demo-voice-identifier".into());
}

pub fn runtime_status(window: &AppWindow, snapshot: &RuntimeSnapshot) {
    let text = super::speech_status::describe(&snapshot.service(AiService::Tts));
    window.global::<VoiceUi>().set_runtime_status(text.into());
}

#[cfg(test)]
mod tests {
    use super::VoicePreviewMode;

    #[test]
    fn preview_modes_are_explicit() {
        assert_eq!(
            VoicePreviewMode::parse("api").unwrap(),
            VoicePreviewMode::Api
        );
        assert_eq!(
            VoicePreviewMode::parse("local").unwrap(),
            VoicePreviewMode::Local
        );
        assert_eq!(
            VoicePreviewMode::parse("telegram").unwrap(),
            VoicePreviewMode::Telegram
        );
        assert!(VoicePreviewMode::parse("other").is_err());
    }
}

pub fn render_remote(window: &AppWindow, config: &RemoteVoiceConfig) {
    let ui = window.global::<VoiceUi>();
    let names = config
        .presets
        .iter()
        .map(|p| format!("{} ({})", p.name, &p.id[..8]).into())
        .collect::<Vec<SharedString>>();
    let ids = config
        .presets
        .iter()
        .map(|p| p.id.clone().into())
        .collect::<Vec<SharedString>>();
    let selected = config
        .presets
        .iter()
        .position(|p| p.id == config.active_id)
        .unwrap_or(0);
    let Some(preset) = config.presets.get(selected) else {
        return;
    };
    let label = names.get(selected).cloned().unwrap_or_default();
    ui.set_api_profiles(ModelRc::new(VecModel::from(names)));
    ui.set_api_preset_ids(ModelRc::new(VecModel::from(ids)));
    ui.set_api_profile(label);
    ui.set_api_preset_id(preset.id.clone().into());
    ui.set_api_preset_name(preset.name.clone().into());
    ui.set_api_key(preset.api_key.clone().into());
    ui.set_api_model(preset.model.clone().into());
    ui.set_api_speed(i32::from(preset.speed_percent));
    ui.set_api_models(ModelRc::new(VecModel::from(
        REMOTE_TTS_TEMPLATES[0]
            .models
            .iter()
            .copied()
            .map(Into::into)
            .collect::<Vec<SharedString>>(),
    )));
    let voices = std::iter::once(preset.default_voice_id.clone())
        .chain(
            neuromita::character::CharacterId::ALL
                .into_iter()
                .map(|id| {
                    preset
                        .character_voice_ids
                        .get(&id)
                        .cloned()
                        .unwrap_or_default()
                }),
        )
        .map(Into::into)
        .collect::<Vec<SharedString>>();
    ui.set_character_voice_ids(ModelRc::new(VecModel::from(voices.clone())));
    let chosen = usize::try_from(ui.get_character() + 1).unwrap_or(0);
    ui.set_voice_id(voices.get(chosen).cloned().unwrap_or_default());
}

pub fn remote_status(window: &AppWindow, message: &str, busy: bool) {
    let ui = window.global::<VoiceUi>();
    ui.set_remote_status(message.into());
    ui.set_remote_preview_busy(busy);
}
