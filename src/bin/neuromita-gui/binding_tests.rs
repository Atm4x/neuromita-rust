use super::*;

mod probe {
    slint::slint! {
        import { I18n } from "../../../ui/i18n.slint";
        import { CollapsibleSettingsSection } from "../../../ui/components/collapsible-section.slint";
        import { SettingsTheme } from "../../../ui/settings/theme.slint";
        import { SettingsScrollView } from "../../../ui/settings/scroll-view.slint";
        import { SettingsToggleRow, SettingsNumberRow, SettingsButtonRow } from "../../../ui/components/settings-kit.slint";
        import { CharacterVoiceTabs } from "../../../ui/components/character-voice-tabs.slint";
        import { CharacterCatalog, CharacterOption } from "../../../ui/characters/catalog.slint";
        export { CharacterCatalog, CharacterOption }
        export { I18n }
        export component VoiceTabsProbe inherits Window {
            preferred-width: 600px; preferred-height: 234px;
            background: SettingsTheme.canvas;
            CharacterVoiceTabs { selected: 0; }
        }
        export component ControlsProbe inherits Window {
            preferred-width: 400px; preferred-height: 140px;
            out property <length> row-top: row.y;
            VerticalLayout { padding: 0px; spacing: 0px;
                CollapsibleSettingsSection { title: "Title"; subtitle: "Subtitle"; glyph: "\u{f1de}"; }
                row := SettingsToggleRow { title: "Toggle"; height: 60px; checked: true; }
                Rectangle { vertical-stretch: 1; }
            }
        }
        export component TranslationProbe inherits Window {
            out property <string> welcome: I18n.i18n("launcher.home.welcome", "Добро пожаловать!");
            out property <string> more: I18n.i18n("launcher.actions.more", "Дополнительно");
        }
        export component RowSizingProbe inherits Window {
            preferred-width: 500px; preferred-height: 200px;
            out property <length> first-height: first.height;
            out property <length> second-top: second.y;
            out property <length> second-bottom: second.y + second.height;
            out property <length> button-top: button.y;
            VerticalLayout { padding: 0px; spacing: 10px; alignment: start;
                first := SettingsNumberRow { title: "Minimum rounds"; value: 1; }
                second := SettingsNumberRow { title: "Maximum turns"; value: 24; }
                button := SettingsButtonRow { title: "Launch game"; button-text: "Launch"; }
            }
        }
        export component AccordionProbe inherits Window {
            background: SettingsTheme.panel;
            preferred-width: 500px;
            preferred-height: 600px;
            in-out property <bool> expanded;
            out property <length> section-height: section.height;
            out property <length> next-top: next.y;
            VerticalLayout {
                padding: 0px; spacing: 12px;
                section := CollapsibleSettingsSection {
                    title: "Geometry probe";
                    expanded: root.expanded;
                    Rectangle { height: 300px; }
                }
                next := Rectangle { height: 50px; }
                Rectangle { vertical-stretch: 1; }
            }
        }
        export component OverflowProbe inherits Window {
            preferred-width: 500px; preferred-height: 260px;
            in-out property <int> expanded-count: 3;
            in-out property <length> payload-height: 300px;
            in-out property <length> offset <=> scroll.content-y;
            out property <length> first-height: first.height;
            out property <length> second-height: second.height;
            out property <length> third-height: third.height;
            out property <length> last-bottom: last.y + last.height;
            out property <length> content-height: sections.preferred-height;
            out property <bool> scrollable: scroll.scrollable;
            out property <length> viewport-width: scroll.visible-width;
            out property <length> viewport-left: sections.x;
            scroll := SettingsScrollView {
                width: parent.width; height: parent.height;
                content-height: sections.preferred-height;
                sections := VerticalLayout {
                    width: scroll.visible-width; padding: 0px; spacing: 12px; alignment: start;
                    first := CollapsibleSettingsSection { expanded: root.expanded-count > 0; Rectangle { height: root.payload-height; } }
                    second := CollapsibleSettingsSection { expanded: root.expanded-count > 1; Rectangle { height: 200px; } }
                    third := CollapsibleSettingsSection { expanded: root.expanded-count > 2; Rectangle { height: 300px; } }
                    last := Rectangle { height: 40px; }
                }
            }
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
    let controls = probe::ControlsProbe::new().unwrap();
    controls.show().unwrap();
    controls
        .window()
        .set_size(slint::PhysicalSize::new(400, 140));
    let image = controls.window().take_snapshot().unwrap();
    let green_rows = image
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, pixel)| {
            pixel[1] > pixel[0].saturating_add(40) && pixel[1] > pixel[2].saturating_add(30)
        })
        .map(|(index, _)| (index / 400) as f32)
        .collect::<Vec<_>>();
    assert!(!green_rows.is_empty());
    assert!(green_rows[0] > controls.get_row_top() + 17.0);
    assert!(*green_rows.last().unwrap() < controls.get_row_top() + 43.0);
    controls.hide().unwrap();
    let voice = probe::VoiceTabsProbe::new().unwrap();
    voice
        .global::<probe::CharacterCatalog>()
        .set_entries(ModelRc::new(VecModel::from(
            neuromita::character::character_catalog()
                .enumerate()
                .map(|(index, item)| probe::CharacterOption {
                    index: index as i32,
                    id: item.id.as_str().into(),
                    name: item.name.source().into(),
                    name_key: item.name.id().into(),
                    conversation_name: item.conversation_name.source().into(),
                    conversation_key: item.conversation_name.id().into(),
                    initial: item.id.as_str().chars().next().unwrap().to_string().into(),
                })
                .collect::<Vec<_>>(),
        )));
    voice.show().unwrap();
    voice.window().set_size(slint::PhysicalSize::new(600, 234));
    let surface = voice.window().take_snapshot().unwrap();
    let pixel = |x: usize, y: usize| {
        let offset = (y * 600 + x) * 4;
        &surface.as_bytes()[offset..offset + 3]
    };
    assert_eq!(
        pixel(248, 107),
        &[10, 10, 24],
        "inactive tabs must leave a gap before the editor surface"
    );
    let arrow_rows = (27..109)
        .filter(|y| (570..596).any(|x| pixel(x, *y).iter().all(|channel| *channel > 200)))
        .collect::<Vec<_>>();
    assert!(
        !arrow_rows.is_empty(),
        "enabled right scroll arrow must be visible"
    );
    let arrow_center = (arrow_rows[0] + arrow_rows[arrow_rows.len() - 1]) / 2;
    assert!(
        (60..80).contains(&arrow_center),
        "scroll arrows must be vertically centered in the tab strip: {arrow_center}"
    );
    voice.hide().unwrap();
    let rows = probe::RowSizingProbe::new().unwrap();
    rows.show().unwrap();
    rows.window().set_size(slint::PhysicalSize::new(500, 200));
    rows.window().take_snapshot().unwrap();
    assert!(rows.get_first_height() >= 40.0);
    assert!(rows.get_second_top() >= rows.get_first_height() + 9.0);
    assert!(rows.get_button_top() >= rows.get_second_bottom() + 9.0);
    rows.hide().unwrap();
    let accordion = probe::AccordionProbe::new().unwrap();
    accordion.show().unwrap();
    accordion
        .window()
        .set_size(slint::PhysicalSize::new(500, 600));
    accordion.window().take_snapshot().unwrap();
    let closed = (accordion.get_section_height(), accordion.get_next_top());
    assert!((closed.0 - 50.0).abs() < 1.0);
    accordion.set_expanded(true);
    let expanded = accordion.window().take_snapshot().unwrap();
    assert!(accordion.get_next_top() > closed.1 + 300.0);
    let pixel = |x: usize, y: usize| {
        let offset = (y * expanded.width() as usize + x) * 4;
        &expanded.as_bytes()[offset..offset + 3]
    };
    assert_ne!(
        pixel(250, 20),
        pixel(250, 100),
        "expanded body must retain its QSS background layer"
    );
    assert_ne!(
        pixel(0, 100),
        pixel(250, 100),
        "content must not paint over the section border"
    );
    accordion.set_expanded(false);
    accordion.window().take_snapshot().unwrap();
    assert_eq!(
        (accordion.get_section_height(), accordion.get_next_top()),
        closed
    );
    accordion.hide().unwrap();
    let overflow = probe::OverflowProbe::new().unwrap();
    overflow.show().unwrap();
    overflow
        .window()
        .set_size(slint::PhysicalSize::new(500, 260));
    overflow.window().take_snapshot().unwrap();
    assert!(overflow.get_scrollable());
    assert_eq!(overflow.get_viewport_left(), 0.0);
    let viewport_width = overflow.get_viewport_width();
    assert!(viewport_width < 500.0 && viewport_width > 480.0);
    assert!(overflow.get_first_height() > 350.0);
    assert!(overflow.get_second_height() > 250.0);
    assert!(overflow.get_third_height() > 350.0);
    let first_height = overflow.get_first_height();
    overflow.set_expanded_count(1);
    overflow.window().take_snapshot().unwrap();
    assert_eq!(overflow.get_first_height(), first_height);
    overflow.set_payload_height(500.0);
    overflow.set_expanded_count(3);
    overflow.window().take_snapshot().unwrap();
    assert!(overflow.get_first_height() > first_height + 199.0);
    overflow.set_offset(260.0 - overflow.get_content_height());
    overflow.window().take_snapshot().unwrap();
    assert!((overflow.get_last_bottom() + overflow.get_offset() - 260.0).abs() < 1.0);
    overflow.set_expanded_count(0);
    overflow.window().take_snapshot().unwrap();
    assert!(!overflow.get_scrollable());
    assert_eq!(overflow.get_viewport_width(), viewport_width);
    assert_eq!(overflow.get_offset(), 0.0);
    overflow.hide().unwrap();
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
    use super::super::view::{ChatRow, ChatStatus, ReleaseEntry};
    let window = AppWindow::new().unwrap();
    window
        .global::<I18n>()
        .on_lookup(translation_lookup(catalog));
    window.global::<I18n>().set_locale("ru".into());
    super::super::sandbox::render_catalog(
        &window,
        neuromita::character::character_catalog().collect(),
    );
    let catalog = window
        .global::<super::super::view::CharacterCatalog>()
        .get_entries();
    assert_eq!(
        catalog.row_count(),
        neuromita::character::CharacterId::ALL.len()
    );
    for id in neuromita::character::CharacterId::ALL {
        let row = catalog.row_data(id.index()).unwrap();
        assert_eq!(row.id, id.as_str());
        assert_eq!(row.index, id.index() as i32);
    }
    assert_eq!(catalog.row_data(0).unwrap().name, "Безумная Мита");
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
    window.set_release_releases(ModelRc::new(VecModel::from(vec![
        ReleaseEntry {
            id: "2026.11.01".into(),
            title: "NeuroMita v2026.11.01".into(),
            date: "2026-11-01".into(),
            summary: "Тестовый интеграционный релиз / Integration test release.".into(),
            details: "Релиз включает обновления интерфейса и основные исправления.".into(),
            url: "https://github.com/Atm4x/NeuroMita/releases/tag/v2026.11.01".into(),
            prerelease: false,
        },
        ReleaseEntry {
            id: "2026.10.22".into(),
            title: "NeuroMita v2026.10.22".into(),
            date: "2026-10-22".into(),
            summary: "Тестовая сборка / Test build.".into(),
            details: String::new().into(),
            url: "https://github.com/Atm4x/NeuroMita/releases/tag/v2026.10.22".into(),
            prerelease: false,
        },
    ])));
    for (page, filename) in [
        (PageId::Releases, "releases-ru.ppm"),
        (PageId::About, "about-ru.ppm"),
    ] {
        window.set_page(page);
        let image = window.window().take_snapshot().unwrap();
        assert_eq!((image.width(), image.height()), (1560, 918));
        if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            for rgba in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(directory.join(filename), ppm).unwrap();
        }
    }
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

