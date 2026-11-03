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
            for rgba in image.as_bytes().chunks_exact(4) {
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
            for rgba in image.as_bytes().chunks_exact(4) {
                ppm.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(PathBuf::from(directory).join(format!("{name}.ppm")), ppm).unwrap();
        }
    }
    window.global::<I18n>().set_locale("en".into());
    window.window().take_snapshot().unwrap();
    window.hide().unwrap();
}
