use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc, time::Duration};

use neuromita::localization::{
    FallbackPolicy, LocaleId, LocalizationError, LocalizedText, Localizer, i18n,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{
    launcher_ui::{LauncherUi, load_config},
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
    let _desktop = super::sandbox::attach(
        &window,
        options.data_directory.clone(),
        options.model.clone(),
    )?;
    let catalog = Rc::new(RefCell::new(catalog));
    window
        .global::<I18n>()
        .on_lookup(translation_lookup(catalog.clone()));
    let launcher = Rc::new(RefCell::new(LauncherUi::new(load_config(
        options.unity_directory.as_deref(),
        options.launcher_config.as_deref(),
    )?)));
    let presenter = Rc::new(RefCell::new(ShellPresenter::new(
        HomePreview::python_reference(),
        options.locale,
    )));
    if options.sandbox {
        presenter
            .borrow_mut()
            .dispatch(ShellCommand::Navigate(Page::Sandbox));
    }
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
    window.set_game_running(launcher.borrow_mut().owned_running());

    let weak = window.as_weak();
    let launcher_for_dispatch = launcher.clone();
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
        window.set_game_running(launcher_for_dispatch.borrow_mut().owned_running());
    });
    let handler = dispatch.clone();
    window.on_navigate(move |page| handler(ShellCommand::Navigate(from_view(page))));
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
    let launcher_for_play = launcher.clone();
    window.on_play(move || {
        let notice = launcher_for_play.borrow_mut().toggle_play();
        handler(ShellCommand::LauncherFeedback(notice));
    });
    let handler = dispatch.clone();
    let launcher_for_action = launcher.clone();
    window.on_launcher_action(move |action| {
        let action = launcher_action(action);
        match action {
            LauncherAction::OpenUnityFolder => {
                let notice = launcher_for_action.borrow().open_location();
                handler(ShellCommand::LauncherFeedback(notice));
            }
            action => handler(ShellCommand::Launcher(action)),
        }
    });
    let handler = dispatch.clone();
    window.on_social_selected(move |name| handler(ShellCommand::Social(name.into())));
    window.on_dismiss_notice(move || dispatch(ShellCommand::DismissNotice));
    let timer = slint::Timer::default();
    let weak = window.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(1),
        move || {
            if let Some(window) = weak.upgrade() {
                window.set_game_running(launcher.borrow_mut().owned_running());
            }
        },
    );
    window.run()?;
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
    locale: LocaleId,
    data_directory: PathBuf,
    model: Option<String>,
    sandbox: bool,
    unity_directory: Option<PathBuf>,
    launcher_config: Option<PathBuf>,
}

impl Options {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut options = Self {
            directory: None,
            locale: LocaleId::russian(),
            data_directory: super::runtime::data_directory()?,
            model: None,
            sandbox: false,
            unity_directory: None,
            launcher_config: None,
        };
        let mut arguments = std::env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--sandbox") => options.sandbox = true,
                Some("--unity-dir") => {
                    options.unity_directory = Some(
                        arguments
                            .next()
                            .ok_or("--unity-dir requires a path")?
                            .into(),
                    );
                }
                Some("--launcher-config") => {
                    options.launcher_config = Some(
                        arguments
                            .next()
                            .ok_or("--launcher-config requires a path")?
                            .into(),
                    );
                }
                Some("--model") => {
                    options.model = Some(
                        arguments
                            .next()
                            .ok_or("--model requires a model identifier")?
                            .into_string()
                            .map_err(|_| "model must be UTF-8")?,
                    )
                }
                Some("--data-dir") => {
                    let directory = arguments.next().ok_or("--data-dir requires a path")?;
                    if directory.is_empty() {
                        return Err("--data-dir must not be empty".into());
                    }
                    options.data_directory = directory.into();
                }
                Some("--translations-dir") => {
                    options.directory = Some(
                        arguments
                            .next()
                            .ok_or("--translations-dir requires a path")?
                            .into(),
                    )
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
        Ok(options)
    }
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
        Page::Releases => PageId::Releases,
        Page::Gallery => PageId::Gallery,
        Page::Wiki => PageId::Wiki,
        Page::Logs => PageId::Logs,
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