#[test]
fn chat_refresh_and_live_reconcile_preserves_model_and_deduplicates_rows() {
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    use super::super::sandbox::reconcile_rows;
    use super::super::view::{ChatRow, ChatStatus};

    let user = ChatRow {
        sequence: "#1".into(),
        text: "player".into(),
        user: true,
        ..Default::default()
    };
    let pending = ChatRow {
        request_id: "1234".into(),
        text: "draft".into(),
        status: ChatStatus::Text,
        ..Default::default()
    };
    let accepted = ChatRow {
        sequence: "#2".into(),
        text: "final".into(),
        status: ChatStatus::Success,
        ..Default::default()
    };
    reconcile_rows(&window, vec![user.clone(), pending.clone()]);
    let model = window.get_chat_messages();
    assert_eq!(model.row_count(), 2);
    reconcile_rows(&window, vec![user.clone(), pending.clone()]);
    assert_eq!(
        model,
        window.get_chat_messages(),
        "refresh must retain the model"
    );
    assert_eq!(model.row_count(), 2);

    let streaming = ChatRow {
        text: "draft updated".into(),
        ..pending.clone()
    };
    reconcile_rows(&window, vec![user.clone(), streaming.clone()]);
    assert_eq!(model, window.get_chat_messages());
    assert_eq!(model.row_data(1), Some(streaming));
    reconcile_rows(&window, vec![user.clone(), accepted.clone()]);
    reconcile_rows(&window, vec![user.clone(), accepted.clone()]);
    assert_eq!(
        model.row_count(),
        2,
        "accepted message replaces the request row"
    );
    assert_eq!(model.row_data(1), Some(accepted.clone()));

    let newest = ChatRow {
        sequence: "#3".into(),
        text: "newer".into(),
        ..Default::default()
    };
    reconcile_rows(&window, vec![accepted.clone(), newest.clone()]);
    assert_eq!(model, window.get_chat_messages());
    assert_eq!(model.row_count(), 2);
    assert_eq!(model.row_data(0), Some(accepted));
    assert_eq!(model.row_data(1), Some(newest));

    let other_branch = ChatRow {
        sequence: "#1".into(),
        text: "other branch".into(),
        user: true,
        ..Default::default()
    };
    reconcile_rows(&window, vec![other_branch.clone()]);
    assert_eq!(model.row_count(), 1);
    assert_eq!(model.row_data(0), Some(other_branch));
    window.hide().unwrap();
}

#[test]
fn chat_feedback_and_asr_callbacks_are_scoped_to_selection_revision() {
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    let editor = std::sync::Arc::new(std::sync::Mutex::new(
        super::super::memory::MemoryEditor::default(),
    ));
    let commands = Rc::new(RefCell::new(Vec::new()));
    let collected = commands.clone();
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    super::super::sandbox::bind_chat_actions(
        &window,
        Rc::new(move |command| collected.borrow_mut().push(command)),
        revision.clone(),
    );
    use super::super::runtime::{ChatOrigin, Command, Update};
    use neuromita::character::CharacterId;
    let first = ChatOrigin {
        character: CharacterId::Crazy,
        selection_revision: 0,
    };
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    window.invoke_chat_attach("identical draft".into(), "/test/identical.png".into());
    window.invoke_chat_select_character(CharacterId::Kind.index() as i32);
    assert_eq!(
        window.get_chat_character(),
        CharacterId::Kind.index() as i32
    );
    window.set_chat_draft("identical draft".into());
    window.set_chat_attachment_path("/test/identical.png".into());
    super::super::sandbox::apply(
        &window,
        Update::FileAttached(
            first,
            "identical draft".into(),
            "/test/identical.png".into(),
        ),
        &editor,
        &revision,
    );
    assert_eq!(
        window.get_chat_draft(),
        "identical draft",
        "late A completion must not clear B draft"
    );
    assert_eq!(window.get_chat_attachment_path(), "/test/identical.png");
    let old_draft = window.get_chat_draft();
    super::super::sandbox::apply(
        &window,
        Update::RecognizedText {
            origin: first,
            text: "obsolete ASR".into(),
            auto_send_after: Some(std::time::Duration::ZERO),
            merge_with_draft: true,
        },
        &editor,
        &revision,
    );
    assert_eq!(window.get_chat_draft(), old_draft);
    window.set_chat_error("B error".into());
    super::super::sandbox::apply(
        &window,
        Update::ChatError(first, "A error".into()),
        &editor,
        &revision,
    );
    assert_eq!(
        window.get_chat_error(),
        "B error",
        "stale file failure must not overwrite B error"
    );
    window.invoke_chat_attach("B draft".into(), "/test/b.png".into());
    window.invoke_chat_select_character(CharacterId::Crazy.index() as i32);
    assert_eq!(
        window.get_chat_character(),
        CharacterId::Crazy.index() as i32
    );
    super::super::sandbox::apply(
        &window,
        Update::FileAttached(
            first,
            "identical draft".into(),
            "/test/identical.png".into(),
        ),
        &editor,
        &revision,
    );
    assert_eq!(
        window.get_chat_draft(),
        "identical draft",
        "reselected A is a distinct selection"
    );
    assert_eq!(window.get_chat_attachment_path(), "/test/identical.png");
    super::super::sandbox::apply(
        &window,
        Update::RecognizedText {
            origin: first,
            text: "old A ASR".into(),
            auto_send_after: Some(std::time::Duration::ZERO),
            merge_with_draft: false,
        },
        &editor,
        &revision,
    );
    assert_eq!(window.get_chat_draft(), "identical draft");
    super::super::sandbox::apply(
        &window,
        Update::RecognizedText {
            origin: ChatOrigin {
                character: CharacterId::Crazy,
                selection_revision: 2,
            },
            text: "current ASR".into(),
            auto_send_after: None,
            merge_with_draft: true,
        },
        &editor,
        &revision,
    );
    assert_eq!(window.get_chat_draft(), "identical draft current ASR");
    window.set_chat_draft("identical draft".into());
    super::super::sandbox::apply(
        &window,
        Update::FileAttached(
            ChatOrigin {
                character: CharacterId::Crazy,
                selection_revision: 2,
            },
            "identical draft".into(),
            "/test/identical.png".into(),
        ),
        &editor,
        &revision,
    );
    assert_eq!(window.get_chat_draft(), "");
    assert_eq!(window.get_chat_attachment_path(), "");
    let cancel_id = uuid::Uuid::new_v4();
    window.invoke_chat_cancel(cancel_id.to_string().into());
    let recorded = commands.borrow();
    assert!(matches!(&recorded[0], Command::AttachFile(origin, _, _) if *origin == first));
    assert!(
        matches!(&recorded[1], Command::Select(origin) if origin.character == CharacterId::Kind && origin.selection_revision == 1)
    );
    assert!(
        matches!(&recorded[2], Command::AttachFile(origin, _, _) if origin.character == CharacterId::Kind && origin.selection_revision == 1)
    );
    assert!(
        matches!(&recorded[3], Command::Select(origin) if origin.character == CharacterId::Crazy && origin.selection_revision == 2)
    );
    assert!(
        matches!(&recorded[4], Command::Cancel(origin, id) if origin.character == CharacterId::Crazy && origin.selection_revision == 2 && id.0 == cancel_id)
    );
    window.hide().unwrap();
}

