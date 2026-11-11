use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc, time::Duration};

use slint::{ComponentHandle, ModelRc, StyledText, VecModel};

mod view {
    slint::include_modules!();
}

fn screenshot(
    window: &view::AppWindow,
    target: &std::path::Path,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let image = window.window().take_snapshot()?;
    let bytes = image.as_bytes();
    let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
    for pixel in bytes.as_chunks::<4>().0 {
        ppm.extend_from_slice(&pixel[..3]);
    }
    std::fs::write(target, ppm)?;
    Ok(bytes.to_vec())
}

fn assert_changed(before: &[u8], after: &[u8], scenario: &str) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!(
            "{scenario}: rendered buffer dimensions changed unexpectedly"
        ));
    }
    let changed = before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    if changed < 500 {
        return Err(format!("{scenario}: only {changed} pixels changed"));
    }
    eprintln!("{scenario}: {changed} changed pixels");
    Ok(())
}

fn home_cards(locale: &str, left: bool) -> ModelRc<view::WikiHomeCardEntry> {
    ModelRc::new(VecModel::from(
        neuromita::onboarding::wiki::HOME_CARDS
            .iter()
            .enumerate()
            .filter(|(index, _)| (*index % 2 == 0) == left)
            .map(|(_, card)| view::WikiHomeCardEntry {
                title: card.title(locale).into(),
                description: card.body(locale).into(),
                destination: card.destination.into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn click(window: &view::AppWindow, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
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
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: q2_onboarding_acceptance <output-directory>")?,
    );
    std::fs::create_dir_all(&output)?;
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .select()?;
    let window = view::AppWindow::new()?;
    window.set_onboarding_eula_text(neuromita::onboarding::eula_text("ru").into());
    window.set_onboarding_required(true);
    window.show()?;
    let ui_scale = std::env::var("Q2_UI_SCALE_FACTOR")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(1.0);
    if !(1.0..=2.0).contains(&ui_scale) {
        return Err("Q2_UI_SCALE_FACTOR must be between 1.0 and 2.0".into());
    }
    window.window().set_size(slint::PhysicalSize::new(
        (1180.0 * ui_scale).round() as u32,
        (680.0 * ui_scale).round() as u32,
    ));
    eprintln!("Q2 GUI physical scale factor: {ui_scale}");

    let clicked_link = Rc::new(RefCell::new(None::<String>));
    let clicked_for_callback = clicked_link.clone();
    window.on_wiki_open_link(move |link| {
        *clicked_for_callback.borrow_mut() = Some(link.to_string());
    });
    let weak_navigate = window.as_weak();
    window.on_navigate(move |page| {
        if let Some(window) = weak_navigate.upgrade() {
            window.set_page(page);
        }
    });
    let weak_article = window.as_weak();
    window.on_wiki_open_article(move |id| {
        if let Some(window) = weak_article.upgrade() {
            window.set_wiki_current_article(id);
        }
    });
    let reopened_guide = Rc::new(RefCell::new(0_usize));
    let reopened_counter = reopened_guide.clone();
    let weak_reopen = window.as_weak();
    window.on_onboarding_guide_reopen(move || {
        *reopened_counter.borrow_mut() += 1;
        if let Some(window) = weak_reopen.upgrade() {
            window.set_onboarding_guide_visible(true);
        }
    });

    let output = Rc::new(output);
    let reference = Rc::new(RefCell::new(Vec::new()));
    let outcome: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
    let step = Rc::new(RefCell::new(0u8));
    let timer = Rc::new(slint::Timer::default());
    let weak = window.as_weak();
    let completed = outcome.clone();
    let t = timer.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(350),
        move || {
            let result = (|| -> Result<(), Box<dyn Error>> {
                let window = weak.upgrade().ok_or("window closed")?;
                let shot = |name: &str| screenshot(&window, &output.join(format!("{name}.ppm")));
                match *step.borrow() {
                    0 => *reference.borrow_mut() = shot("eula-ru-before")?,
                    1 => {
                        window
                            .window()
                            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                                text: slint::platform::Key::PageDown.into(),
                            });
                    }
                    2 => {
                        let image = shot("eula-ru-pagedown")?;
                        assert_changed(&reference.borrow(), &image, "EULA RU PageDown")?;
                        *reference.borrow_mut() = image;
                    }
                    3 => {
                        window
                            .window()
                            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                                text: slint::platform::Key::PageDown.into(),
                            });
                        window
                            .window()
                            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                                text: slint::platform::Key::Home.into(),
                            });
                    }
                    4 => {
                        *reference.borrow_mut() = shot("eula-ru-home")?;
                        window.window().dispatch_event(
                            slint::platform::WindowEvent::PointerScrolled {
                                position: slint::LogicalPosition::new(480.0, 360.0),
                                delta_x: 0.0,
                                delta_y: -250.0,
                            },
                        );
                    }
                    5 => {
                        let image = shot("eula-ru-wheel")?;
                        assert_changed(&reference.borrow(), &image, "EULA RU mouse wheel")?;
                    }
                    6 => {
                        window.set_onboarding_required(false);
                        window.set_onboarding_eula_text(
                            neuromita::onboarding::eula_text("en").into(),
                        );
                        window.set_onboarding_language("en".into());
                        window.set_onboarding_required(true);
                    }
                    7 => *reference.borrow_mut() = shot("eula-en-before")?,
                    8 => {
                        window.window().dispatch_event(
                            slint::platform::WindowEvent::PointerScrolled {
                                position: slint::LogicalPosition::new(480.0, 360.0),
                                delta_x: 0.0,
                                delta_y: -250.0,
                            },
                        );
                    }
                    9 => {
                        let image = shot("eula-en-wheel")?;
                        assert_changed(&reference.borrow(), &image, "EULA EN mouse wheel")?;
                    }
                    10 => {
                        window.set_onboarding_required(false);
                        window.set_page(view::PageId::Wiki);
                        window.set_wiki_current_article("getting-started.md".into());
                        window.set_wiki_current_title("Index".into());
                        let sections = (0..30)
                            .map(|i| view::WikiSection {
                                id: format!("section-{i}").into(),
                                title: format!("Section {i}").into(),
                                level: if i % 5 == 0 { 1 } else { 2 },
                                text: StyledText::from_markdown(&format!(
                                    "Body text {i}, [link](./getting-started.md)."
                                ))
                                .expect("valid markdown"),
                            })
                            .collect::<Vec<_>>();
                        window.set_wiki_sections(ModelRc::new(VecModel::from(sections)));
                    }
                    11 => *reference.borrow_mut() = shot("wiki-before-anchor")?,
                    12 => {
                        window.set_wiki_scroll_target(20);
                        window.set_wiki_scroll_revision(1);
                    }
                    13 => {
                        let image = shot("wiki-after-anchor")?;
                        assert_changed(&reference.borrow(), &image, "Wiki anchor scroll")?;
                    }
                    14 => {
                        window.set_wiki_current_article("index.md".into());
                        window.set_wiki_current_title("Главная".into());
                        window.set_wiki_language("ru".into());
                        window.set_wiki_home_left(home_cards("ru", true));
                        window.set_wiki_home_right(home_cards("ru", false));
                    }
                    15 => *reference.borrow_mut() = shot("wiki-home-ru")?,
                    16 => {
                        window.set_wiki_language("en".into());
                        window.set_wiki_current_title("Home".into());
                        window.set_wiki_home_left(home_cards("en", true));
                        window.set_wiki_home_right(home_cards("en", false));
                    }
                    17 => {
                        let image = shot("wiki-home-en")?;
                        assert_changed(&reference.borrow(), &image, "Wiki home locale")?;
                        *reference.borrow_mut() = image;
                    }
                    18 => {
                        window.window().dispatch_event(
                            slint::platform::WindowEvent::PointerScrolled {
                                position: slint::LogicalPosition::new(825.0, 490.0),
                                delta_x: 0.0,
                                delta_y: -380.0,
                            },
                        );
                    }
                    19 => {
                        let image = shot("wiki-home-en-wheel")?;
                        assert_changed(&reference.borrow(), &image, "Wiki home wheel")?;
                    }
                    20 => {
                        window.window().dispatch_event(
                            slint::platform::WindowEvent::PointerScrolled {
                                position: slint::LogicalPosition::new(825.0, 490.0),
                                delta_x: 0.0,
                                delta_y: 450.0,
                            },
                        );
                    }
                    21 => {
                        click(&window, 665.0, 290.0);
                    }
                    22 => {
                        let observed = clicked_link.borrow().clone();
                        if observed.as_deref() != Some("getting-started.md") {
                            return Err(format!("Wiki home card click: expected getting-started.md, got {observed:?}").into());
                        }
                        window.set_page(view::PageId::Home);
                        window.set_onboarding_guide_wiki_target("getting-started.md".into());
                        window.set_onboarding_guide_title("Guide acceptance".into());
                        window.set_onboarding_guide_page(2);
                        window.set_onboarding_guide_visible(true);
                    }
                    23 => {
                        let _ = shot("guide-before-wiki")?;
                    }
                    24 => click(&window, 220.0, 611.0),
                    25 => {
                        let _ = shot("guide-to-wiki")?;
                        if window.get_page() != view::PageId::Wiki
                            || window.get_onboarding_guide_visible()
                            || window.get_wiki_current_article().as_str() != "getting-started.md"
                        {
                            return Err("Guide to Wiki click did not navigate".into());
                        }
                    }
                    26 => click(&window, 558.0, 58.0),
                    27 => {
                        let _ = shot("wiki-back-to-guide")?;
                        if window.get_page() != view::PageId::Home
                            || !window.get_onboarding_guide_visible()
                            || window.get_onboarding_guide_page() != 2
                            || *reopened_guide.borrow() != 0
                        {
                            return Err(format!("Wiki Back did not restore the existing Guide page; callbacks={}", reopened_guide.borrow()).into());
                        }
                    }
                    _ => {
                        *completed.borrow_mut() = Some(Ok(()));
                        t.stop();
                        let _ = slint::quit_event_loop();
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                *completed.borrow_mut() = Some(Err(error.to_string()));
                t.stop();
                let _ = slint::quit_event_loop();
            }
            *step.borrow_mut() += 1;
        },
    );
    slint::run_event_loop()?;
    outcome
        .borrow_mut()
        .take()
        .ok_or("fixture did not finish")?
        .map_err(Into::into)
}
