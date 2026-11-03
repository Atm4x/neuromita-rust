use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc, time::Duration};

use neuromita::localization::{FallbackPolicy, LocaleId, Localizer, i18n};
use neuromita::presentation::PresetsPresenter;
use slint::ComponentHandle;

mod view {
    slint::include_modules!();
}

fn main() -> Result<(), Box<dyn Error>> {
    let destination = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: gui_snapshot <new-output.ppm>")?,
    );
    if destination.exists() {
        return Err("snapshot destination already exists".into());
    }
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .select()?;
    let window = view::AppWindow::new()?;
    let catalog = Localizer::bundled(FallbackPolicy::EnglishThenSource)?;
    let translations = window.global::<view::I18n>();
    translations.on_lookup(move |code, _, id, source| {
        catalog
            .resolve(
                &LocaleId::new(&code).expect("fixture locale"),
                &i18n(&id, &source),
            )
            .expect("fixture translation")
            .into()
    });
    translations.set_locale("ru".into());
    window.set_page(view::PageId::Settings);
    window.set_version(format!("v{}", env!("CARGO_PKG_VERSION")).into());
    let general = std::env::args().nth(2).as_deref() == Some("general");
    window
        .global::<view::SettingsUi>()
        .set_section(if general { 0 } else { 2 });
    let characters = neuromita::character::character_catalog()
        .enumerate()
        .map(|(index, item)| view::CharacterOption {
            index: index as i32,
            id: item.id.as_str().into(),
            name: item.name.source().into(),
            name_key: item.name.id().into(),
            conversation_name: item.conversation_name.source().into(),
            conversation_key: item.conversation_name.id().into(),
            initial: item.id.as_str().chars().next().unwrap().to_string().into(),
        })
        .collect::<Vec<_>>();
    window
        .global::<view::CharacterCatalog>()
        .set_entries(slint::ModelRc::new(slint::VecModel::from(characters)));
    let mode = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "characters".into());
    let settings = window.global::<view::SettingsUi>();
    settings.set_section(if general { 0 } else if mode.starts_with("api") { 1 } else { 2 });
    if mode.starts_with("api") {
        let templates = PresetsPresenter::providers()
            .into_iter()
            .map(|provider| view::PresetTemplateItem {
                id: provider.id.into(),
                name: provider.name.into(),
                icon: provider.icon.into(),
                format: provider.schema_label.into(),
                documentation: provider.documentation.into(),
                pricing: provider.pricing.into(),
                badge_kind: provider.badge_kind.into(),
                endpoint: if provider.id == "gemini" {
                    neuromita::generation::gemini::GeminiTemplate::GOOGLE_ENDPOINT.into()
                } else {
                    "".into()
                },
                available: provider.supported,
            })
            .collect::<Vec<_>>();
        settings.set_preset_templates(slint::ModelRc::new(slint::VecModel::from(templates)));
        let parameters = PresetsPresenter::parameters()
            .into_iter()
            .map(|parameter| view::PresetParameterItem {
                id: parameter.id.into(),
                wire_key: parameter.wire_key.into(),
                title_ru: parameter.title_ru.into(),
                title_en: parameter.title_en.into(),
                description_ru: parameter.description_ru.into(),
                description_en: parameter.description_en.into(),
                editable: parameter.editable,
                sent: parameter.sent,
            })
            .collect::<Vec<_>>();
        settings.set_preset_parameters(slint::ModelRc::new(slint::VecModel::from(parameters)));
        settings.set_preset_tab(if mode == "api-generation" { 1 } else { 0 });
    }
    window.show()?;
    window
        .window()
        .set_size(slint::PhysicalSize::new(1560, 918));
    let result = Rc::new(RefCell::new(None));
    let completed = result.clone();
    let weak = window.as_weak();
    slint::Timer::single_shot(Duration::from_millis(500), move || {
        let outcome = (|| -> Result<(), Box<dyn Error>> {
            let window = weak.upgrade().ok_or("snapshot window closed")?;
            let image = window.window().take_snapshot()?;
            let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            for pixel in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&pixel[..3]);
            }
            use std::io::Write;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?
                .write_all(&ppm)?;
            window.hide()?;
            Ok(())
        })();
        *completed.borrow_mut() = Some(outcome.map_err(|error| error.to_string()));
        let _ = slint::quit_event_loop();
    });
    slint::run_event_loop()?;
    result
        .borrow_mut()
        .take()
        .ok_or("snapshot callback did not run")?
        .map_err(Into::into)
}
