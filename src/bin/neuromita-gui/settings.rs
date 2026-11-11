use super::{
    runtime::{Command, ToolOption},
    view::{
        AppWindow, CharacterDebugItem, CharacterPresets, PresetItem, PresetParameterItem,
        PresetTemplateItem, PromptSetItem, SettingsUi,
    },
};
use neuromita::{
    character::CharacterId,
    generation::{PresetConnectionEdit, PresetGenerationEdit, PresetId, PresetRecord},
    presentation::CharacterDebugField,
    presentation::{GuiPresenter, PresetSlot, PresetsPresenter, PresetsView},
    settings::{
        PersistenceState, PresetSelection, SettingsArea, SettingsSnapshot, StructuredDisplayMode,
    },
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{rc::Rc, sync::Arc};

const GUI_SETTINGS_AREAS: [SettingsArea; 11] = [
    SettingsArea::Api,
    SettingsArea::Characters,
    SettingsArea::Voice,
    SettingsArea::Language,
    SettingsArea::Asr,
    SettingsArea::AiEngine,
    SettingsArea::Games,
    SettingsArea::Models,
    SettingsArea::Images,
    SettingsArea::Updates,
    SettingsArea::Datasets,
];

fn area_for_section(index: i32) -> Option<SettingsArea> {
    usize::try_from(index)
        .ok()
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| GUI_SETTINGS_AREAS.get(index).copied())
}

pub struct AdministrationSnapshot {
    pub settings: Arc<SettingsSnapshot>,
    pub presets: PresetsView,
    pub prompt_sets: Vec<neuromita::prompts::PromptSetInfo>,
}

