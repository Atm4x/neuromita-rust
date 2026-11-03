use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use super::{
    sandbox,
    view::{AppWindow, SettingsUi, VoiceUi},
};
use neuromita::character::character_catalog;
use slint::{ComponentHandle, ModelRc, VecModel};

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

pub fn bind(window: &AppWindow) {
    let ui = window.global::<VoiceUi>();
    let draft = Rc::new(RefCell::new(BTreeMap::<i32, slint::SharedString>::new()));
    let drafts = draft.clone();
    ui.on_voice_id_changed(move |character, value| {
        drafts.borrow_mut().insert(character, value);
    });
    let weak_selection = window.as_weak();
    ui.on_select_character(move |character| {
        if let Some(window) = weak_selection.upgrade() {
            let ui = window.global::<VoiceUi>();
            draft
                .borrow_mut()
                .insert(ui.get_character(), ui.get_voice_id());
            let value = draft.borrow().get(&character).cloned().unwrap_or_default();
            ui.set_character(character);
            ui.set_voice_id(value);
        }
    });
    let weak = window.as_weak();
    ui.on_open_engine_settings(move || {
        if let Some(window) = weak.upgrade() {
            window.global::<SettingsUi>().set_section(6);
        }
    });
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
    ui.set_sample_text("Привет! Я Мита. Давай проверим, как звучит мой голос.".into());
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
