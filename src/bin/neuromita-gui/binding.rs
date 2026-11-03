use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc};

use neuromita::localization::{
    FallbackPolicy, LocaleId, LocalizationError, LocalizedText, Localizer, i18n,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{
    memory::{EditAction, MemoryIntent, MemoryUi},
    presenter::{
        ComponentPreview, HomePreview, LauncherAction, Page, ShellCommand, ShellPresenter,
    },
    view::{AppWindow, ComponentCard, I18n, LauncherMenuAction, LocaleOption, PageId, ReleaseItem},
};

pub fn run() -> Result<(), Box<dyn Error>> {
    let options = Options::parse()?;
    let mut catalog = Localizer::bundled(FallbackPolicy::EnglishThenSource)?;
    if let Some(directory) = &options.directory {
        catalog.reload_directory(directory)?;
    }
    if !catalog.contains_locale(&options.locale) {
        return Err(LocalizationError::UnknownLocale(options.locale.to_string()).into());
    }
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .select()?;
    let window = AppWindow::new()?;
    let catalog = Rc::new(RefCell::new(catalog));
    window
        .global::<I18n>()
        .on_lookup(translation_lookup(catalog.clone()));
    let presenter = Rc::new(RefCell::new(ShellPresenter::new(
        HomePreview::python_reference(),
        options.locale,
    )));
    window.set_version("v2026.10.15.3".into());
    let releases = presenter
        .borrow()
        .home()
        .releases
        .iter()
        .map(|item| ReleaseItem {
            title: item.title.into(),
            date: item.date.into(),
        })
        .collect::<Vec<_>>();
    window.set_releases(ModelRc::new(VecModel::from(releases)));
    render(&window, &presenter.borrow(), &catalog.borrow());
    window.set_memory_status("Откройте память, чтобы подключиться к Core".into());
    let memory = MemoryUi::new(window.as_weak(), options.data_directory.clone());

    let weak = window.as_weak();
    let dispatch = Rc::new(move |command| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let mut presenter = presenter.borrow_mut();
        let command = if matches!(command, ShellCommand::ReloadTranslations) {
            if let Some(directory) = &options.directory {
                let mut candidate = catalog.borrow().clone();
                match candidate.reload_directory(directory).and_then(|()| {
                    if candidate.contains_locale(presenter.locale()) {
                        Ok(())
                    } else {
                        Err(LocalizationError::UnknownLocale(
                            presenter.locale().to_string(),
                        ))
                    }
                }) {
                    Ok(()) => {
                        *catalog.borrow_mut() = candidate;
                        ShellCommand::TranslationsReloaded
                    }
                    Err(error) => ShellCommand::LocalizationFailed(error.to_string()),
                }
            } else {
                ShellCommand::ReloadTranslations
            }
        } else {
            command
        };
        let command = match command {
            ShellCommand::SelectLanguage(locale) if !catalog.borrow().contains_locale(&locale) => {
                ShellCommand::LocalizationFailed(
                    LocalizationError::UnknownLocale(locale.to_string()).to_string(),
                )
            }
            command => command,
        };
        presenter.dispatch(command);
        render(&window, &presenter, &catalog.borrow());
    });
    let handler = dispatch.clone();
    let memory_send = memory.sender();
    let memory_window = window.as_weak();
    window.on_navigate(move |page| {
        let target = from_view(page);
        handler(ShellCommand::Navigate(target));
        if target == Page::Memory
            && let Some(window) = memory_window.upgrade()
        {
            window.set_memory_busy(true);
            window.set_memory_ready(false);
            window.set_memory_status("Загрузка памяти...".into());
            if let Some(character) =
                neuromita::character::CharacterId::parse(&window.get_memory_character())
            {
                let _ = memory_send.send(MemoryIntent::Load(character));
            }
        }
    });
    let handler = dispatch.clone();
    window.on_language_selected(move |code| {
        handler(match LocaleId::new(&code) {
            Ok(locale) => ShellCommand::SelectLanguage(locale),
            Err(error) => ShellCommand::LocalizationFailed(error.to_string()),
        })
    });
    let handler = dispatch.clone();
    window.on_reload_translations(move || handler(ShellCommand::ReloadTranslations));
    let handler = dispatch.clone();
    window.on_toggle_backend(move || handler(ShellCommand::ToggleBackend));
    let handler = dispatch.clone();
    window.on_toggle_client(move || handler(ShellCommand::ToggleClient));
    let handler = dispatch.clone();
    window.on_play(move || handler(ShellCommand::Play));
    let handler = dispatch.clone();
    window
        .on_launcher_action(move |action| handler(ShellCommand::Launcher(launcher_action(action))));
    let handler = dispatch.clone();
    window.on_social_selected(move |name| handler(ShellCommand::Social(name.into())));
    window.on_dismiss_notice(move || dispatch(ShellCommand::DismissNotice));
    let memory_send = memory.sender();
    let memory_window = window.as_weak();
    window.on_memory_character_changed(move |id| {
        if let Some(character) = neuromita::character::CharacterId::parse(&id) {
            if let Some(window) = memory_window.upgrade() {
                window.set_memory_busy(true);
                window.set_memory_ready(false);
                window.set_memory_items(ModelRc::new(VecModel::from(Vec::new())));
                window.set_memory_status("Загрузка памяти...".into());
            }
            let _ = memory_send.send(MemoryIntent::Load(character));
        }
    });
    let memory_send = memory.sender();
    let memory_window = window.as_weak();
    window.on_memory_execute(move |action, id, text| {
        let Some(window) = memory_window.upgrade() else {
            return;
        };
        let Some(character) =
            neuromita::character::CharacterId::parse(&window.get_memory_character())
        else {
            return;
        };
        window.set_memory_busy(true);
        window.set_memory_status("Выполнение операции...".into());
        let intent = match EditAction::from_view(action) {
            Some(action) => MemoryIntent::Edit {
                character,
                action,
                id: id.to_string(),
                text: text.to_string(),
            },
            None => MemoryIntent::Load(character),
        };
        let _ = memory_send.send(intent);
    });
    window.run()?;
    drop(memory);
    Ok(())
}