fn settings_render_and_bindings(window: &AppWindow) {
    use super::super::{memory::MemoryEditor, runtime::Update, sandbox};
    let editor = std::sync::Arc::new(std::sync::Mutex::new(MemoryEditor::default()));
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    use super::super::view::{AiEngineUi, PresetItem, SettingsUi};
    use neuromita::ai_runtime::{
        AiService,
        runtime_status::{LoadedModel, RuntimePhase, RuntimeSnapshot, ServiceRuntimeStatus},
    };
    let mut runtime = RuntimeSnapshot::default();
    assert_eq!(
        window.global::<AiEngineUi>().get_embedding_status(),
        "Не настроен"
    );
    runtime.services.insert(
        AiService::Embedding,
        ServiceRuntimeStatus {
            phase: RuntimePhase::Failed,
            last_error: Some("Timeout: mock embedding infer".into()),
            ..Default::default()
        },
    );
    runtime.services.insert(
        AiService::Reranker,
        ServiceRuntimeStatus {
            phase: RuntimePhase::ModelReady,
            active_model: Some(LoadedModel {
                plugin_id: "mock_reranker".into(),
                asset_revision: None,
            }),
            ..Default::default()
        },
    );
    sandbox::apply(
        window,
        Update::RuntimeStatus(runtime.clone()),
        &editor,
        &revision,
    );
    let engine = window.global::<AiEngineUi>();
    assert!(engine.get_embedding_failed());
    assert!(
        engine
            .get_embedding_status()
            .contains("Timeout: mock embedding infer")
    );
    assert!(!engine.get_reranker_failed());
    assert!(engine.get_reranker_status().contains("mock_reranker"));
    engine.set_process_mode("split".into());
    window.global::<SettingsUi>().set_section(6);
    window.set_page(PageId::Settings);
    let failed_frame = window.window().take_snapshot().unwrap();
    runtime.services.insert(
        AiService::Embedding,
        ServiceRuntimeStatus {
            phase: RuntimePhase::ModelReady,
            active_model: Some(LoadedModel {
                plugin_id: "mock_embedding".into(),
                asset_revision: None,
            }),
            ..Default::default()
        },
    );
    sandbox::apply(
        window,
        Update::RuntimeStatus(runtime.clone()),
        &editor,
        &revision,
    );
    assert!(!engine.get_embedding_failed());
    assert!(engine.get_embedding_status().contains("mock_embedding"));
    assert!(!engine.get_embedding_status().contains("Timeout"));
    let recovered_frame = window.window().take_snapshot().unwrap();
    assert_ne!(failed_frame.as_bytes(), recovered_frame.as_bytes());
    sandbox::apply(
        window,
        Update::RuntimeStatus(RuntimeSnapshot::default()),
        &editor,
        &revision,
    );
    assert_eq!(engine.get_embedding_status(), "Не настроен");
    assert!(!engine.get_embedding_failed());
    let settings = window.global::<SettingsUi>();
    settings.set_preset_templates(ModelRc::new(VecModel::from(
        super::super::settings::provider_templates(
            &neuromita::presentation::PresetsPresenter::providers(),
        ),
    )));
    settings.set_settings_presets(ModelRc::new(VecModel::from(vec![PresetItem {
        key_error: "".into(),
        output_token_wire_key: "maxOutputTokens".into(),
        template_id: "gemini".into(),
        temperature_enabled: true,
        tokens_enabled: true,
        stream: true,
        max_retries: "3".into(),
        provider: "google".into(),
        configured: true,
        id: "7".into(),
        name: "Main".into(),
        model: "fixture-model".into(),
        endpoint: "http://127.0.0.1:12345/v1beta/".into(),
        key_env: "env:FIXTURE_KEY".into(),
        temperature: "0.7".into(),
        tokens: "3000".into(),
        timeout: "60".into(),
    }])));
    settings.set_settings_active("7".into());
    settings.set_settings_revision("12".into());
    settings.set_settings_persistence(1);
    settings.set_settings_voice_launcher(true);
    settings.set_settings_volume(65);
    settings.set_character_presets(ModelRc::new(VecModel::from(
        neuromita::character::CharacterId::ALL
            .map(|_| super::super::view::CharacterPresets {
                reply: "active".into(),
                react: "7".into(),
                initiate: "active".into(),
                split: true,
            })
            .to_vec(),
    )));
    let commands = Rc::new(RefCell::new(Vec::new()));
    let sent = commands.clone();
    super::super::settings::bind(
        window,
        Rc::new(move |command| sent.borrow_mut().push(command)),
    );
    settings.invoke_memory_dedup_toggle(true);
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::SetMemoryDedupEnabled(true))
    ));
    settings.invoke_memory_dedup_threshold_change("0.925".into());
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::SetMemoryDedupThreshold(925))
    ));
    let before_invalid = commands.borrow().len();
    settings.invoke_memory_dedup_threshold_change("1.001".into());
    assert_eq!(commands.borrow().len(), before_invalid);
    assert!(!settings.get_settings_error().is_empty());
    settings.set_settings_error("".into());
    settings.set_memory_dedup_enabled(false);
    settings.set_memory_dedup_threshold("0.800".into());
    assert!(!settings.get_memory_dedup_enabled());
    assert_eq!(settings.get_memory_dedup_threshold(), "0.800");
    settings.invoke_tool_option("calculator".into(), true);
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::SetToolOption(
            super::super::runtime::ToolOption::Calculator,
            true
        ))
    ));
    settings.invoke_tool_depth_change("5".into());
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::SetToolDepth(5))
    ));
    let last_count = commands.borrow().len();
    settings.invoke_tool_depth_change("6".into());
    assert_eq!(last_count, commands.borrow().len());
    assert!(!settings.get_settings_error().is_empty());
    settings.set_settings_error("".into());
    settings.set_character_prompt_sets(ModelRc::new(VecModel::from(
        vec!["Default".into(); neuromita::character::CharacterId::ALL.len()],
    )));
    settings.invoke_character_load(0);
    assert_eq!(settings.get_character_draft_revision(), "12");
    assert_eq!(settings.get_character_draft_id(), 0);
    assert!(!settings.get_character_draft_dirty());
    settings.set_character_draft_prompt("Alternative".into());
    assert!(settings.get_character_draft_dirty());
    settings.invoke_character_load(0);
    assert!(!settings.get_character_draft_dirty());
    assert_eq!(settings.get_character_draft_prompt(), "Default");
    settings.set_character_draft_reply("7".into());
    settings.set_character_draft_react("7".into());
    settings.set_character_profile_pending(true);
    settings.invoke_character_profile_save(
        0,
        "12".into(),
        "Default".into(),
        "7".into(),
        "7".into(),
        "active".into(),
    );
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::SaveCharacterProfile(
            neuromita::character::CharacterId::Crazy,
            12,
            _
        ))
    ));
    let before = commands.borrow().len();
    settings.invoke_character_load(1);
    settings.invoke_character_profile_save(
        0,
        "12".into(),
        "Default".into(),
        "7".into(),
        "7".into(),
        "active".into(),
    );
    assert_eq!(commands.borrow().len(), before);
    assert!(!settings.get_character_profile_pending());
    assert!(!settings.get_character_profile_error().is_empty());
    settings.invoke_character_load(0);
    settings.invoke_preset_edit("7".into());
    assert!(!settings.get_preset_dirty());
    settings.set_preset_stream(false);
    assert!(settings.get_preset_dirty());
    settings.set_preset_stream(true);
    assert!(!settings.get_preset_dirty());
    settings.set_preset_max_retries("4".into());
    assert!(settings.get_preset_dirty());
    settings.set_preset_max_retries("3".into());
    assert!(!settings.get_preset_dirty());
    assert_eq!(settings.get_preset_model(), "fixture-model");
    assert_eq!(
        settings.get_preset_request_url(),
        "http://127.0.0.1:12345/v1beta/models/fixture-model:generateContent"
    );
    settings.set_preset_model("changed-model".into());
    assert_eq!(
        settings.get_preset_request_url(),
        "http://127.0.0.1:12345/v1beta/models/changed-model:generateContent"
    );
    assert_eq!(
        settings.get_preset_endpoint(),
        "http://127.0.0.1:12345/v1beta/"
    );
    settings.set_preset_model("fixture-model".into());
    settings.invoke_preset_save();
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::UpdatePreset(
            neuromita::generation::PresetId(7),
            _
        ))
    ));
    settings.set_preset_stream(false);
    settings.set_preset_max_retries("4".into());
    settings.invoke_preset_save();
    {
        let captured = commands.borrow();
        let Some(super::super::runtime::Command::UpdatePreset(_, edit)) = captured.last() else {
            panic!("expected preset update");
        };
        let connection = edit
            .connection
            .as_ref()
            .expect("expected configured preset");
        assert!(!connection.stream);
        assert_eq!(connection.max_retries, 4);
    }
    settings.set_preset_temperature("invalid".into());
    let count = commands.borrow().len();
    settings.invoke_preset_save();
    assert_eq!(commands.borrow().len(), count);
    assert!(!settings.get_settings_error().is_empty());
    settings.set_settings_error("".into());
    settings.set_preset_temperature("0.7".into());
    settings.set_preset_max_retries("5".into());
    let count = commands.borrow().len();
    settings.invoke_preset_save();
    assert_eq!(commands.borrow().len(), count);
    assert!(!settings.get_settings_error().is_empty());
    settings.set_settings_error("".into());
    settings.set_preset_max_retries("3".into());
    settings.set_preset_stream(true);
    assert!(!settings.get_preset_dirty());
    settings.invoke_preset_edit("7".into());
    super::super::settings::saved(window, None);
    assert_eq!(settings.get_preset_edit_id(), "7");
    settings.invoke_preset_new();
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::NewPreset)
    ));
    let before = commands.borrow().len();
    settings.set_preset_model("modified-before-switch".into());
    assert!(settings.get_preset_dirty());
    settings.invoke_preset_new();
    assert_eq!(commands.borrow().len(), before);
    assert_eq!(settings.get_preset_pending_id(), "__new__");
    settings.invoke_preset_confirm_cancel();
    assert!(settings.get_preset_pending_id().is_empty());
    settings.invoke_preset_new();
    settings.invoke_preset_confirm_discard();
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::NewPreset)
    ));

    window.set_page(PageId::Settings);
    window.set_core_connected(true);
    window.global::<I18n>().set_locale("ru".into());
    for (width, height) in [(1560, 918), (1200, 760)] {
        window
            .window()
            .set_size(slint::PhysicalSize::new(width, height));
        for section in [0, 4, 7, 8, 9] {
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
        super::super::settings::execute(
            app.presenter(),
            super::super::runtime::Command::SelectAsrRecognizer("whisper".into()),
        )
        .unwrap();
        let mut absent_dropdown = app.presenter().settings.asr.preferences.get();
        absent_dropdown.recognizer_id.clear();
        super::super::settings::execute(
            app.presenter(),
            super::super::runtime::Command::SaveAsr(absent_dropdown, None),
        )
        .unwrap();
        assert_eq!(
            app.presenter().settings.asr.preferences.get().recognizer_id,
            "whisper"
        );
        let initial_tools = app.presenter().settings.tools.state.get();
        assert!(!initial_tools.enabled);
        super::super::settings::execute(
            app.presenter(),
            Command::SetToolOption(super::super::runtime::ToolOption::Enabled, true),
        )
        .unwrap();
        super::super::settings::execute(
            app.presenter(),
            Command::SetToolOption(super::super::runtime::ToolOption::Calculator, true),
        )
        .unwrap();
        super::super::settings::execute(app.presenter(), Command::SetToolDepth(5)).unwrap();
        assert!(
            app.presenter()
                .settings
                .tools
                .state
                .get()
                .supports("calculator")
        );
        assert_eq!(app.presenter().settings.tools.state.get().max_depth, 5);
        assert!(
            super::super::settings::execute(app.presenter(), Command::SetToolDepth(6)).is_err()
        );
        assert_eq!(app.presenter().settings.tools.state.get().max_depth, 5);
        assert!(
            super::super::settings::execute(
                app.presenter(),
                Command::SetToolOption(super::super::runtime::ToolOption::WebSearch, true)
            )
            .is_err()
        );
        assert!(
            !app.presenter()
                .settings
                .tools
                .state
                .get()
                .web
                .search_enabled
        );
        let initial_revision = app.presenter().settings.snapshot().unwrap().revision;
        super::super::settings::execute(
            app.presenter(),
            Command::AssignAllPresets(
                CharacterId::Crazy,
                neuromita::settings::PresetSelection::Specific(neuromita::generation::PresetId(0)),
            ),
        )
        .unwrap();
        let after = app.presenter().settings.snapshot().unwrap();
        assert_eq!(after.revision, initial_revision + 1);
        let assignments = after.values.characters.crazy.presets;
        assert_eq!(assignments.reply, assignments.react);
        assert_eq!(assignments.reply, assignments.initiate);
        assert!(
            super::super::settings::execute(
                app.presenter(),
                Command::AssignAllPresets(
                    CharacterId::Crazy,
                    neuromita::settings::PresetSelection::Specific(
                        neuromita::generation::PresetId(999)
                    )
                )
            )
            .is_err()
        );
        assert_eq!(
            app.presenter().settings.snapshot().unwrap().revision,
            after.revision
        );
        let characters = &app.presenter().characters;
        let initial = characters.memories(CharacterId::Crazy).await.unwrap();
        characters
            .edit_memories(
                CharacterId::Crazy,
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
            .list_memories(CharacterId::Crazy, Default::default())
            .await
            .unwrap();
        memory::render(window, &state, &page);
        let ui = window.global::<MemoryUi>();
        ui.set_open(true);
        window
            .global::<super::super::view::SettingsUi>()
            .set_section(2);
        window
            .global::<super::super::view::SettingsUi>()
            .set_character_prompt_expanded(false);
        ui.invoke_edit(page.records()[0].id.0.to_string().into());
        assert!(ui.get_graph_eligible());
        ui.invoke_graph_preview(0);
        assert!(ui.get_graph_pending());
        let Command::GraphPreview(graph_request, graph_character, graph_memory, graph_mode) =
            commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected graph maintenance preview")
        };
        assert_eq!(graph_character, CharacterId::Crazy);
        assert_eq!(graph_memory, page.records()[0].id);
        assert_eq!(
            graph_mode,
            neuromita::knowledge::maintenance::GraphMaintenanceMode::Clear
        );
        memory::graph_previewed(
            window,
            &state,
            uuid::Uuid::new_v4(),
            Ok(crate::runtime::GraphPreviewReport {
                count: 9,
                relations: Vec::new(),
            }),
        );
        assert!(ui.get_graph_pending());
        assert!(!ui.get_graph_confirm_ready());
        memory::graph_previewed(
            window,
            &state,
            graph_request,
            Ok(crate::runtime::GraphPreviewReport {
                count: 1,
                relations: vec![neuromita::memory::ProvenancedRelation {
                    source: graph_memory,
                    source_revision: 3,
                    relation: neuromita::memory::MemoryRelation {
                        subject: "Mita\nspoof".into(),
                        predicate: "likes".into(),
                        object: "tea".into(),
                    },
                }],
            }),
        );
        assert!(!ui.get_graph_pending());
        assert!(ui.get_graph_confirm_ready());
        assert!(ui.get_graph_summary().contains("1 derived"));
        assert!(ui.get_graph_details().contains(&graph_memory.0.to_string()));
        assert!(ui.get_graph_details().contains("revision 3"));
        assert!(ui.get_graph_details().contains("Mita\\nspoof"));
        ui.invoke_graph_confirm();
        assert!(ui.get_graph_pending());
        assert!(!ui.get_graph_confirm_ready());
        let Command::GraphApply(applied_id, applied_character) =
            commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected confirmed graph maintenance")
        };
        assert_eq!(applied_id, graph_request);
        assert_eq!(applied_character, CharacterId::Crazy);
        memory::graph_applied(window, &state, uuid::Uuid::new_v4(), Ok(()));
        assert!(ui.get_graph_pending());
        memory::graph_applied(
            window,
            &state,
            graph_request,
            Err("stale graph preview".into()),
        );
        assert!(!ui.get_graph_pending());
        assert!(ui.get_error().contains("stale graph preview"));
        assert!(!ui.get_graph_confirm_ready());
        ui.invoke_graph_preview(0);
        let Command::GraphPreview(aborted_id, _, _, _) = commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected second graph preview")
        };
        ui.invoke_graph_cancel();
        let Command::GraphAbort(id) = commands.borrow_mut().pop().unwrap() else {
            panic!("expected abort command")
        };
        assert_eq!(id, aborted_id);
        memory::graph_previewed(
            window,
            &state,
            aborted_id,
            Ok(crate::runtime::GraphPreviewReport {
                count: 2,
                relations: Vec::new(),
            }),
        );
        assert!(!ui.get_graph_pending());
        assert!(!ui.get_graph_confirm_ready());
        assert!(ui.get_graph_summary().is_empty());
        assert!(ui.get_graph_details().is_empty());
        ui.set_content("Изменённый текст".into());
        ui.invoke_graph_preview(0);
        let Command::GraphPreview(in_flight_id, _, _, _) = commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected in-flight graph preview")
        };
        memory::graph_previewed(
            window,
            &state,
            in_flight_id,
            Ok(crate::runtime::GraphPreviewReport {
                count: 1,
                relations: Vec::new(),
            }),
        );
        ui.invoke_graph_confirm();
        let Command::GraphApply(confirmed_id, _) = commands.borrow_mut().pop().unwrap() else {
            panic!("expected in-flight confirmed graph apply")
        };
        assert_eq!(confirmed_id, in_flight_id);
        assert!(ui.get_graph_applying());
        ui.invoke_graph_cancel();
        assert!(ui.get_graph_applying());
        let snapshot = characters.memories(CharacterId::Crazy).await.unwrap();
        characters
            .edit_memories(
                CharacterId::Crazy,
                snapshot,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Новое воспоминание от внешнего клиента.".into(),
                }],
            )
            .await
            .unwrap();
        let page = characters
            .list_memories(CharacterId::Crazy, Default::default())
            .await
            .unwrap();
        memory::render(window, &state, &page);
        assert!(ui.get_graph_applying());
        memory::graph_applied(window, &state, confirmed_id, Ok(()));
        assert!(!ui.get_graph_pending());
        assert!(ui.get_graph_summary().contains("Canonical memory retained"));
        assert_eq!(ui.get_content(), "Изменённый текст");

        assert!(!ui.get_graph_reindex_available());
        ui.invoke_graph_preview(1);
        assert!(ui.get_error().contains("unavailable"));
        assert!(commands.borrow().is_empty());
        ui.set_graph_reindex_available(true);
        ui.invoke_graph_preview(1);
        let Command::GraphPreview(reindex_id, reindex_character, reindex_source, reindex_mode) =
            commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected typed graph reindex preview")
        };
        assert_eq!(reindex_character, CharacterId::Crazy);
        assert_eq!(reindex_source, graph_memory);
        assert_eq!(
            reindex_mode,
            neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex
        );
        memory::graph_previewed(
            window,
            &state,
            reindex_id,
            Ok(crate::runtime::GraphPreviewReport {
                count: 3,
                relations: Vec::new(),
            }),
        );
        assert!(ui.get_graph_reindex_confirmation());
        assert!(ui.get_graph_summary().contains("Reindex will replace 3"));
        ui.invoke_graph_confirm();
        let Command::GraphApply(reindex_apply_id, reindex_apply_character) =
            commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected confirmed reindex")
        };
        assert_eq!(reindex_apply_id, reindex_id);
        assert_eq!(reindex_apply_character, CharacterId::Crazy);
        assert!(ui.get_graph_applying());
        memory::graph_applied(window, &state, reindex_id, Ok(()));
        assert!(!ui.get_graph_pending());
        assert!(!ui.get_graph_reindex_confirmation());
        assert!(ui.get_graph_summary().contains("reindexed"));

        ui.invoke_graph_preview(1);
        let Command::GraphPreview(backend_lost_id, _, _, mode) =
            commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected reindex preview after previous apply")
        };
        assert_eq!(
            mode,
            neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex
        );
        ui.set_graph_reindex_available(false);
        memory::graph_previewed(
            window,
            &state,
            backend_lost_id,
            Ok(crate::runtime::GraphPreviewReport {
                count: 4,
                relations: Vec::new(),
            }),
        );
        assert!(!ui.get_graph_confirm_ready());
        assert!(!ui.get_graph_reindex_confirmation());
        assert!(ui.get_error().contains("unavailable"));
        ui.invoke_graph_confirm();
        assert!(commands.borrow().is_empty());
        ui.invoke_apply(0);
        let command = commands.borrow_mut().pop().unwrap();
        let Command::MemoryEdit(edit_id, expected, edits) = command else {
            panic!("expected typed memory edit")
        };
        assert!(
            characters
                .edit_memories(expected.character(), expected, edits)
                .await
                .is_err()
        );
        ui.invoke_edit(page.records()[1].id.0.to_string().into());
        ui.set_content("new draft after save".into());
        memory::saved(window, &state, edit_id);
        assert_eq!(ui.get_content(), "new draft after save");
        assert!(ui.get_editing());
        memory::failed(window, &state, edit_id, "old error".into());
        assert!(ui.get_error().is_empty());

        let snapshot = characters.memories(CharacterId::Crazy).await.unwrap();
        let forgotten_id = snapshot
            .records()
            .iter()
            .find(|record| record.content == "Игрок любит зелёный чай.")
            .unwrap()
            .id;
        characters
            .edit_memories(
                CharacterId::Crazy,
                snapshot,
                vec![MemoryCommand::Forget { id: forgotten_id }],
            )
            .await
            .unwrap();
        let page = characters
            .list_memories(CharacterId::Crazy, Default::default())
            .await
            .unwrap();
        memory::render(window, &state, &page);
        ui.invoke_edit(forgotten_id.0.to_string().into());
        assert!(ui.get_selected_forgotten());
        assert!(!ui.get_graph_eligible());
        ui.set_recall_query("чай".into());
        ui.invoke_recall();
        assert!(ui.get_recall_pending());
        let Command::MemoryRecall(request_id, character, id, query) =
            commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected explicit forgotten recall command");
        };
        assert_eq!(character, CharacterId::Crazy);
        assert_eq!(id, forgotten_id);
        assert_eq!(query, "чай");
        memory::recalled(
            window,
            &state,
            uuid::Uuid::new_v4(),
            Ok(Some("wrong request".into())),
        );
        assert!(ui.get_recall_pending());
        assert!(ui.get_recall_result().is_empty());
        memory::recalled(
            window,
            &state,
            request_id,
            Ok(Some("[FORGOTTEN] Игрок любит зелёный чай.".into())),
        );
        assert!(!ui.get_recall_pending());
        assert!(ui.get_recall_result().contains("[FORGOTTEN]"));
        ui.invoke_edit(
            page.records()
                .iter()
                .find(|record| record.id != forgotten_id)
                .unwrap()
                .id
                .0
                .to_string()
                .into(),
        );
        assert!(!ui.get_selected_forgotten());
        assert!(ui.get_recall_result().is_empty());

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
        ui.invoke_edit(forgotten_id.0.to_string().into());
        ui.set_recall_query("чай".into());
        ui.invoke_recall();
        let Command::MemoryRecall(delayed_id, _, _, _) = commands.borrow_mut().pop().unwrap()
        else {
            panic!("expected delayed historical inspection command");
        };
        ui.invoke_select(CharacterId::Kind.index() as i32);
        memory::recalled(window, &state, delayed_id, Ok(Some("stale secret".into())));
        assert!(ui.get_recall_result().is_empty());
        assert!(!ui.get_recall_pending());
        memory::render(window, &state, &page);
        assert_eq!(ui.get_character(), CharacterId::Kind.index() as i32);
        ui.set_open(false);
        app.shutdown().await.unwrap();
    });
}

