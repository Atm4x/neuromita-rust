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
    window
        .window()
        .set_size(slint::PhysicalSize::new(1180, 680));

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