fn render(window: &AppWindow, presenter: &ShellPresenter, catalog: &Localizer) {
    let localize =
        |text: &LocalizedText| display_translation(catalog.resolve(presenter.locale(), text));
    let translations = window.global::<I18n>();
    translations.set_locale(presenter.locale().as_str().into());
    translations.set_revision((catalog.revision() % i32::MAX as u64) as i32);
    let languages = catalog
        .languages()
        .into_iter()
        .map(|language| LocaleOption {
            code: language.locale.as_str().into(),
            name: language.name.into(),
        })
        .collect::<Vec<_>>();
    translations.set_languages(ModelRc::new(VecModel::from(languages)));
    window.set_page(to_view(presenter.page()));
    window.set_page_title(localize(&presenter.page().title()));
    window.set_backend(component_card(&presenter.home().backend, &localize));
    window.set_client(component_card(&presenter.home().client, &localize));
    let (backend, client) = presenter.selections();
    window.set_backend_selected(backend);
    window.set_client_selected(client);
    window.set_notice(presenter.notice().map(localize).unwrap_or_default());
}

fn component_card(
    preview: &ComponentPreview,
    localize: &impl Fn(&LocalizedText) -> SharedString,
) -> ComponentCard {
    ComponentCard {
        title: localize(&preview.kind.title()).to_uppercase().into(),
        value: localize(
            &i18n("launcher.component.installed", "Установлен v{ver}").arg("ver", preview.version),
        ),
        update: preview.update.into(),
    }
}

fn display_translation(result: Result<String, LocalizationError>) -> SharedString {
    match result {
        Ok(text) => text.into(),
        Err(error) => {
            tracing::error!(%error, "localization failed");
            format!("⟦{error}⟧").into()
        }
    }
}

fn translation_lookup(
    catalog: Rc<RefCell<Localizer>>,
) -> impl Fn(SharedString, i32, SharedString, SharedString) -> SharedString {
    move |code, _revision, id, source| {
        let result = LocaleId::new(&code)
            .and_then(|locale| catalog.borrow().resolve(&locale, &i18n(&id, &source)));
        display_translation(result)
    }
}

struct Options {
    directory: Option<PathBuf>,
    data_directory: PathBuf,
    locale: LocaleId,
}

impl Options {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut options = Self {
            directory: None,
            data_directory: PathBuf::new(),
            locale: LocaleId::russian(),
        };
        let mut arguments = std::env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--translations-dir") => {
                    options.directory = Some(
                        arguments
                            .next()
                            .ok_or("--translations-dir requires a path")?
                            .into(),
                    )
                }
                Some("--data-dir") => {
                    options.data_directory =
                        PathBuf::from(arguments.next().ok_or("--data-dir requires a directory")?);
                    if options.data_directory.as_os_str().is_empty() {
                        return Err("empty data directory".into());
                    }
                }
                Some("--language") => {
                    options.locale = LocaleId::new(
                        arguments
                            .next()
                            .ok_or("--language requires a locale code")?
                            .to_str()
                            .ok_or("locale code must be UTF-8")?,
                    )?
                }
                _ => {
                    return Err(
                        format!("unknown GUI argument: {}", argument.to_string_lossy()).into(),
                    );
                }
            }
        }
        if options.data_directory.as_os_str().is_empty() {
            options.data_directory = default_data_directory()?;
        }
        Ok(options)
    }
}

fn default_data_directory() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = std::env::var_os("NEUROMITA_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(path).join("NeuroMita"));
    }
    #[cfg(not(windows))]
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("neuromita"));
    }
    if let Some(path) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(path).join(".config").join("neuromita"));
    }
    Err("data directory unavailable; specify --data-dir".into())
}

fn launcher_action(action: LauncherMenuAction) -> LauncherAction {
    match action {
        LauncherMenuAction::CheckUpdates => LauncherAction::CheckUpdates,
        LauncherMenuAction::InstallUnity => LauncherAction::InstallUnity,
        LauncherMenuAction::VerifyUnity => LauncherAction::VerifyUnity,
        LauncherMenuAction::OpenUnityFolder => LauncherAction::OpenUnityFolder,
        LauncherMenuAction::UpdateSettings => LauncherAction::UpdateSettings,
    }
}
fn from_view(page: PageId) -> Page {
    match page {
        PageId::Home => Page::Home,
        PageId::Settings => Page::Settings,
        PageId::Sandbox => Page::Sandbox,
        PageId::Memory => Page::Memory,
        PageId::Releases => Page::Releases,
        PageId::Gallery => Page::Gallery,
        PageId::Wiki => Page::Wiki,
        PageId::Logs => Page::Logs,
    }
}
fn to_view(page: Page) -> PageId {
    match page {
        Page::Home => PageId::Home,
        Page::Settings => PageId::Settings,
        Page::Sandbox => PageId::Sandbox,
        Page::Memory => PageId::Memory,
        Page::Releases => PageId::Releases,
        Page::Gallery => PageId::Gallery,
        Page::Wiki => PageId::Wiki,
        Page::Logs => PageId::Logs,
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
