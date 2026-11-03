use super::*;

mod probe {
    slint::slint! {
        import { I18n } from "../../../ui/i18n.slint";
        export { I18n }
        export component TranslationProbe inherits Window {
            out property <string> welcome: I18n.i18n("launcher.home.welcome", "Добро пожаловать!");
            out property <string> more: I18n.i18n("launcher.actions.more", "Дополнительно");
        }
    }
}

struct TestPlatform;
impl slint::platform::Platform for TestPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

#[test]
fn existing_slint_bindings_retranslate_on_locale_change_and_catalog_revision() {
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let catalog = Rc::new(RefCell::new(
        Localizer::bundled(FallbackPolicy::EnglishThenSource).unwrap(),
    ));
    let window = probe::TranslationProbe::new().unwrap();
    let bridge = window.global::<probe::I18n>();
    bridge.on_lookup(translation_lookup(catalog.clone()));
    bridge.set_locale("ru".into());
    assert_eq!(window.get_welcome(), "Добро пожаловать!");
    bridge.set_locale("en".into());
    assert_eq!(window.get_welcome(), "Welcome!");
    assert_eq!(window.get_more(), "More");
    bridge.set_locale("de".into());
    assert_ne!(window.get_welcome(), "Welcome!");
    bridge.set_locale("en".into());
    let candidate = catalog
        .borrow()
        .with_catalog(
            LocaleId::english(),
            r#"{
        "launcher.home.welcome":{"source":"Добро пожаловать!","translation":"Welcome again!"}
    }"#,
        )
        .unwrap();
    *catalog.borrow_mut() = candidate;
    bridge.set_revision(1);
    assert_eq!(window.get_welcome(), "Welcome again!");
    bridge.set_locale("ru".into());
    assert_eq!(window.get_welcome(), "Добро пожаловать!");
    sandbox_render_and_bindings(catalog);
}

fn sandbox_render_and_bindings(catalog: Rc<RefCell<Localizer>>) {
    use super::super::view::{ChatRow, ChatStatus};
    let window = AppWindow::new().unwrap();
    window
        .global::<I18n>()
        .on_lookup(translation_lookup(catalog));
    window.global::<I18n>().set_locale("ru".into());
    window.set_page(PageId::Sandbox);
    window.set_version("v0.1.0".into());
    window.set_core_connected(true);
    window.set_chat_generation_ready(true);
    window.set_chat_model("Gemini · test fixture".into());
    window.set_chat_preset("#0".into());
    window.set_chat_attitude("60.0".into());
    window.set_chat_boredom("10.0".into());
    window.set_chat_stress("5.0".into());
    window.set_chat_active(1);
    window.set_chat_messages(ModelRc::new(VecModel::from(vec![
        ChatRow { text: "Ты тут?".into(), user: true, sequence: "#1".into(), ..Default::default() },
        ChatRow { text: "Конечно я тут! Куда же я от тебя денусь? Рада снова тебя видеть. Чем был занят?".into(), status: ChatStatus::Success, sequence: "#2".into(), ..Default::default() },
        ChatRow { text: "Как твои дела?".into(), user: true, sequence: "#3".into(), ..Default::default() },
        ChatRow { text: "Со мной всё в порядке. Сейчас закончу озвучивать ответ, а пока можешь его прочитать.".into(), status: ChatStatus::Voice, can_cancel: true, request_id: uuid::Uuid::new_v4().to_string().into(), ..Default::default() },
    ])));
    let sent = Rc::new(RefCell::new(String::new()));
    let sent_callback = sent.clone();
    window.on_chat_send(move |text| *sent_callback.borrow_mut() = text.to_string());
    window.invoke_chat_send("typed command".into());
    assert_eq!(&*sent.borrow(), "typed command");
    window.set_chat_draft("draft survives navigation".into());
    window.set_page(PageId::Home);
    window.set_page(PageId::Sandbox);
    assert_eq!(window.get_chat_draft(), "draft survives navigation");
    window.set_chat_draft("".into());
    window.show().unwrap();
    for (width, height, name) in [(1560, 918, "sandbox-ru"), (1200, 760, "sandbox-compact")] {
        window
            .window()
            .set_size(slint::PhysicalSize::new(width, height));
        let image = window.window().take_snapshot().unwrap();
        assert_eq!((image.width(), image.height()), (width, height));
        if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
            for rgba in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(directory.join(format!("{name}.ppm")), ppm).unwrap();
        }
    }
    window
        .window()
        .set_size(slint::PhysicalSize::new(1560, 918));
    window.window().take_snapshot().unwrap();
    for (x, name) in [
        (1260.0, "sandbox-state"),
        (1365.0, "sandbox-debug"),
        (1500.0, "sandbox-collapsed"),
    ] {
        let position = slint::LogicalPosition { x, y: 111.0 };
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        let image = window.window().take_snapshot().unwrap();
        if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
            let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            for rgba in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(PathBuf::from(directory).join(format!("{name}.ppm")), ppm).unwrap();
        }
    }
    window.set_page(PageId::Home);
    window.window().take_snapshot().unwrap();
    window.set_page(PageId::Sandbox);
    window.set_core_connected(false);
    window.set_chat_error("Core is not connected".into());
    let image = window.window().take_snapshot().unwrap();
    if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
        let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
        for rgba in image.as_bytes().as_chunks::<4>().0 {
            ppm.extend_from_slice(&rgba[..3]);
        }
        std::fs::write(
            PathBuf::from(directory).join("sandbox-disconnected-error.ppm"),
            ppm,
        )
        .unwrap();
    }
    window.global::<I18n>().set_locale("en".into());
    window.window().take_snapshot().unwrap();
    settings_render_and_bindings(&window);
    memory_render_and_contract(&window);
    window.hide().unwrap();
}

