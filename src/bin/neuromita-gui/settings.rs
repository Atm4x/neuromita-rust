use super::{
    runtime::Command,
    view::{AppWindow, PresetItem, SettingsUi},
};
use neuromita::{
    character::CharacterId,
    generation::{GeminiPresetDefinition, PresetDefinition, PresetId, PresetRecord},
    presentation::{GuiPresenter, PresetSlot},
    settings::{PersistenceState, PresetSelection, SettingsSnapshot},
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{rc::Rc, sync::Arc};

pub struct AdministrationSnapshot {
    pub settings: Arc<SettingsSnapshot>,
    pub presets: Vec<(PresetId, PresetRecord)>,
}

pub fn execute(
    api: &GuiPresenter,
    command: Command,
) -> Result<Option<(PresetId, uuid::Uuid)>, String> {
    match command {
        Command::CreatePreset(record, token) => {
            return api
                .presets
                .create(record)
                .map(|id| Some((id, token)))
                .map_err(|e| e.to_string());
        }
        Command::UpdatePreset(id, record) => {
            api.presets.update(id, record).map_err(|e| e.to_string())?
        }
        Command::DeletePreset(id) => api.presets.delete(id).map_err(|e| e.to_string())?,
        Command::ActivePreset(id) => {
            api.presets.require(id).map_err(|e| e.to_string())?;
            api.settings
                .presets
                .active
                .set(id)
                .map_err(|e| e.to_string())?;
        }
        Command::AssignPreset(character, operation, selection) => {
            if let PresetSelection::Specific(id) = selection {
                api.presets.require(id).map_err(|e| e.to_string())?;
            }
            let settings = api.settings.characters.for_character(character);
            let target = match operation {
                PresetSlot::Reply => &settings.reply,
                PresetSlot::React => &settings.react,
                PresetSlot::Initiate => &settings.initiate,
            };
            target.set(selection).map_err(|e| e.to_string())?;
        }
        Command::VoiceInLauncher(value) => api
            .settings
            .voice
            .in_launcher
            .set(value)
            .map_err(|e| e.to_string())?,
        Command::VoiceInGame(value) => api
            .settings
            .voice
            .in_game
            .set(value)
            .map_err(|e| e.to_string())?,
        Command::VoiceName(value) => api
            .settings
            .voice
            .voice
            .set(value)
            .map_err(|e| e.to_string())?,
        _ => return Err("Unsupported administration command".into()),
    }
    Ok(None)
}

fn id(value: &str) -> Result<PresetId, String> {
    value
        .parse::<u32>()
        .map(PresetId)
        .map_err(|_| "Invalid preset ID".into())
}

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>) {
    let settings = window.global::<SettingsUi>();
    let weak = window.as_weak();
    let send = dispatch.clone();
    settings.on_preset_save(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        let record = (|| -> Result<PresetRecord, String> {
            Ok(PresetRecord {
                name: window.get_preset_name().into(),
                definition: PresetDefinition::Gemini(GeminiPresetDefinition {
                    endpoint: window.get_preset_endpoint().into(),
                    model: window.get_preset_model().into(),
                    credential_env: window.get_preset_key_env().into(),
                    temperature: window
                        .get_preset_temperature()
                        .parse()
                        .map_err(|_| "Invalid temperature")?,
                    max_output_tokens: window
                        .get_preset_tokens()
                        .parse()
                        .map_err(|_| "Invalid output token limit")?,
                    timeout_ms: window
                        .get_preset_timeout()
                        .parse::<u64>()
                        .map_err(|_| "Invalid timeout")?,
                }),
            })
        })();
        match record {
            Ok(record) if window.get_preset_edit_id().is_empty() => {
                let token = uuid::Uuid::new_v4();
                window.set_preset_save_token(token.to_string().into());
                send(Command::CreatePreset(record, token))
            }
            Ok(record) => match id(&window.get_preset_edit_id()) {
                Ok(id) => send(Command::UpdatePreset(id, record)),
                Err(error) => window.set_settings_error(error.into()),
            },
            Err(error) => window.set_settings_error(error.into()),
        }
    });
    let weak = window.as_weak();
    settings.on_preset_edit(move |selected| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        window.set_preset_save_token("".into());
        let record = window
            .get_settings_presets()
            .iter()
            .find(|r| r.id == selected);
        if let Some(record) = record {
            window.set_preset_edit_id(record.id);
            window.set_preset_name(record.name);
            window.set_preset_model(record.model);
            window.set_preset_endpoint(record.endpoint);
            window.set_preset_key_env(record.key_env);
            window.set_preset_temperature(record.temperature);
            window.set_preset_tokens(record.tokens);
            window.set_preset_timeout(record.timeout);
        } else {
            window.set_settings_error("Preset is no longer available".into());
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_preset_delete(move |value| {
        if let Some(component) = weak.upgrade() {
            let window = component.global::<SettingsUi>();
            match id(&value) {
                Ok(id) => send(Command::DeletePreset(id)),
                Err(error) => window.set_settings_error(error.into()),
            }
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_preset_activate(move |value| {
        if let Some(component) = weak.upgrade() {
            let window = component.global::<SettingsUi>();
            match id(&value) {
                Ok(id) => send(Command::ActivePreset(id)),
                Err(error) => window.set_settings_error(error.into()),
            }
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_character_preset(move |character, operation, value| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        let character = match character {
            0 => CharacterId::Mita,
            1 => CharacterId::KindMita,
            _ => {
                window.set_settings_error("Unknown character".into());
                return;
            }
        };
        let operation = match operation {
            0 => PresetSlot::Reply,
            1 => PresetSlot::React,
            2 => PresetSlot::Initiate,
            _ => {
                window.set_settings_error("Unknown operation".into());
                return;
            }
        };
        let selection = if value == "active" {
            Ok(PresetSelection::Active)
        } else {
            id(&value).map(PresetSelection::Specific)
        };
        match selection {
            Ok(selection) => send(Command::AssignPreset(character, operation, selection)),
            Err(error) => window.set_settings_error(error.into()),
        }
    });
    let send = dispatch.clone();
    settings.on_voice_launcher(move |value| send(Command::VoiceInLauncher(value)));
    let send = dispatch.clone();
    settings.on_voice_game(move |value| send(Command::VoiceInGame(value)));
    let send = dispatch.clone();
    settings.on_volume(move |value| {
        if let Ok(value) = u8::try_from(value) {
            send(Command::Volume(value));
        }
    });
    settings.on_voice_name(move |value| dispatch(Command::VoiceName(value.into())));
}

pub fn render(window: &AppWindow, model: &AdministrationSnapshot) {
    let window = window.global::<SettingsUi>();
    let settings = &model.settings;
    window.set_settings_revision(settings.revision.to_string().into());
    window.set_settings_persistence(match settings.persistence {
        PersistenceState::Pending => 0,
        PersistenceState::Saved(_) => 1,
        PersistenceState::Failed { .. } => 2,
    });
    if let PersistenceState::Failed { error, .. } = &settings.persistence {
        window.set_settings_error(error.to_string().into());
    }
    window.set_settings_active(settings.values.presets.active.0.to_string().into());
    window.set_settings_volume(i32::from(settings.values.voice.volume.get()));
    window.set_settings_voice_launcher(settings.values.voice.in_launcher);
    window.set_settings_voice_game(settings.values.voice.in_game);
    window.set_settings_voice_name(settings.values.voice.voice.as_str().into());
    let selection = |value: PresetSelection| match value {
        PresetSelection::Active => "active".into(),
        PresetSelection::Specific(id) => id.0.to_string(),
    };
    window.set_mita_reply(selection(settings.values.characters.mita.presets.reply).into());
    window.set_mita_react(selection(settings.values.characters.mita.presets.react).into());
    window.set_mita_initiate(selection(settings.values.characters.mita.presets.initiate).into());
    window.set_kind_reply(selection(settings.values.characters.kind_mita.presets.reply).into());
    window.set_kind_react(selection(settings.values.characters.kind_mita.presets.react).into());
    window
        .set_kind_initiate(selection(settings.values.characters.kind_mita.presets.initiate).into());
    let records = model
        .presets
        .iter()
        .map(|(id, record)| {
            let PresetDefinition::Gemini(definition) = &record.definition;
            PresetItem {
                id: id.0.to_string().into(),
                name: record.name.as_str().into(),
                model: definition.model.as_str().into(),
                endpoint: definition.endpoint.as_str().into(),
                key_env: definition.credential_env.as_str().into(),
                temperature: definition.temperature.to_string().into(),
                tokens: definition.max_output_tokens.to_string().into(),
                timeout: definition.timeout_ms.to_string().into(),
            }
        })
        .collect::<Vec<_>>();
    window.set_settings_presets(ModelRc::new(VecModel::from(records)));
}

pub fn saved(window: &AppWindow, created: Option<(PresetId, uuid::Uuid)>) {
    let ui = window.global::<SettingsUi>();
    if let Some((id, token)) = created {
        if ui.get_preset_save_token() != token.to_string() {
            return;
        }
        ui.set_preset_edit_id(id.0.to_string().into());
        ui.set_preset_save_token("".into());
    }
    ui.set_settings_error("".into());
}