#[test]
fn conversation_admin_callbacks_reject_late_a_b_a_results() {
    use super::super::runtime::{ChatOrigin, Command, Update};
    use super::super::view::ConversationUi;
    use neuromita::{character::CharacterId, presentation::ConversationChange};

    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    let editor = std::sync::Arc::new(std::sync::Mutex::new(
        super::super::memory::MemoryEditor::default(),
    ));
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let commands = Rc::new(RefCell::new(Vec::new()));
    let collected = commands.clone();
    super::super::sandbox::bind_chat_actions(
        &window,
        Rc::new(move |command| collected.borrow_mut().push(command)),
        revision.clone(),
    );
    let first = ChatOrigin {
        character: CharacterId::Crazy,
        selection_revision: 0,
    };
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    let conversation = window.global::<ConversationUi>();
    conversation.invoke_preview(1);
    conversation.invoke_trace();
    conversation.set_confirmation(1);
    conversation.set_history(true);
    conversation.set_diagnostics("A-private-trace".into());

    window.invoke_chat_select_character(CharacterId::Kind.index() as i32);
    assert_eq!(conversation.get_confirmation(), 0);
    assert!(!conversation.get_history());
    assert_eq!(conversation.get_diagnostics(), "");
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(first, Some((ConversationChange::ClearMessages, 7))),
        &editor,
        &revision,
    );
    super::super::sandbox::apply(
        &window,
        Update::Diagnostics(first, "A-private-trace".into()),
        &editor,
        &revision,
    );
    super::super::sandbox::apply(
        &window,
        Update::ConversationError(first, "A error".into()),
        &editor,
        &revision,
    );
    assert_eq!(conversation.get_confirmation(), 0);
    assert_eq!(conversation.get_diagnostics(), "");
    assert_eq!(window.get_chat_error(), "");

    window.invoke_chat_select_character(CharacterId::Crazy.index() as i32);
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(first, Some((ConversationChange::ResetContext, 8))),
        &editor,
        &revision,
    );
    super::super::sandbox::apply(
        &window,
        Update::Diagnostics(first, "A-private-trace".into()),
        &editor,
        &revision,
    );
    assert_eq!(conversation.get_confirmation(), 0);
    assert_eq!(conversation.get_diagnostics(), "");

    let selected = ChatOrigin {
        character: CharacterId::Crazy,
        selection_revision: 2,
    };
    conversation.invoke_preview(2);
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(selected, Some((ConversationChange::ResetContext, 3))),
        &editor,
        &revision,
    );
    assert_eq!(conversation.get_confirmation(), 2);
    assert_eq!(conversation.get_count(), 3);
    conversation.invoke_confirm();
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(selected, None),
        &editor,
        &revision,
    );
    assert_eq!(conversation.get_confirmation(), 0);
    conversation.invoke_trace();
    super::super::sandbox::apply(
        &window,
        Update::Diagnostics(selected, "new trace".into()),
        &editor,
        &revision,
    );
    assert_eq!(conversation.get_diagnostics(), "new trace");
    conversation.invoke_abort();

    let recorded = commands.borrow();
    assert!(
        matches!(&recorded[0], Command::Preview(origin, ConversationChange::ClearMessages) if *origin == first)
    );
    assert!(matches!(&recorded[1], Command::Diagnostics(origin) if *origin == first));
    assert!(
        matches!(&recorded[2], Command::Select(origin) if origin.character == CharacterId::Kind && origin.selection_revision == 1)
    );
    assert!(matches!(&recorded[3], Command::Select(origin) if *origin == selected));
    assert!(
        matches!(&recorded[4], Command::Preview(origin, ConversationChange::ResetContext) if *origin == selected)
    );
    assert!(matches!(&recorded[5], Command::Confirm(origin) if *origin == selected));
    assert!(matches!(&recorded[6], Command::Diagnostics(origin) if *origin == selected));
    assert!(matches!(&recorded[7], Command::Abort(origin) if *origin == selected));
}

