use std::{cell::RefCell, error::Error, path::PathBuf, process::Command, rc::Rc};

use neuromita::localization::{
    FallbackPolicy, LocaleId, LocalizationError, LocalizedText, Localizer, i18n,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::{
    launcher_ui::{LauncherUi, load_config},
    presenter::{
        ComponentPreview, HomePreview, LauncherAction, Page, ShellCommand, ShellPresenter,
    },
    view::{
        AppWindow, ComponentCard, GuideFocusTarget, I18n, LauncherMenuAction, LocaleOption,
        PageId, ReleaseItem,
    },
    voice::VoicePreviewMode,
};

pub fn run() -> Result<(), Box<dyn Error>> {
    let result = run_inner();
    neuromita::diagnostics::boundary(
        neuromita::diagnostics::Boundary::GuiFinish,
        if result.is_ok() {
            neuromita::diagnostics::Outcome::Completed
        } else {
            neuromita::diagnostics::Outcome::Failed
        },
    );
    result
}

fn run_inner() -> Result<(), Box<dyn Error>> {
    let options = Options::parse()?;
    let diagnostics = neuromita::diagnostics::init().map_err(std::io::Error::other)?;
    let root = neuromita::portable::PortableDataRoot::explicit(options.data_directory.clone())?;
    diagnostics.attach(&root)?;
    neuromita::diagnostics::boundary(
        neuromita::diagnostics::Boundary::GuiStart,
        neuromita::diagnostics::Outcome::Started,
    );
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
    let requires_consent = neuromita::onboarding::pending(&options.data_directory)?;
    window.set_onboarding_required(requires_consent);
    let initial_code = options.locale.as_str();
    window.set_onboarding_language(initial_code.into());
    window.set_onboarding_eula_text(neuromita::onboarding::eula_text(initial_code).into());
    window.set_onboarding_guide_level(
        neuromita::onboarding::selected_guide_level(&options.data_directory)?.index(),
    );
    update_guide(&window, 0);
    let desktop = Rc::new(RefCell::new(None));
    if !requires_consent {
        *desktop.borrow_mut() = Some(super::sandbox::attach(
            &window,
            options.data_directory.clone(),
            options.model.clone(),
        )?);
    }
    let catalog = Rc::new(RefCell::new(catalog));
    window
        .global::<I18n>()
        .on_lookup(translation_lookup(catalog.clone()));
    let launcher = Rc::new(RefCell::new(LauncherUi::new(
        load_config(
            options.unity_directory.as_deref(),
            options.launcher_config.as_deref(),
        )?,
        window.as_weak(),
    )?));
    let presenter = Rc::new(RefCell::new(ShellPresenter::new(
        HomePreview::runtime(),
        options.locale,
    )));
    if options.voice_preview.is_some() || options.asr_preview {
        presenter
            .borrow_mut()
            .dispatch(ShellCommand::Navigate(Page::Settings));
    } else if options.sandbox {
        presenter
            .borrow_mut()
            .dispatch(ShellCommand::Navigate(Page::Sandbox));
    }
    window.set_version(format!("v{}", env!("CARGO_PKG_VERSION")).into());
    window.set_releases(ModelRc::new(VecModel::<ReleaseItem>::default()));
    render(&window, &presenter.borrow(), &catalog.borrow());
    if let Some(mode) = options.voice_preview {
        super::voice::preview(&window, mode);
    }
    if options.asr_preview {
        super::asr::preview(&window);
    }

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
    let guide_window = window.as_weak();
    window.on_onboarding_guide_navigate(move |index| {
        if let Some(window) = guide_window.upgrade() {
            update_guide(&window, index);
        }
    });
    let guide_window = window.as_weak();
    window.on_onboarding_guide_select_step(move |number| {
        let Some(window) = guide_window.upgrade() else {
            return;
        };
        set_guide_focus(&window, number);
    });
    let guide_window = window.as_weak();
    let guide_runtime = desktop.clone();
    window.on_onboarding_guide_select_level(move |index| {
        let Some(window) = guide_window.upgrade() else {
            return;
        };
        let level = neuromita::onboarding::guide::GuideLevel::from_index(index);
        if level.index() == window.get_onboarding_guide_level() {
            return;
        }
        let runtime = guide_runtime.borrow();
        let Some(runtime) = runtime.as_ref() else {
            window.set_notice("Guide settings are unavailable until the runtime starts".into());
            return;
        };
        if let Err(error) = runtime
            .commands
            .try_send(super::runtime::Command::SetGuideLevel(level))
        {
            window.set_notice(format!("Failed to queue Guide level change: {error}").into());
        }
    });
    let language_window = window.as_weak();
    window.on_onboarding_language_selected(move |code| {
        if let Some(window) = language_window.upgrade() {
            window.set_onboarding_language(code.clone());
            window.set_onboarding_eula_text(neuromita::onboarding::eula_text(&code).into());
            update_guide(&window, window.get_onboarding_guide_page());
        }
    });
    let accept_window = window.as_weak();
    let accept_directory = options.data_directory.clone();
    let accept_model = options.model.clone();
    let accept_desktop = desktop.clone();
    let accept_dispatch = dispatch.clone();
    window.on_onboarding_accept(move || {
        let Some(window) = accept_window.upgrade() else {
            return;
        };
        let code = window.get_onboarding_language();
        if let Err(error) = neuromita::onboarding::accept(&accept_directory, &code) {
            window.set_notice(format!("Не удалось сохранить согласие: {error}").into());
            return;
        }
        window.set_onboarding_required(false);
        update_guide(&window, 0);
        window.set_onboarding_guide_visible(true);
        match super::sandbox::attach(&window, accept_directory.clone(), accept_model.clone()) {
            Ok(runtime) => {
                *accept_desktop.borrow_mut() = Some(runtime);
            }
            Err(error) => window.set_notice(error.to_string().into()),
        }
        if let Ok(locale) = LocaleId::new(&code) {
            accept_dispatch(ShellCommand::SelectLanguage(locale));
        }
    });
    let reject_window = window.as_weak();
    window.on_onboarding_reject(move || {
        if let Some(window) = reject_window.upgrade() {
            let _ = window.hide();
        }
    });
    let handler = dispatch.clone();
    let nav_window = window.as_weak();
    window.on_navigate(move |page| {
        handler(ShellCommand::Navigate(from_view(page)));
        if page == PageId::Releases
            && let Some(window) = nav_window.upgrade()
            && !window.get_release_loading()
            && window.get_release_prereleases().row_count() == 0
            && window.get_release_releases().row_count() == 0
            && window.get_release_error().is_empty()
        {
            window.invoke_releases_refresh();
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
    let link_window = window.as_weak();
    window.on_external_link(move |url| {
        if let Err(error) = open_project_url(&url)
            && let Some(window) = link_window.upgrade()
        {
            window.set_notice(error.into());
        }
    });
    let result = window.run();
    let shutdown = launcher.borrow_mut().shutdown();
    result?;
    shutdown.map_err(std::io::Error::other)?;
    Ok(())
}

pub(super) fn sync_guide_level(
    window: &AppWindow,
    next: neuromita::onboarding::guide::GuideLevel,
) {
    use neuromita::onboarding::guide::{self, GuideLevel};

    let previous = GuideLevel::from_index(window.get_onboarding_guide_level());
    if previous == next {
        return;
    }
    let destination = guide::remap_index(
        previous,
        window.get_onboarding_guide_page() as usize,
        next,
    );
    window.set_onboarding_guide_level(next.index());
    update_guide(window, destination as i32);
}

fn set_guide_focus(window: &AppWindow, number: i32) {
    use neuromita::onboarding::guide::{self, GuideLevel};

    let level = GuideLevel::from_index(window.get_onboarding_guide_level());
    let Some(page) = guide::page(level, window.get_onboarding_guide_page() as usize) else {
        return;
    };
    let chosen = u8::try_from(number).ok().and_then(|number| {
        page.focus_steps().iter().find(|target| target.number == number)
    });
    match chosen {
        Some(step) => {
            window.set_onboarding_guide_focus_step(i32::from(step.number));
            window.set_onboarding_guide_focus_x(step.rect.left + step.rect.width / 2.0);
            window.set_onboarding_guide_focus_y(step.rect.top + step.rect.height / 2.0);
            window.set_onboarding_guide_zoom(window.get_onboarding_guide_zoom().max(1.35));
        }
        None => {
            window.set_onboarding_guide_focus_step(0);
            window.set_onboarding_guide_focus_x(0.5);
            window.set_onboarding_guide_focus_y(0.5);
        }
    }
}

fn update_guide(window: &AppWindow, requested_index: i32) {
    use neuromita::onboarding::guide::{self, GuideLevel};

    let level = GuideLevel::from_index(window.get_onboarding_guide_level());
    let count = guide::count(level);
    let index = requested_index.clamp(0, (count - 1) as i32);
    let page = guide::page(level, index as usize).expect("guide has a visible page");
    let language = window.get_onboarding_language();
    if window.get_onboarding_guide_page() != index {
        window.set_onboarding_guide_zoom(1.0);
        window.set_onboarding_guide_focus_step(0);
        window.set_onboarding_guide_focus_x(0.5);
        window.set_onboarding_guide_focus_y(0.5);
    }
    window.set_onboarding_guide_count(count as i32);
    window.set_onboarding_guide_page(index);
    window.set_onboarding_guide_title(page.title(&language).into());
    window.set_onboarding_guide_description(page.description(&language).into());
    window.set_onboarding_guide_image(page.image_key.into());
    window.set_onboarding_guide_wiki_url(page.wiki_url(&language).unwrap_or_default().into());
    let crop = page.image_crop();
    window.set_onboarding_guide_crop_left(crop.left);
    window.set_onboarding_guide_crop_top(crop.top);
    window.set_onboarding_guide_crop_right(crop.right);
    window.set_onboarding_guide_crop_bottom(crop.bottom);
    let targets = page
        .focus_steps()
        .iter()
        .map(|step| GuideFocusTarget {
            number: i32::from(step.number),
            left: step.rect.left,
            top: step.rect.top,
            width: step.rect.width,
            height: step.rect.height,
        })
        .collect::<Vec<_>>();
    window.set_onboarding_guide_targets(ModelRc::new(VecModel::from(targets)));
}

fn render(window: &AppWindow, presenter: &ShellPresenter, catalog: &Localizer) {
    let localize =
        |text: &LocalizedText| display_translation(catalog.resolve(presenter.locale(), text));
    let translations = window.global::<I18n>();
    translations.set_locale(presenter.locale().as_str().into());
    if !window.get_onboarding_required() {
        window.set_onboarding_language(presenter.locale().as_str().into());
        update_guide(window, window.get_onboarding_guide_page());
    }
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
    voice_preview: Option<VoicePreviewMode>,
    asr_preview: bool,
    unity_directory: Option<PathBuf>,
    launcher_config: Option<PathBuf>,
}

impl Options {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut options = Self {
            directory: None,
            locale: LocaleId::russian(),
            data_directory: PathBuf::new(),
            model: None,
            sandbox: false,
            voice_preview: None,
            asr_preview: false,
            unity_directory: None,
            launcher_config: None,
        };
        let mut explicit_data_directory = None;
        let mut explicit_language = false;
        let mut arguments = std::env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--sandbox") => options.sandbox = true,
                Some("--asr-preview") => options.asr_preview = true,
                Some("--voice-preview") => {
                    let mode = arguments.next().ok_or("--voice-preview requires a mode")?;
                    options.voice_preview = Some(VoicePreviewMode::parse(
                        mode.to_str().ok_or("voice preview mode must be UTF-8")?,
                    )?);
                }
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
                    explicit_data_directory = Some(directory.into());
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
                    explicit_language = true;
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
        let root = neuromita::portable::PortableDataRoot::resolve(explicit_data_directory)?;
        root.ensure_writable()?;
        options.data_directory = root.into_path_buf();
        if !explicit_language {
            let path = options.data_directory.join("settings.json");
            if path.exists() {
                use neuromita::settings::{JsonSettingsRepository, Language, SettingsRepository};
                let stored = JsonSettingsRepository::new(path).load()?;
                options.locale = match stored.settings.interface.language {
                    Language::Russian => LocaleId::russian(),
                    Language::English => LocaleId::english(),
                };
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
        PageId::About => Page::About,
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
        Page::About => PageId::About,
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;

fn open_project_url(url: &str) -> Result<(), String> {
    let permitted = [
        "https://github.com/Atm4x/NeuroMita",
        "https://github.com/Atm4x/neuromita-rust",
    ];
    if !permitted
        .iter()
        .any(|base| url == *base || url.starts_with(&format!("{base}/")))
    {
        return Err("Недопустимая ссылка на внешний ресурс".into());
    }
    open_external_url(url)
}

pub(super) fn open_external_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        command.arg(url);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .map_err(|error| format!("Не удалось открыть ссылку: {error}"))?;
    Ok(())
}