pub fn execute(api: &GuiPresenter, command: Command) -> Result<(), String> {
    match command {
        Command::SelectPromptSet(character, name) => {
            let catalog = api
                .prompts
                .as_ref()
                .ok_or("Prompt catalog is not configured")?;
            catalog
                .resolve_set(character, Some(&name))
                .map_err(|e| e.to_string())?;
            api.settings
                .characters
                .for_character(character)
                .prompt_set
                .set(Some(name))
                .map_err(|e| e.to_string())?;
        }
        Command::UpdatePreset(id, record) => api
            .presets
            .update_connection(id, record)
            .map_err(|e| e.to_string())?,
        Command::DeletePreset(id) => api.presets.delete(id).map_err(|e| e.to_string())?,
        Command::MovePreset(id, step) => api
            .presets
            .move_one(id, step)
            .map_err(|error| error.to_string())?,
        Command::ActivePreset(id) => {
            api.presets
                .require_configured(id)
                .map_err(|e| e.to_string())?;
            api.settings
                .presets
                .active
                .set(id)
                .map_err(|e| e.to_string())?;
        }
        Command::AssignAllPresets(character, selection) => {
            if let PresetSelection::Specific(id) = selection {
                api.presets.require(id).map_err(|e| e.to_string())?;
            }
            api.settings
                .characters
                .for_character(character)
                .assignments
                .set(neuromita::settings::OperationPresets {
                    reply: selection,
                    react: selection,
                    initiate: selection,
                })
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
        Command::SaveTts(mut value) => {
            let current = api.settings.tts.preferences.get();
            value.character_voice_ids = current.character_voice_ids;
            value.default_voice_id = current.default_voice_id;
            api.settings
                .tts
                .save_python_preferences(value)
                .map_err(|e| e.to_string())?;
        }
        Command::SaveCharacterVoice(character, voice_id) => {
            if voice_id.len() > 512 {
                return Err("Invalid voice ID".into());
            }
            let mut value = api.settings.tts.preferences.get();
            if let Some(id) = character {
                value.character_voice_ids.insert(id, voice_id);
            } else {
                value.default_voice_id = voice_id;
            }
            api.settings
                .tts
                .preferences
                .set(value)
                .map_err(|e| e.to_string())?;
        }
        Command::SelectAsrRecognizer(model_id) => {
            let model = (!model_id.is_empty())
                .then(|| neuromita::settings::SpeechModelReference::python(model_id));
            api.settings
                .asr
                .model
                .set(model)
                .map_err(|e| e.to_string())?;
        }
        Command::SaveAsr(mut value, language) => {
            let current = api.settings.asr.preferences.get();
            value.recognizer_id = current.recognizer_id;
            value.model_languages = current.model_languages;
            if let Some(language) = language
                && !value.recognizer_id.is_empty()
            {
                value
                    .model_languages
                    .insert(value.recognizer_id.clone(), language);
            }
            api.settings
                .asr
                .preferences
                .set(value)
                .map_err(|e| e.to_string())?;
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
        Command::SectionVisibility(area, enabled) => api
            .settings
            .interface
            .set_visible(area, enabled)
            .map_err(|e| e.to_string())?,
        Command::HidePrivate(value) => api
            .settings
            .interface
            .hide_private
            .set(value)
            .map_err(|e| e.to_string())?,
        Command::StructuredDisplay(mode) => api
            .settings
            .interface
            .structured_display
            .set(mode)
            .map_err(|e| e.to_string())?,
        Command::SetGuideLevel(level) => api
            .settings
            .onboarding
            .guide_level
            .set(level)
            .map_err(|e| e.to_string())?,
        Command::SetGuidePage(page) => api
            .settings
            .onboarding
            .guide_page
            .set(page)
            .map_err(|e| e.to_string())?,
        Command::SetGuideLanguage(language) => api
            .settings
            .interface
            .language
            .set(language)
            .map_err(|e| e.to_string())?,
        Command::SetToolOption(option, enabled) => {
            let mut settings = api.settings.tools.state.get();
            match option {
                ToolOption::Enabled => settings.enabled = enabled,
                ToolOption::Calculator => settings.calculator = enabled,
                ToolOption::MemorySearch => settings.memory_search = enabled,
                ToolOption::WebSearch => settings.web.search_enabled = enabled,
                ToolOption::WebReader => settings.web.reader_enabled = enabled,
            }
            settings.web.enabled = settings.web.search_enabled || settings.web.reader_enabled;
            settings.validate().map_err(|error| error.to_string())?;
            api.settings
                .tools
                .state
                .set(settings)
                .map_err(|e| e.to_string())?;
        }
        Command::SetMemoryDedupEnabled(enabled) => {
            let mut state = api.settings.memory.state.get();
            state.model_add_dedup_enabled = enabled;
            api.settings
                .memory
                .state
                .set(state)
                .map_err(|error| error.to_string())?;
        }
        Command::SetMemoryDedupThreshold(value) => {
            let mut state = api.settings.memory.state.get();
            state.threshold_per_mille = value;
            api.settings
                .memory
                .state
                .set(state)
                .map_err(|error| error.to_string())?;
        }
        Command::SetToolDepth(value) => {
            if !(1..=5).contains(&value) {
                return Err("Tool chain depth must be between 1 and 5".into());
            }
            let mut settings = api.settings.tools.state.get();
            settings.max_depth = value;
            api.settings
                .tools
                .state
                .set(settings)
                .map_err(|e| e.to_string())?;
        }
        Command::VoiceName(value) => api
            .settings
            .voice
            .voice
            .set(value)
            .map_err(|e| e.to_string())?,
        _ => return Err("Unsupported administration command".into()),
    }
    Ok(())
}

fn parse_memory_threshold(value: &str) -> Result<u16, String> {
    let value = value.trim();
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction))
            if !fraction.is_empty()
                && fraction.len() <= 3
                && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (whole, fraction)
        }
        None => (value, ""),
        _ => return Err("Memory dedup threshold must be between 0.500 and 1.000".into()),
    };
    let integer = match whole {
        "0" => 0,
        "1" => 1000,
        _ => return Err("Memory dedup threshold must be between 0.500 and 1.000".into()),
    };
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u16>().map_err(|error| error.to_string())?
            * 10u16.pow(3 - fraction.len() as u32)
    };
    let parsed = integer + fraction;
    if !(500..=1000).contains(&parsed) {
        return Err("Memory dedup threshold must be between 0.500 and 1.000".into());
    }
    Ok(parsed)
}