#[test]
fn chat_single_message_hide_callback_uses_event_id_and_origin_revision() {
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    let editor = std::sync::Arc::new(std::sync::Mutex::new(
        super::super::memory::MemoryEditor::default(),
    ));
    let received = Rc::new(RefCell::new(Vec::new()));
    let collected = received.clone();
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    super::super::sandbox::bind_chat_actions(
        &window,
        Rc::new(move |command| collected.borrow_mut().push(command)),
        revision.clone(),
    );
    use super::super::{
        runtime::{ChatOrigin, Command, Update},
        view::ConversationUi,
    };
    use neuromita::{character::CharacterId, presentation::ConversationChange};
    let target = neuromita::timeline::EventId::new();
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    window.invoke_chat_hide_message(target.0.to_string().into());
    assert!(
        matches!(&received.borrow()[0], Command::Preview(origin, ConversationChange::HideMessage(id))
        if origin.character == CharacterId::Crazy && origin.selection_revision == 0 && *id == target)
    );
    window.invoke_chat_select_character(CharacterId::Kind.index() as i32);
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(
            ChatOrigin {
                character: CharacterId::Crazy,
                selection_revision: 0,
            },
            Some((ConversationChange::HideMessage(target), 1)),
        ),
        &editor,
        &revision,
    );
    assert_eq!(window.global::<ConversationUi>().get_confirmation(), 0);
    window.invoke_chat_select_character(CharacterId::Crazy.index() as i32);
    window.invoke_chat_hide_message(target.0.to_string().into());
    assert!(
        matches!(&received.borrow()[3], Command::Preview(origin, ConversationChange::HideMessage(id))
        if origin.character == CharacterId::Crazy && origin.selection_revision == 2 && *id == target)
    );
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(
            ChatOrigin {
                character: CharacterId::Crazy,
                selection_revision: 2,
            },
            Some((ConversationChange::HideMessage(target), 1)),
        ),
        &editor,
        &revision,
    );
    assert_eq!(window.global::<ConversationUi>().get_confirmation(), 3);
    window.global::<ConversationUi>().invoke_confirm();
    assert!(matches!(&received.borrow()[4], Command::Confirm(origin)
        if origin.character == CharacterId::Crazy && origin.selection_revision == 2));
    window.hide().unwrap();
}

