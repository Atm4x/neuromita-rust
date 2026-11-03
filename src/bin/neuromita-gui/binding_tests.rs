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
}