#[cfg(test)]
mod memory_threshold_tests {
    use super::parse_memory_threshold;

    #[test]
    fn accepts_exact_fixed_point_thresholds() {
        for (input, expected) in [
            ("0.5", 500),
            ("0.800", 800),
            ("0.925", 925),
            ("1", 1000),
            ("1.000", 1000),
        ] {
            assert_eq!(parse_memory_threshold(input).unwrap(), expected);
        }
    }

    #[test]
    fn rejects_invalid_or_ambiguous_thresholds() {
        for input in [
            "", "0.499", "1.001", "0.8009", "0.8e0", "NaN", "800", "-0.8",
        ] {
            assert!(parse_memory_threshold(input).is_err(), "{input:?}");
        }
    }
}

fn id(value: &str) -> Result<PresetId, String> {
    value
        .parse::<u32>()
        .map(PresetId)
        .map_err(|_| "Invalid preset ID".into())
}

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>) {
    let settings = window.global::<SettingsUi>();
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_preset_new(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        if ui.get_preset_dirty() {
            ui.set_preset_pending_id("__new__".into());
        } else {
            send(Command::NewPreset);
        }
    });
    let weak = window.as_weak();
    settings.on_preset_template_select(move |id| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<SettingsUi>();
            if id.is_empty() {
                ui.set_preset_template_id("".into());
                ui.set_preset_template_name("".into());
                ui.set_preset_provider_icon("".into());
                ui.set_preset_schema_label("".into());
                ui.set_preset_url_template("".into());
                ui.set_preset_endpoint("".into());
                ui.set_preset_model("".into());
                ui.set_preset_key_env("".into());
                ui.set_preset_temperature("".into());
                ui.set_preset_tokens("".into());
                ui.set_preset_timeout("".into());
            } else if ui
                .get_preset_templates()
                .iter()
                .any(|item| item.id == id && item.available)
            {
                let template = ui.get_preset_templates().iter().find(|item| item.id == id);
                if let Some(template) = template {
                    ui.set_preset_template_id(id);
                    ui.set_preset_template_name(template.name);
                    ui.set_preset_provider_icon(template.icon);
                    ui.set_preset_schema_label(template.format);
                    let endpoint = if ui.get_preset_endpoint().is_empty() {
                        ui.set_preset_endpoint(template.endpoint.clone());
                        template.endpoint
                    } else {
                        ui.get_preset_endpoint()
                    };
                    let request_url_template = PresetsPresenter::request_url_template(
                        ui.get_preset_template_id().as_str(),
                        &endpoint,
                    )
                    .unwrap_or_default();
                    ui.set_preset_url_template(request_url_template.into());
                    match PresetsPresenter::template(ui.get_preset_template_id().as_str()) {
                        Ok(template) => {
                            ui.set_preset_output_token_wire_key(
                                template.output_token_wire_key().into(),
                            );
                            ui.set_preset_temperature_enabled(template.send_temperature);
                            ui.set_preset_tokens_enabled(template.send_max_output_tokens);
                            if ui.get_preset_temperature().is_empty() {
                                ui.set_preset_temperature(template.temperature.to_string().into());
                            }
                            if ui.get_preset_tokens().is_empty() {
                                ui.set_preset_tokens(template.max_output_tokens.to_string().into());
                            }
                            if ui.get_preset_timeout().is_empty() {
                                ui.set_preset_timeout(template.timeout_ms.to_string().into());
                            }
                        }
                        Err(error) => ui.set_settings_error(error.to_string().into()),
                    }
                }
            } else {
                ui.set_settings_error("Этот провайдер пока не поддерживается ядром".into());
            }
        }
    });
    let weak = window.as_weak();
    settings.on_preset_documentation(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let chosen = ui.get_preset_template_id();
        let trusted = PresetsPresenter::providers()
            .into_iter()
            .find(|provider| chosen == provider.id && provider.supported);
        if let Some(provider) =
            trusted.filter(|provider| provider.documentation.starts_with("https://"))
            && let Err(error) = super::binding::open_external_url(provider.documentation)
        {
            ui.set_settings_error(error.into());
        }
    });
    let weak = window.as_weak();
    settings.on_preset_help(move |kind| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let selected = ui.get_preset_template_id();
        let target = PresetsPresenter::providers()
            .into_iter()
            .find(|provider| selected == provider.id && provider.supported)
            .map(|provider| {
                if kind == 1 {
                    provider.models_url
                } else if kind == 2 {
                    provider.credentials_url
                } else {
                    ""
                }
            })
            .filter(|link| link.starts_with("https://"));
        if let Some(url) = target
            && let Err(error) = super::binding::open_external_url(url)
        {
            ui.set_settings_error(error.into());
        }
    });
    let weak = window.as_weak();
    let send = dispatch.clone();
    settings.on_preset_save(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        let record = (|| -> Result<PresetConnectionEdit, String> {
            if window.get_preset_edit_id().is_empty() {
                return Err("Choose a preset before editing".into());
            }
            if window.get_preset_name().trim().is_empty() {
                return Err("Preset name is required".into());
            }
            if window.get_preset_template_id().is_empty() {
                return Ok(PresetConnectionEdit {
                    key: None,
                    name: window.get_preset_name().into(),
                    connection: None,
                });
            }
            let template = PresetsPresenter::template(window.get_preset_template_id().as_str())
                .map_err(|e| e.to_string())?;
            Ok(PresetConnectionEdit {
                key: Some(neuromita::generation::SecretKey::new(
                    window.get_preset_key_env().to_string(),
                )),
                name: window.get_preset_name().into(),
                connection: Some(PresetGenerationEdit {
                    protocol: template.protocol,
                    send_temperature: window.get_preset_temperature_enabled(),
                    send_max_output_tokens: window.get_preset_tokens_enabled(),
                    endpoint: window.get_preset_endpoint().into(),
                    model: window.get_preset_model().into(),
                    credential: None,
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
        if selected == window.get_preset_edit_id() {
            return;
        }
        if window.get_preset_dirty() {
            window.set_preset_pending_id(selected);
            return;
        }
        if let Some(record) = window
            .get_settings_presets()
            .iter()
            .find(|r| r.id == selected)
        {
            select_preset(&window, &record);
        } else {
            window.set_settings_error("Preset is no longer available".into());
        }
    });
    let weak = window.as_weak();
    settings.on_preset_confirm_cancel(move || {
        if let Some(component) = weak.upgrade() {
            component
                .global::<SettingsUi>()
                .set_preset_pending_id("".into());
        }
    });
    let weak = window.as_weak();
    settings.on_preset_confirm_save(move || {
        if let Some(component) = weak.upgrade() {
            let ui = component.global::<SettingsUi>();
            ui.set_preset_pending_save(true);
            ui.invoke_preset_save();
        }
    });
    let weak = window.as_weak();
    let send = dispatch.clone();
    settings.on_preset_confirm_discard(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let target = ui.get_preset_pending_id();
        ui.set_preset_pending_id("".into());
        if target == "__new__" {
            send(Command::NewPreset);
        } else if let Some(record) = ui
            .get_settings_presets()
            .iter()
            .find(|row| row.id == target)
        {
            select_preset(&ui, &record);
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
    settings.on_preset_move(move |value, step| {
        if let Some(component) = weak.upgrade() {
            let ui = component.global::<SettingsUi>();
            match id(&value) {
                Ok(id) => send(Command::MovePreset(id, step)),
                Err(error) => ui.set_settings_error(error.into()),
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
    settings.on_character_prompt_set(move |character, name| {
        let Some(character) = CharacterId::from_index(character) else {
            return;
        };
        if !name.is_empty() {
            send(Command::SelectPromptSet(character, name.into()));
        }
    });
    let send = dispatch.clone();
    settings.on_prompt_reload(move || send(Command::RefreshPromptCatalog));
    let send = dispatch.clone();
    settings.on_character_debug_select(move |index| {
        if let Some(character) = CharacterId::from_index(index) {
            send(Command::DebugSelect(character));
        }
    });
    let send = dispatch.clone();
    settings.on_prompt_open_folder(move |index| {
        if let Some(character) = CharacterId::from_index(index) {
            send(Command::OpenPromptFolder(character));
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_character_preset(move |character, operation, value| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        let character = match CharacterId::from_index(character) {
            Some(id) => id,
            None => {
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
    let weak = window.as_weak();
    settings.on_character_common(move |character, value| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let character = match CharacterId::from_index(character) {
            Some(id) => id,
            None => {
                ui.set_settings_error("Unknown character".into());
                return;
            }
        };
        let selection = if value == "active" {
            Ok(PresetSelection::Active)
        } else {
            id(&value).map(PresetSelection::Specific)
        };
        match selection {
            Ok(selection) => send(Command::AssignAllPresets(character, selection)),
            Err(error) => ui.set_settings_error(error.into()),
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_structured_mode_change({
        let send = dispatch.clone();
        move |value| {
            let mode = match value {
                0 => StructuredDisplayMode::Off,
                1 => StructuredDisplayMode::Brief,
                2 => StructuredDisplayMode::Json,
                _ => return,
            };
            send(Command::StructuredDisplay(mode));
        }
    });
    settings.on_tool_option(move |name, enabled| {
        let option = match name.as_str() {
            "enabled" => Some(ToolOption::Enabled),
            "calculator" => Some(ToolOption::Calculator),
            "memory_search" => Some(ToolOption::MemorySearch),
            "web_search" => Some(ToolOption::WebSearch),
            "web_reader" => Some(ToolOption::WebReader),
            _ => None,
        };
        if let Some(option) = option {
            send(Command::SetToolOption(option, enabled));
        } else if let Some(window) = weak.upgrade() {
            window
                .global::<SettingsUi>()
                .set_settings_error("Unsupported tool".into());
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_tool_depth_change(move |value| match value.parse::<u8>() {
        Ok(depth) if (1..=5).contains(&depth) => send(Command::SetToolDepth(depth)),
        _ => {
            if let Some(window) = weak.upgrade() {
                window
                    .global::<SettingsUi>()
                    .set_settings_error("Tool chain depth must be between 1 and 5".into());
            }
        }
    });
    let send = dispatch.clone();
    settings.on_memory_dedup_toggle(move |enabled| send(Command::SetMemoryDedupEnabled(enabled)));
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_memory_dedup_threshold_change(move |value| match parse_memory_threshold(&value) {
        Ok(value) => send(Command::SetMemoryDedupThreshold(value)),
        Err(error) => {
            if let Some(window) = weak.upgrade() {
                window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into());
            }
        }
    });
    let send = dispatch.clone();
    settings.on_section_visibility(move |index, checked| {
        if let Some(area) = area_for_section(index) {
            send(Command::SectionVisibility(area, checked));
        }
    });
    let send = dispatch.clone();
    settings.on_set_hide_private(move |checked| send(Command::HidePrivate(checked)));
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

pub fn render_character_debug(
    window: &AppWindow,
    character: CharacterId,
    fields: &[CharacterDebugField],
    error: Option<&str>,
) {
    let settings = window.global::<SettingsUi>();
    settings.set_character_debug_character(character.index() as i32);
    settings.set_character_debug_error(error.unwrap_or_default().into());
    settings.set_character_debug_values(ModelRc::new(VecModel::from(
        fields
            .iter()
            .map(|field| CharacterDebugItem {
                label: field.label.as_str().into(),
                value: field.value.as_str().into(),
            })
            .collect::<Vec<_>>(),
    )));
}

pub fn render(window: &AppWindow, model: &AdministrationSnapshot) {
    let window = window.global::<SettingsUi>();
    let settings = &model.settings;
    window.set_settings_revision(settings.revision.to_string().into());
    window.set_section_visible(ModelRc::new(VecModel::from(
        std::iter::once(true)
            .chain(
                GUI_SETTINGS_AREAS
                    .iter()
                    .map(|area| settings.values.interface.visibility.is_visible(*area)),
            )
            .collect::<Vec<_>>(),
    )));
    window.set_hide_private(settings.values.interface.hide_private);
    window.set_structured_mode(match settings.values.interface.structured_display {
        StructuredDisplayMode::Off => 0,
        StructuredDisplayMode::Brief => 1,
        StructuredDisplayMode::Json => 2,
    });
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
    window.set_memory_dedup_enabled(settings.values.memory.model_add_dedup_enabled);
    let threshold = settings.values.memory.threshold_per_mille;
    window
        .set_memory_dedup_threshold(format!("{}.{:03}", threshold / 1000, threshold % 1000).into());
    let tools = &settings.values.tools;
    window.set_tool_enabled(tools.enabled);
    window.set_tool_calculator(tools.calculator);
    window.set_tool_memory_search(tools.memory_search);
    window.set_tool_reminder_enabled(settings.values.reminders.enabled);
    window.set_tool_web_search(tools.web.search_enabled);
    window.set_tool_web_reader(tools.web.reader_enabled);
    window.set_tool_search_configured(tools.web.search_endpoint.is_some());
    window.set_tool_reader_configured(!tools.web.reader_allowed_hosts.is_empty());
    window.set_tool_max_depth(tools.max_depth.to_string().into());
    let previous = window.get_character_presets();
    let encode = |value: PresetSelection| match value {
        PresetSelection::Active => "active".into(),
        PresetSelection::Specific(id) => id.0.to_string().into(),
    };
    let rows = CharacterId::ALL
        .into_iter()
        .map(|id| {
            let presets = settings.values.characters.for_character(id).presets;
            let mut row = CharacterPresets {
                reply: encode(presets.reply),
                react: encode(presets.react),
                initiate: encode(presets.initiate),
                split: presets.reply != presets.react || presets.reply != presets.initiate,
            };
            if let Some(old) = previous.row_data(id.index())
                && old.reply == row.reply
                && old.react == row.react
                && old.initiate == row.initiate
            {
                row.split = old.split;
            }
            row
        })
        .collect::<Vec<_>>();
    window.set_character_presets(ModelRc::new(VecModel::from(rows)));
    let prompt_options = model
        .prompt_sets
        .iter()
        .map(|set| PromptSetItem {
            character: set.character.index() as i32,
            name: set.name.as_str().into(),
            author: set.author.as_str().into(),
            version: set.version.as_str().into(),
            description: set.description.as_str().into(),
        })
        .collect::<Vec<_>>();
    window.set_prompt_set_options(ModelRc::new(VecModel::from(prompt_options)));
    let prompt_selections = CharacterId::ALL
        .into_iter()
        .map(|id| {
            let requested = settings
                .values
                .characters
                .for_character(id)
                .prompt_set
                .as_deref();
            if let Some(name) = requested {
                if model
                    .prompt_sets
                    .iter()
                    .any(|set| set.character == id && set.name == name)
                {
                    return name.into();
                }
                return "".into();
            }
            model
                .prompt_sets
                .iter()
                .filter(|set| set.character == id)
                .find(|set| set.name.eq_ignore_ascii_case("Default"))
                .or_else(|| model.prompt_sets.iter().find(|set| set.character == id))
                .map(|set| set.name.as_str())
                .unwrap_or_default()
                .into()
        })
        .collect::<Vec<_>>();
    window.set_character_prompt_sets(ModelRc::new(VecModel::from(prompt_selections)));
    let templates = provider_templates(&model.presets.providers);
    window.set_preset_templates(ModelRc::new(VecModel::from(templates)));
    let parameters = model
        .presets
        .parameters
        .iter()
        .map(|parameter| PresetParameterItem {
            id: parameter.id.into(),
            wire_key: parameter.wire_key.into(),
            title: parameter.title.source().into(),
            title_key: parameter.title.id().into(),

            description: parameter.description.source().into(),
            description_key: parameter.description.id().into(),

            editable: parameter.editable,
            sent: parameter.sent,
        })
        .collect::<Vec<_>>();
    window.set_preset_parameters(ModelRc::new(VecModel::from(parameters)));
    let records = model
        .presets
        .records
        .iter()
        .map(|record| PresetItem {
            output_token_wire_key: record.output_token_wire_key.into(),
            template_id: record.template_id.into(),
            temperature_enabled: record.send_temperature,
            tokens_enabled: record.send_max_output_tokens,
            id: record.id.0.to_string().into(),
            name: record.name.as_str().into(),
            model: record.model.as_str().into(),
            provider: record.provider.into(),
            endpoint: record.endpoint.as_str().into(),
            key_env: record
                .api_key
                .as_ref()
                .map(|key| key.expose())
                .unwrap_or_default()
                .into(),
            key_error: record
                .credential_error
                .as_deref()
                .unwrap_or_default()
                .into(),
            temperature: if record.configured {
                record.temperature.to_string().into()
            } else {
                "".into()
            },
            tokens: if record.configured {
                record.max_output_tokens.to_string().into()
            } else {
                "".into()
            },
            timeout: if record.configured {
                record.timeout_ms.to_string().into()
            } else {
                "".into()
            },
            configured: record.configured,
        })
        .collect::<Vec<_>>();
    let selected = window.get_preset_edit_id();
    if !window.get_preset_creating()
        && (selected.is_empty() || !records.iter().any(|record| record.id == selected))
    {
        let active = settings.values.presets.active.0.to_string();
        let first = records
            .iter()
            .find(|record| record.id == active)
            .or_else(|| records.first());
        if let Some(record) = first {
            select_preset(&window, record);
        }
    }
    if records.is_empty() {
        clear_preset(&window);
    } else if let Some(record) = records.iter().find(|r| r.id == window.get_preset_edit_id())
        && record.configured != window.get_preset_configured()
    {
        select_preset(&window, record);
    }
    window.set_settings_presets(ModelRc::new(VecModel::from(records)));
}

fn mark_preset_baseline(ui: &SettingsUi) {
    ui.set_preset_baseline_name(ui.get_preset_name());
    ui.set_preset_baseline_model(ui.get_preset_model());
    ui.set_preset_baseline_endpoint(ui.get_preset_endpoint());
    ui.set_preset_baseline_key_env(ui.get_preset_key_env());
    ui.set_preset_baseline_template(ui.get_preset_template_id());
    ui.set_preset_baseline_temperature_enabled(ui.get_preset_temperature_enabled());
    ui.set_preset_baseline_tokens_enabled(ui.get_preset_tokens_enabled());
    ui.set_preset_baseline_temperature(ui.get_preset_temperature());
    ui.set_preset_baseline_tokens(ui.get_preset_tokens());
    ui.set_preset_baseline_timeout(ui.get_preset_timeout());
}

fn select_preset(ui: &SettingsUi, record: &PresetItem) {
    ui.set_preset_creating(false);
    ui.set_preset_configured(record.configured);
    ui.set_preset_delete_id("".into());
    ui.set_preset_edit_id(record.id.clone());
    ui.set_preset_name(record.name.clone());
    ui.set_preset_model(record.model.clone());
    ui.set_preset_endpoint(record.endpoint.clone());
    ui.set_preset_key_revealed(false);
    ui.set_preset_key_env(record.key_env.clone());
    if !record.key_error.is_empty() {
        ui.set_settings_error(record.key_error.clone());
    }
    ui.set_preset_temperature_enabled(record.temperature_enabled);
    ui.set_preset_tokens_enabled(record.tokens_enabled);
    ui.set_preset_output_token_wire_key(record.output_token_wire_key.clone());
    ui.set_preset_temperature(record.temperature.clone());
    ui.set_preset_tokens(record.tokens.clone());
    ui.set_preset_timeout(record.timeout.clone());
    ui.set_preset_template_id("".into());
    ui.set_preset_template_name("".into());
    ui.set_preset_provider_icon("".into());
    ui.set_preset_schema_label("".into());
    ui.set_preset_url_template("".into());
    if record.configured {
        if let Some(template) = ui
            .get_preset_templates()
            .iter()
            .find(|item| item.id == record.template_id)
        {
            ui.set_preset_template_id(template.id);
            ui.set_preset_template_name(template.name);
            ui.set_preset_provider_icon(record.provider.clone());
            ui.set_preset_schema_label(template.format);
            if let Some(request_url_template) = PresetsPresenter::request_url_template(
                record.template_id.as_str(),
                &record.endpoint,
            ) {
                ui.set_preset_url_template(request_url_template.into());
            }
        } else {
            ui.set_settings_error("Provider metadata is unavailable for this preset".into());
        }
    }
    mark_preset_baseline(ui);
}

pub fn clear_preset(ui: &SettingsUi) {
    ui.set_preset_edit_id("".into());
    ui.set_preset_configured(false);
    ui.set_preset_name("".into());
    ui.set_preset_model("".into());
    ui.set_preset_endpoint("".into());
    ui.set_preset_key_env("".into());
    ui.set_preset_temperature("".into());
    ui.set_preset_tokens("".into());
    ui.set_preset_timeout("".into());
    ui.set_preset_template_id("".into());
    ui.set_preset_template_name("".into());
    ui.set_preset_provider_icon("".into());
    ui.set_preset_schema_label("".into());
    ui.set_preset_url_template("".into());
    ui.set_preset_delete_id("".into());
    mark_preset_baseline(ui);
}

pub fn created(window: &AppWindow, id: PresetId, record: &PresetRecord) {
    let ui = window.global::<SettingsUi>();
    let item = PresetItem {
        key_error: "".into(),
        output_token_wire_key: "".into(),
        template_id: "".into(),
        temperature_enabled: false,
        tokens_enabled: false,
        id: id.0.to_string().into(),
        name: record.name.as_str().into(),
        model: "".into(),
        provider: "".into(),
        endpoint: "".into(),
        key_env: "".into(),
        temperature: "".into(),
        tokens: "".into(),
        timeout: "".into(),
        configured: false,
    };
    select_preset(&ui, &item);
    ui.set_preset_search("".into());
    ui.set_preset_tab(0);
}

pub fn saved(window: &AppWindow, edited: Option<PresetId>) {
    let ui = window.global::<SettingsUi>();
    ui.set_settings_error("".into());
    if edited.is_some_and(|id| ui.get_preset_edit_id() == id.0.to_string()) {
        mark_preset_baseline(&ui);
        if ui.get_preset_pending_save() {
            let target = ui.get_preset_pending_id();
            ui.set_preset_pending_id("".into());
            ui.set_preset_pending_save(false);
            if target == "__new__" {
                ui.invoke_preset_new();
            } else if !target.is_empty() {
                ui.invoke_preset_edit(target);
            }
        }
    }
}

pub(super) fn provider_templates(
    providers: &[neuromita::presentation::ProviderOption],
) -> Vec<PresetTemplateItem> {
    providers
        .iter()
        .map(|provider| PresetTemplateItem {
            id: provider.id.into(),
            name: provider.name.into(),
            icon: provider.icon.into(),
            format: provider.schema_label.into(),
            endpoint: PresetsPresenter::template(provider.id)
                .map(|template| template.endpoint)
                .unwrap_or_default()
                .into(),
            documentation: provider.documentation.into(),
            pricing: provider.pricing.into(),
            badge_kind: provider.badge_kind.into(),
            available: provider.supported,
        })
        .collect::<Vec<_>>()
}

#[cfg(test)]
mod preset_move_gui_tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn slint_move_callback_dispatches_selected_id_and_direction() {
        let window = AppWindow::new().expect("Slint test window");
        let captured = Rc::new(RefCell::new(Vec::new()));
        let events = Rc::clone(&captured);
        bind(
            &window,
            Rc::new(move |command| events.borrow_mut().push(command)),
        );
        let settings = window.global::<SettingsUi>();
        settings.invoke_preset_move("42".into(), -1);
        settings.invoke_preset_move("42".into(), 1);
        let commands = captured.borrow();
        assert!(matches!(
            commands.first(),
            Some(Command::MovePreset(PresetId(42), -1))
        ));
        assert!(matches!(
            commands.get(1),
            Some(Command::MovePreset(PresetId(42), 1))
        ));
    }
}