#[test]
fn chat_real_history_pages_are_ticket_fenced_and_preserve_live_rows() {
    use super::super::{
        runtime::{ChatOrigin, Command, Update},
        sandbox::{HistoryPager, apply_with_pager, bind_history_actions, reconcile_rows},
        view::{ChatRow, ChatStatus},
    };
    use neuromita::{
        character::CharacterId,
        presentation::ConversationHistoryPage,
        timeline::{
            ContextAnchor,
            domain::{EventKind, MessageRole, ProjectedMessage},
            selection::TimelineSelection,
        },
    };
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    let editor = std::sync::Arc::new(std::sync::Mutex::new(
        super::super::memory::MemoryEditor::default(),
    ));
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let pager = std::sync::Arc::new(std::sync::Mutex::new(HistoryPager::default()));
    let origin = ChatOrigin {
        character: CharacterId::Crazy,
        selection_revision: 0,
    };
    let branch = uuid::Uuid::new_v4();
    let at = ContextAnchor {
        branch_id: branch,
        through: 205,
    };
    let selected = TimelineSelection {
        branch_id: neuromita::timeline::domain::BranchId(branch),
        generation: 1,
    };
    let commands = Rc::new(RefCell::new(Vec::new()));
    let collected = commands.clone();
    bind_history_actions(
        &window,
        Rc::new(move |command| collected.borrow_mut().push(command)),
        revision.clone(),
        pager.clone(),
    );
    pager.lock().unwrap().observe(origin, at, selected, 205);
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    let existing = (6..=205)
        .map(|i| ChatRow {
            sequence: format!("#{i}").into(),
            event_id: uuid::Uuid::from_u128(i).to_string().into(),
            text: format!("history-{i}").into(),
            user: true,
            ..Default::default()
        })
        .chain(std::iter::once(ChatRow {
            request_id: "pending-live".into(),
            text: "streaming".into(),
            status: ChatStatus::Text,
            can_cancel: true,
            ..Default::default()
        }))
        .collect();
    reconcile_rows(&window, existing);
    let model = window.get_chat_messages();
    window.invoke_chat_load_older();
    window.invoke_chat_load_older();
    assert_eq!(
        commands.borrow().len(),
        1,
        "one bounded read must be in flight"
    );
    let (ticket, offset) = match &commands.borrow()[0] {
        Command::LoadOlder(request_origin, id, request_at, request_selection, offset) => {
            assert_eq!(*request_origin, origin);
            assert_eq!(*request_at, at);
            assert_eq!(*request_selection, selected);
            (*id, *offset)
        }
        _ => panic!("expected typed Timeline history page"),
    };
    assert_eq!(offset, 200);
    let older = (1..=5)
        .map(|i| ProjectedMessage {
            event_id: neuromita::timeline::EventId(uuid::Uuid::from_u128(i)),
            request_id: None,
            kind: EventKind::PlayerMessage,
            message_id: format!("message-{i}"),
            external_message_id: None,
            sequence: i as u64,
            learned_at: i as u64,
            role: MessageRole::User,
            speaker: Some("player".into()),
            content: format!("history-{i}"),
            fields: Default::default(),
        })
        .collect::<Vec<_>>();
    let page = ConversationHistoryPage {
        character: CharacterId::Crazy,
        at,
        selection: selected,
        offset: 200,
        total: 205,
        messages: older.clone(),
    };
    apply_with_pager(
        &window,
        Update::OlderPage(origin, ticket, Ok(page)),
        &editor,
        &revision,
        Some(&pager),
    );
    assert_eq!(
        window.get_chat_messages(),
        model,
        "pages retain mounted Slint model"
    );
    assert_eq!(model.row_count(), 206, "older messages plus live pending");
    assert_eq!(model.row_data(0).unwrap().text, "history-1");
    assert_eq!(model.row_data(204).unwrap().text, "history-205");
    assert_eq!(model.row_data(205).unwrap().request_id, "pending-live");
    assert!(!window.get_chat_has_older());
    window.invoke_chat_load_older();
    assert_eq!(commands.borrow().len(), 1);
    apply_with_pager(
        &window,
        Update::OlderPage(
            origin,
            ticket,
            Ok(ConversationHistoryPage {
                character: CharacterId::Crazy,
                at,
                selection: selected,
                offset: 200,
                total: 205,
                messages: older.clone(),
            }),
        ),
        &editor,
        &revision,
        Some(&pager),
    );
    assert_eq!(
        model.row_count(),
        206,
        "duplicate page cannot append duplicate events"
    );

    let loading_at = ContextAnchor { through: 206, ..at };
    let changed_at = ContextAnchor { through: 207, ..at };
    pager
        .lock()
        .unwrap()
        .observe(origin, loading_at, selected, 206);
    assert_eq!(
        pager.lock().unwrap().visible(),
        206,
        "a new accepted reply must retain loaded older rows and include the new row"
    );
    assert!(
        pager.lock().unwrap().request(origin).is_none(),
        "a live reply must not re-request pages already mounted"
    );
    *pager.lock().unwrap() = HistoryPager::default();
    pager
        .lock()
        .unwrap()
        .observe(origin, loading_at, selected, 205);
    let (late_ticket, _, _, _) = pager.lock().unwrap().request(origin).unwrap();
    pager
        .lock()
        .unwrap()
        .observe(origin, changed_at, selected, 204);
    apply_with_pager(
        &window,
        Update::OlderPage(
            origin,
            late_ticket,
            Ok(ConversationHistoryPage {
                character: CharacterId::Crazy,
                at,
                selection: selected,
                offset: 200,
                total: 205,
                messages: older.clone(),
            }),
        ),
        &editor,
        &revision,
        Some(&pager),
    );
    assert_eq!(
        model.row_count(),
        206,
        "edit/hide during read cannot resurrect old rows"
    );

    pager.lock().unwrap().observe(
        origin,
        at,
        TimelineSelection {
            generation: 2,
            ..selected
        },
        205,
    );
    let (new_ticket, _, _, _) = pager.lock().unwrap().request(origin).unwrap();
    apply_with_pager(
        &window,
        Update::OlderPage(
            origin,
            new_ticket,
            Ok(ConversationHistoryPage {
                character: CharacterId::Crazy,
                at,
                selection: selected,
                offset: 200,
                total: 205,
                messages: older.clone(),
            }),
        ),
        &editor,
        &revision,
        Some(&pager),
    );
    assert_eq!(
        model.row_count(),
        206,
        "same branch/head but different generation must be fenced"
    );

    revision.store(2, std::sync::atomic::Ordering::Release);
    apply_with_pager(
        &window,
        Update::OlderPage(
            origin,
            new_ticket,
            Ok(ConversationHistoryPage {
                character: CharacterId::Crazy,
                at,
                selection: selected,
                offset: 200,
                total: 205,
                messages: older,
            }),
        ),
        &editor,
        &revision,
        Some(&pager),
    );
    assert_eq!(
        model.row_count(),
        206,
        "A→B→A old result cannot mutate current selection"
    );
    window.hide().unwrap();
}