fn settings_render_and_bindings(window: &AppWindow) {
    use super::super::view::{PresetItem, SettingsUi};
    let settings = window.global::<SettingsUi>();
    settings.set_settings_presets(ModelRc::new(VecModel::from(vec![PresetItem {
        id: "7".into(),
        name: "Main".into(),
        model: "fixture-model".into(),
        endpoint: "http://127.0.0.1:12345/v1beta/".into(),
        key_env: "FIXTURE_KEY".into(),
        temperature: "0.7".into(),
        tokens: "3000".into(),
        timeout: "60".into(),
    }])));
    settings.set_settings_active("7".into());
    settings.set_settings_revision("12".into());
    settings.set_settings_persistence(1);
    settings.set_settings_voice_launcher(true);
    settings.set_settings_volume(65);
    settings.set_mita_reply("active".into());
    settings.set_mita_react("7".into());
    settings.set_mita_initiate("active".into());
    let commands = Rc::new(RefCell::new(Vec::new()));
    let sent = commands.clone();
    super::super::settings::bind(
        window,
        Rc::new(move |command| sent.borrow_mut().push(command)),
    );
    settings.invoke_preset_edit("7".into());
    assert_eq!(settings.get_preset_model(), "fixture-model");
    settings.invoke_preset_save();
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::UpdatePreset(
            neuromita::generation::PresetId(7),
            _
        ))
    ));
    settings.set_preset_temperature("invalid".into());
    let count = commands.borrow().len();
    settings.invoke_preset_save();
    assert_eq!(commands.borrow().len(), count);
    assert!(!settings.get_settings_error().is_empty());
    settings.set_settings_error("".into());
    settings.set_preset_temperature("0.7".into());
    window.set_page(PageId::Settings);
    window.set_core_connected(true);
    window.global::<I18n>().set_locale("ru".into());
    for (width, height) in [(1560, 918), (1200, 760)] {
        window
            .window()
            .set_size(slint::PhysicalSize::new(width, height));
        for section in 0..4 {
            settings.set_section(section);
            let image = window.window().take_snapshot().unwrap();
            if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
                let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                for rgba in image.as_bytes().as_chunks::<4>().0 {
                    ppm.extend_from_slice(&rgba[..3]);
                }
                std::fs::write(
                    PathBuf::from(directory).join(format!("settings-{section}-{width}.ppm")),
                    ppm,
                )
                .unwrap();
            }
        }
    }
    window.global::<I18n>().set_locale("en".into());
    window.window().take_snapshot().unwrap();
}

