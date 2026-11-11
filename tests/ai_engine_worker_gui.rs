#![cfg(feature = "gui")]

use slint::{ComponentHandle, SharedPixelBuffer};
use std::rc::Rc;

slint::slint! {
    import { AiEngineWorkspace, AiEngineUi } from "../ui/settings/ai-engine.slint";
    import { I18n } from "../ui/i18n.slint";
    export { AiEngineUi, I18n }

    export component AiEngineProbe inherits Window {
        preferred-width: 850px;
        preferred-height: 960px;
        background: #0a0a18;
        AiEngineWorkspace {
            width: parent.width;
            height: parent.height;
        }
    }
}

struct HeadlessPlatform;

impl slint::platform::Platform for HeadlessPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

fn render(component: &AiEngineProbe) -> SharedPixelBuffer<slint::Rgba8Pixel> {
    component.window().take_snapshot().unwrap()
}

#[test]
fn actual_ai_engine_workspace_renders_independent_failure_and_recovery_states() {
    slint::platform::set_platform(Box::new(HeadlessPlatform)).unwrap();
    let component = AiEngineProbe::new().unwrap();
    component
        .global::<I18n>()
        .on_lookup(|_, _, _, source| source);
    component.show().unwrap();
    component
        .window()
        .set_size(slint::PhysicalSize::new(850, 960));
    let ui = component.global::<AiEngineUi>();
    let baseline = render(&component);

    ui.set_embedding_status("Python embedding worker exited: code 73".into());
    ui.set_embedding_failed(true);
    ui.set_reranker_status("mock_reranker ModelReady".into());
    ui.set_reranker_failed(false);
    let failed = render(&component);
    assert_ne!(baseline.as_bytes(), failed.as_bytes(), "failure not drawn");
    assert!(ui.get_embedding_failed());
    assert!(!ui.get_reranker_failed());
    assert!(ui.get_embedding_status().contains("code 73"));

    ui.set_embedding_status("mock_embedding ModelReady".into());
    ui.set_embedding_failed(false);
    let recovered = render(&component);
    assert_ne!(
        failed.as_bytes(),
        recovered.as_bytes(),
        "recovery not drawn"
    );
    assert!(!ui.get_embedding_failed());
    assert!(ui.get_embedding_status().contains("mock_embedding"));
    assert_eq!(ui.get_reranker_status(), "mock_reranker ModelReady");

    if let Some(path) = std::env::var_os("NEUROMITA_AI_ENGINE_CAPTURE_DIR") {
        let folder = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&folder).unwrap();
        for (name, pixels) in [("failed", failed), ("recovered", recovered)] {
            let mut ppm = format!("P6\n{} {}\n255\n", pixels.width(), pixels.height()).into_bytes();
            for pixel in pixels.as_bytes().chunks_exact(4) {
                ppm.extend_from_slice(&pixel[..3]);
            }
            std::fs::write(folder.join(format!("ai-engine-{name}.ppm")), ppm).unwrap();
        }
    }
    component.hide().unwrap();
}