#[test]
fn chat_message_revision_callback_is_fenced_across_character_reselection() {
    use super::super::{
        runtime::{ChatOrigin, Command, Update},
        sandbox::bind_chat_actions,
        view::ConversationUi,
    };
    use neuromita::{character::CharacterId, presentation::ConversationChange};
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    let commands = Rc::new(RefCell::new(Vec::new()));
    let output = commands.clone();
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let editor = std::sync::Arc::new(std::sync::Mutex::new(
        super::super::memory::MemoryEditor::default(),
    ));
    bind_chat_actions(
        &window,
        Rc::new(move |command| output.borrow_mut().push(command)),
        revision.clone(),
    );
    let event = neuromita::timeline::EventId::new();
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    window
        .global::<ConversationUi>()
        .invoke_revise(event.0.to_string().into(), "changed".into());
    assert!(
        matches!(&commands.borrow()[0], Command::Preview(origin, ConversationChange::ReviseMessage(id, content))
        if origin.character == CharacterId::Crazy && origin.selection_revision == 0 && *id == event && content == "changed")
    );
    window.invoke_chat_select_character(CharacterId::Kind.index() as i32);
    let stale = ChatOrigin {
        character: CharacterId::Crazy,
        selection_revision: 0,
    };
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(
            stale,
            Some((
                ConversationChange::ReviseMessage(event, "changed".into()),
                1,
            )),
        ),
        &editor,
        &revision,
    );
    assert_eq!(window.global::<ConversationUi>().get_confirmation(), 0);
    window.invoke_chat_select_character(CharacterId::Crazy.index() as i32);
    assert_eq!(window.global::<ConversationUi>().get_edit_target(), "");
    window
        .global::<ConversationUi>()
        .invoke_revise(event.0.to_string().into(), "current".into());
    assert!(
        matches!(&commands.borrow()[3], Command::Preview(origin, ConversationChange::ReviseMessage(id, content))
        if origin.character == CharacterId::Crazy && origin.selection_revision == 2 && *id == event && content == "current")
    );
    super::super::sandbox::apply(
        &window,
        Update::Confirmation(
            ChatOrigin {
                character: CharacterId::Crazy,
                selection_revision: 2,
            },
            Some((
                ConversationChange::ReviseMessage(event, "current".into()),
                1,
            )),
        ),
        &editor,
        &revision,
    );
    assert_eq!(window.global::<ConversationUi>().get_confirmation(), 4);
    window.global::<ConversationUi>().invoke_confirm();
    assert!(matches!(&commands.borrow()[4], Command::Confirm(origin)
        if origin.character == CharacterId::Crazy && origin.selection_revision == 2));
    window.hide().unwrap();
}

#[test]
fn chat_copy_selection_dialog_scopes_text_to_active_character() {
    use super::super::{runtime::Command, sandbox::bind_chat_actions, view::ConversationUi};
    use neuromita::character::CharacterId;

    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    let commands = Rc::new(RefCell::new(Vec::<Command>::new()));
    let collected = Rc::clone(&commands);
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    bind_chat_actions(
        &window,
        Rc::new(move |command| collected.borrow_mut().push(command)),
        revision.clone(),
    );
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    window.invoke_chat_copy_message("visible text A".into());
    assert!(window.global::<ConversationUi>().get_copy_open());
    assert_eq!(
        window.global::<ConversationUi>().get_copy_text(),
        "visible text A"
    );

    window.invoke_chat_select_character(CharacterId::Kind.index() as i32);
    assert!(!window.global::<ConversationUi>().get_copy_open());
    assert_eq!(window.global::<ConversationUi>().get_copy_text(), "");
    window.invoke_chat_select_character(CharacterId::Crazy.index() as i32);
    assert_eq!(revision.load(std::sync::atomic::Ordering::Acquire), 2);
    assert!(!window.global::<ConversationUi>().get_copy_open());

    window.invoke_chat_copy_message("new selection text".into());
    assert!(window.global::<ConversationUi>().get_copy_open());
    assert_eq!(
        window.global::<ConversationUi>().get_copy_text(),
        "new selection text"
    );
    window.invoke_chat_copy_message("".into());
    assert_eq!(
        window.global::<ConversationUi>().get_copy_text(),
        "new selection text"
    );
    assert_eq!(commands.borrow().len(), 2, "copy must not mutate Timeline");
    window.hide().unwrap();
}

#[test]
fn chat_composer_cancel_and_microphone_actions_follow_selection_and_speech_state() {
    use super::super::{
        runtime::{ChatOrigin, Command, Update},
        sandbox,
        view::AppWindow,
    };
    use neuromita::{
        audio::controller::{SpeechSessionPhase, SpeechSessionSnapshot},
        character::CharacterId,
    };

    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    window.set_core_connected(true);
    window.set_chat_generation_ready(true);
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let commands = Rc::new(RefCell::new(Vec::new()));
    let collected = commands.clone();
    sandbox::bind_chat_actions(
        &window,
        Rc::new(move |command| collected.borrow_mut().push(command)),
        revision.clone(),
    );

    let request_id = uuid::Uuid::new_v4();
    window.set_chat_active_request_id(request_id.to_string().into());
    window.invoke_chat_cancel(request_id.to_string().into());
    window.invoke_chat_microphone_toggle(true);
    let owner = ChatOrigin {
        character: CharacterId::Crazy,
        selection_revision: 0,
    };
    assert!(
        matches!(&commands.borrow()[0], Command::Cancel(origin, id) if *origin == owner && id.0 == request_id)
    );
    assert!(
        matches!(&commands.borrow()[1], Command::ChatSpeechToggle(origin, true) if *origin == owner)
    );

    let editor = std::sync::Arc::new(std::sync::Mutex::new(
        super::super::memory::MemoryEditor::default(),
    ));
    sandbox::apply(
        &window,
        Update::SpeechSessionStatus(SpeechSessionSnapshot {
            phase: SpeechSessionPhase::Starting,
            last_error: None,
        }),
        &editor,
        &revision,
    );
    assert_eq!(window.get_chat_microphone_phase(), 1);
    sandbox::apply(
        &window,
        Update::SpeechSessionStatus(SpeechSessionSnapshot {
            phase: SpeechSessionPhase::Listening,
            last_error: None,
        }),
        &editor,
        &revision,
    );
    assert_eq!(window.get_chat_microphone_phase(), 2);
    window.invoke_chat_microphone_toggle(false);
    assert!(
        matches!(&commands.borrow()[2], Command::ChatSpeechToggle(origin, false) if *origin == owner)
    );

    window.invoke_chat_select_character(CharacterId::Kind.index() as i32);
    assert_eq!(window.get_chat_active_request_id(), "");
    assert!(
        matches!(&commands.borrow()[3], Command::Select(origin) if origin.character == CharacterId::Kind && origin.selection_revision == 1)
    );
    window.invoke_chat_microphone_toggle(true);
    assert!(
        matches!(&commands.borrow()[4], Command::ChatSpeechToggle(origin, true) if origin.character == CharacterId::Kind && origin.selection_revision == 1)
    );
    window.hide().unwrap();
}