fn memory_render_and_contract(window: &AppWindow) {
    use super::super::{memory, runtime::Command, view::MemoryUi};
    use neuromita::{
        app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
        character::CharacterId,
        generation::{
            GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
            gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
        },
        memory::{MemoryCommand, MemoryPriority},
    };
    use std::{
        num::NonZeroUsize,
        sync::{Arc, Mutex},
        time::Duration,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let generator = ResponseGenerator::new([
            Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
        ])
        .unwrap();
        let template = GeminiTemplate::google(
            "fixture",
            "fixture-key",
            GenerationSettings::new(0.7, 100, Duration::from_secs(1)).unwrap(),
        )
        .unwrap();
        let limit = NonZeroUsize::new(16).unwrap();
        let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
            GenerationPreset::new(ProviderTemplate::Gemini(template)),
            generator,
            RuntimeLimits {
                request_queue: limit,
                discovery_buffer: limit,
                character_commands: limit,
                presenter_updates: limit,
                retained_finished: limit,
            },
            "127.0.0.1:0".parse().unwrap(),
        ))
        .await
        .unwrap();
        let characters = &app.presenter().characters;
        let initial = characters.memories(CharacterId::Mita).await.unwrap();
        characters
            .edit_memories(
                CharacterId::Mita,
                initial,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Игрок любит зелёный чай.".into(),
                }],
            )
            .await
            .unwrap();
        let state = Arc::new(Mutex::new(memory::MemoryEditor::default()));
        let commands = Rc::new(RefCell::new(Vec::new()));
        let send = commands.clone();
        memory::bind(
            window,
            Rc::new(move |command| send.borrow_mut().push(command)),
            &state,
        );
        let page = characters
            .list_memories(CharacterId::Mita, Default::default())
            .await
            .unwrap();
        memory::render(window, &state, &page);
        let ui = window.global::<MemoryUi>();
        ui.set_open(true);
        ui.invoke_edit(page.records()[0].id.0.to_string().into());
        ui.set_content("Изменённый текст".into());
        let snapshot = characters.memories(CharacterId::Mita).await.unwrap();
        characters
            .edit_memories(
                CharacterId::Mita,
                snapshot,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Новое воспоминание от внешнего клиента.".into(),
                }],
            )
            .await
            .unwrap();
        let page = characters
            .list_memories(CharacterId::Mita, Default::default())
            .await
            .unwrap();
        memory::render(window, &state, &page);
        assert_eq!(ui.get_content(), "Изменённый текст");
        ui.invoke_apply(0);
        let command = commands.borrow_mut().pop().unwrap();
        let Command::MemoryEdit(expected, edits) = command else {
            panic!("expected typed memory edit")
        };
        assert!(
            characters
                .edit_memories(expected.character(), expected, edits)
                .await
                .is_err()
        );
        for (width, height) in [(1560, 918), (1200, 760)] {
            window
                .window()
                .set_size(slint::PhysicalSize::new(width, height));
            let image = window.window().take_snapshot().unwrap();
            if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
                let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                for rgba in image.as_bytes().as_chunks::<4>().0 {
                    ppm.extend_from_slice(&rgba[..3]);
                }
                std::fs::write(
                    PathBuf::from(directory).join(format!("memory-{width}.ppm")),
                    ppm,
                )
                .unwrap();
            }
        }
        ui.set_open(false);
        app.shutdown().await.unwrap();
    });
}