#[test]
fn chat_retry_callback_preserves_original_request_identity_and_selection() {
    use super::super::{
        runtime::{ChatOrigin, Command},
        sandbox,
        view::AppWindow,
    };
    use neuromita::character::CharacterId;

    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.show().unwrap();
    window.set_chat_character(CharacterId::Crazy.index() as i32);
    let revision = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let recorded = Rc::new(RefCell::new(Vec::new()));
    let commands = recorded.clone();
    sandbox::bind_chat_actions(
        &window,
        Rc::new(move |cmd| commands.borrow_mut().push(cmd)),
        revision.clone(),
    );
    let first = uuid::Uuid::new_v4();
    window.invoke_chat_retry(first.to_string().into());
    assert!(matches!(&recorded.borrow()[0], Command::Retry(origin, id)
        if *origin == ChatOrigin{ character: CharacterId::Crazy, selection_revision: 0 } && id.0 == first));
    window.invoke_chat_select_character(CharacterId::Kind.index() as i32);
    window.invoke_chat_select_character(CharacterId::Crazy.index() as i32);
    let new_attempt = uuid::Uuid::new_v4();
    window.invoke_chat_retry(new_attempt.to_string().into());
    assert!(matches!(&recorded.borrow()[3], Command::Retry(origin, id)
        if *origin == ChatOrigin { character: CharacterId::Crazy, selection_revision: 2 } && id.0 == new_attempt));
    window.hide().unwrap();
}

#[test]
fn ai_hub_preserves_model_identity_when_inventory_updates_arrive_in_either_order() {
    use crate::{ai_hub, runtime::Command, view::HubModelIdentity};
    use neuromita::ai_runtime::{
        native_model::NativeBackend,
        native_revisions::{NativeRevisionMetadata, NativeRevisionOverview},
        native_speech::NativeSpeechKind,
        native_trust::NativeAuthorizedRelease,
        speech_catalog::{SpeechInventory, SpeechModelOption},
    };

    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let speech = SpeechInventory {
        tts: vec![SpeechModelOption {
            id: "shared".into(),
            name: "Shared Voice".into(),
            package_detected: true,
            verified_assets: false,
            installed_revisions: vec![],
        }],
        ..Default::default()
    };
    let releases = [0x11, 0x22].map(|byte| NativeAuthorizedRelease {
        model_id: "shared".into(),
        manifest_sha256: [byte; 32],
        publisher_id: "test-publisher".into(),
    });
    let overview = NativeRevisionOverview {
        revisions: releases
            .iter()
            .map(|release| NativeRevisionMetadata {
                model_id: release.model_id.clone(),
                manifest_sha256: release.manifest_sha256,
                display_name: "Shared Voice".into(),
                package_version: "1.0.0".into(),
                backends: vec![NativeBackend::Cpu],
                speech_services: vec![NativeSpeechKind::Tts],
            })
            .collect(),
        selections: vec![],
    };
    for native_first in [true, false] {
        let window = AppWindow::new().unwrap();
        let commands = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&commands);
        ai_hub::bind(
            &window,
            Rc::new(move |command| captured.borrow_mut().push(command)),
        );
        let mut state = ai_hub::HubCatalogState::default();
        if native_first {
            ai_hub::native_release_catalog(
                &window,
                &mut state,
                releases.to_vec(),
                overview.clone(),
            );
            ai_hub::inventory(&window, &mut state, &speech);
        } else {
            ai_hub::inventory(&window, &mut state, &speech);
            ai_hub::native_release_catalog(
                &window,
                &mut state,
                releases.to_vec(),
                overview.clone(),
            );
        }
        let entries = window.get_ai_hub_entries();
        assert_eq!(entries.row_count(), 3);
        assert_eq!(window.get_ai_hub_category_counts().row_data(0), Some(3));
        for (index, entry) in entries.iter().enumerate() {
            let identity = HubModelIdentity {
                category: entry.category,
                id: entry.id,
                implementation: entry.implementation,
                revision: entry.revision,
            };
            assert_eq!(
                window.invoke_ai_hub_find_selection_index(identity.clone()),
                index as i32
            );
            let mut stale = identity;
            stale.revision = "f".repeat(64).into();
            assert_eq!(window.invoke_ai_hub_find_selection_index(stale), -1);
        }
        let revision = "11".repeat(32);
        window.invoke_native_activate("shared".into(), "tts".into(), revision.clone().into());
        window.invoke_native_stop("tts".into(), "shared".into(), revision.clone().into());
        let observed_commands = commands.borrow();
        assert!(
            matches!(&observed_commands[0], Command::ActivateNativeModel { model_id, service, revision: actual }
            if model_id == "shared" && service == "tts" && actual == &revision)
        );
        assert!(
            matches!(&observed_commands[1], Command::StopNativeModel { model_id, service, revision: actual }
            if model_id == "shared" && service == "tts" && actual == &revision)
        );
        drop(observed_commands);
        window.invoke_ai_hub_use_model(HubModelIdentity {
            category: "tts".into(),
            id: "shared".into(),
            implementation: crate::view::HubImplementation::Rust,
            revision: revision.clone().into(),
        });
        assert!(
            matches!(commands.borrow_mut().pop(), Some(Command::SelectSpeechModel(neuromita::ai_runtime::AiService::Tts, model)) if model == neuromita::settings::SpeechModelReference::rust("shared", revision))
        );
    }
}

#[test]
fn successful_gui_shutdown_never_cancels_a_scheduled_native_update() {
    let cancelled = std::cell::Cell::new(false);
    let result = request_scheduled_shutdown(
        || Ok::<(), &str>(()),
        || {
            cancelled.set(true);
            Err::<(), &str>("must not cancel")
        },
    );
    assert_eq!(result, Ok(()));
    assert!(!cancelled.get());
}

#[test]
fn failed_gui_shutdown_cancels_the_scheduled_native_update() {
    let calls = RefCell::new(Vec::new());
    let result = request_scheduled_shutdown(
        || {
            calls.borrow_mut().push("quit");
            Err::<(), &str>("event loop refused shutdown")
        },
        || {
            calls.borrow_mut().push("cancel");
            Ok::<(), &str>(())
        },
    );
    assert_eq!(*calls.borrow(), ["quit", "cancel"]);
    assert_eq!(
        result,
        Err(ScheduledShutdownError::Cancelled {
            shutdown: "event loop refused shutdown".into(),
        })
    );
}

#[test]
fn failed_gui_shutdown_and_cancellation_report_both_errors_as_pending() {
    let result = request_scheduled_shutdown(
        || Err::<(), &str>("Slint event-loop failure"),
        || Err::<(), &str>("workspace inventory changed"),
    );
    assert_eq!(
        result,
        Err(ScheduledShutdownError::PendingTransaction {
            shutdown: "Slint event-loop failure".into(),
            cancellation: "workspace inventory changed".into(),
        })
    );
}

#[test]
fn guide_navigation_ack_is_correlated_and_does_not_advance_before_persistence() {
    use super::super::{memory::MemoryEditor, runtime::Update, sandbox::apply_with_pager};
    use std::sync::{
        Arc, Mutex,
        atomic::AtomicU64,
    };

    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let window = AppWindow::new().unwrap();
    window.set_onboarding_guide_level(0);
    window.set_onboarding_guide_page(0);
    window.set_onboarding_guide_request_id(17);
    window.set_onboarding_guide_saving(true);

    let editor = Arc::new(Mutex::new(MemoryEditor::default()));
    let revision = Arc::new(AtomicU64::new(0));
    apply_with_pager(
        &window,
        Update::GuidePagePersisted {
            request_id: 16,
            page: 2,
        },
        &editor,
        &revision,
        None,
    );
    assert_eq!(window.get_onboarding_guide_page(), 0);
    assert!(window.get_onboarding_guide_saving());

    apply_with_pager(
        &window,
        Update::GuidePageFailed {
            request_id: 17,
            error: "simulated flush failure".into(),
        },
        &editor,
        &revision,
        None,
    );
    assert_eq!(window.get_onboarding_guide_page(), 0);
    assert!(!window.get_onboarding_guide_saving());
    assert!(window.get_onboarding_guide_save_failed());

    window.set_onboarding_guide_request_id(18);
    window.set_onboarding_guide_saving(true);
    apply_with_pager(
        &window,
        Update::GuidePagePersisted {
            request_id: 18,
            page: 1,
        },
        &editor,
        &revision,
        None,
    );
    assert_eq!(window.get_onboarding_guide_page(), 1);
    assert!(!window.get_onboarding_guide_saving());
    assert!(!window.get_onboarding_guide_save_failed());
}
