use std::{
    cell::RefCell, collections::HashSet, error::Error, path::PathBuf, rc::Rc, time::Duration,
};

use neuromita::settings::{JsonSettingsRepository, Language, SettingsRepository};
use slint::{ComponentHandle, ModelRc, StyledText, VecModel};

struct ConsentScratch(PathBuf);

impl Drop for ConsentScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

fn assert_bundled_guide_image(image: &[u8], scale: f32) -> Result<(), String> {
    let width = (1180.0 * scale).round() as usize;
    let height = (680.0 * scale).round() as usize;
    if image.len() != width * height * 4 {
        return Err("Guide snapshot buffer dimensions do not match the window".into());
    }
    let step = (2.0 * scale).round().max(1.0) as usize;
    let left = (458.0 * scale).round() as usize;
    let right = (722.0 * scale).round() as usize;
    let top = (195.0 * scale).round() as usize;
    let bottom = (357.0 * scale).round() as usize;
    let mut distinct = HashSet::new();
    for y in (top..bottom).step_by(step) {
        for x in (left..right).step_by(step) {
            let offset = (y * width + x) * 4;
            distinct.insert(&image[offset..offset + 3]);
        }
    }
    eprintln!(
        "Bundled Guide image distinct sampled colors: {}",
        distinct.len()
    );
    if distinct.len() < 80 {
        return Err(format!(
            "Bundled Guide screenshot appears missing: only {} sampled colors",
            distinct.len()
        ));
    }
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
    let consent_scratch = Rc::new(ConsentScratch(
        std::env::temp_dir().join(format!("q2-onboarding-eula-{}", uuid::Uuid::new_v4())),
    ));
    std::fs::create_dir_all(&consent_scratch.0)?;
    if !neuromita::onboarding::startup_state(&consent_scratch.0)?.requires_consent {
        return Err("Fresh GUI acceptance fixture must start without consent".into());
    }
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

    let consent_rejected = Rc::new(RefCell::new(0_usize));
    let rejected = consent_rejected.clone();
    window.on_onboarding_reject(move || {
        *rejected.borrow_mut() += 1;
    });
    let consent_accepted = Rc::new(RefCell::new(0_usize));
    let accepted = consent_accepted.clone();
    let accept_failure = Rc::new(RefCell::new(None::<String>));
    let acceptance_failure = accept_failure.clone();
    let acceptance_directory = consent_scratch.clone();
    let weak_accept = window.as_weak();
    window.on_onboarding_accept(move || {
        *accepted.borrow_mut() += 1;
        if let Some(window) = weak_accept.upgrade() {
            match neuromita::onboarding::accept(
                &acceptance_directory.0,
                &window.get_onboarding_language(),
            ) {
                Ok(()) => window.set_onboarding_required(false),
                Err(error) => *acceptance_failure.borrow_mut() = Some(error.to_string()),
            }
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
    let consent_for_assertions = consent_scratch.clone();
    let acceptance_failure_for_assertions = accept_failure.clone();
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
                    10 => click(&window, 764.0, 607.0),
                    11 => {
                        let _ = shot("eula-rejected")?;
                        if *consent_rejected.borrow() != 1
                            || *consent_accepted.borrow() != 0
                            || !window.get_onboarding_required()
                            || consent_for_assertions.0.join("settings.json").exists()
                            || !neuromita::onboarding::startup_state(&consent_for_assertions.0)?
                                .requires_consent
                        {
                            return Err("Reject must not dismiss or persist EULA consent".into());
                        }
                    }
                    12 => click(&window, 927.0, 607.0),
                    13 => {
                        let _ = shot("eula-accepted")?;
                        let restarted = neuromita::onboarding::startup_state(
                            &consent_for_assertions.0,
                        )?;
                        let stored = JsonSettingsRepository::new(
                            consent_for_assertions.0.join("settings.json"),
                        )
                        .load()?;
                        if *consent_accepted.borrow() != 1
                            || *consent_rejected.borrow() != 1
                            || window.get_onboarding_required()
                            || acceptance_failure_for_assertions.borrow().is_some()
                            || restarted.requires_consent
                            || !restarted.requires_guide
                            || restarted.guide_page != 0
                            || !stored.settings.onboarding.eula_accepted
                            || stored.settings.interface.language != Language::English
                        {
                            return Err(format!(
                                "Accept click failed to persist offline startup state: {restarted:?}, error={:?}",
                                acceptance_failure_for_assertions.borrow()
                            )
                            .into());
                        }
                        eprintln!("Offline EULA click persisted; restart skips consent and opens Guide");
                    }
                    14 => {
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
                    15 => *reference.borrow_mut() = shot("wiki-before-anchor")?,
                    16 => {
                        window.set_wiki_scroll_target(20);
                        window.set_wiki_scroll_revision(1);
                    }
                    17 => {
                        let image = shot("wiki-after-anchor")?;
                        assert_changed(&reference.borrow(), &image, "Wiki anchor scroll")?;
                    }
                    18 => {
                        window.set_wiki_current_article("index.md".into());
                        window.set_wiki_current_title("Главная".into());
                        window.set_wiki_language("ru".into());
                        window.set_wiki_home_left(home_cards("ru", true));
                        window.set_wiki_home_right(home_cards("ru", false));
                    }
                    19 => *reference.borrow_mut() = shot("wiki-home-ru")?,
                    20 => {
                        window.set_wiki_language("en".into());
                        window.set_wiki_current_title("Home".into());
                        window.set_wiki_home_left(home_cards("en", true));
                        window.set_wiki_home_right(home_cards("en", false));
                    }
                    21 => {
                        let image = shot("wiki-home-en")?;
                        assert_changed(&reference.borrow(), &image, "Wiki home locale")?;
                        *reference.borrow_mut() = image;
                    }
                    22 => {
                        window.window().dispatch_event(
                            slint::platform::WindowEvent::PointerScrolled {
                                position: slint::LogicalPosition::new(825.0, 490.0),
                                delta_x: 0.0,
                                delta_y: -380.0,
                            },
                        );
                    }
                    23 => {
                        let image = shot("wiki-home-en-wheel")?;
                        assert_changed(&reference.borrow(), &image, "Wiki home wheel")?;
                    }
                    24 => {
                        window.window().dispatch_event(
                            slint::platform::WindowEvent::PointerScrolled {
                                position: slint::LogicalPosition::new(825.0, 490.0),
                                delta_x: 0.0,
                                delta_y: 450.0,
                            },
                        );
                    }
                    25 => {
                        click(&window, 665.0, 290.0);
                    }
                    26 => {
                        let observed = clicked_link.borrow().clone();
                        if observed.as_deref() != Some("getting-started.md") {
                            return Err(format!("Wiki home card click: expected getting-started.md, got {observed:?}").into());
                        }
                        window.set_page(view::PageId::Home);
                        let guide = neuromita::onboarding::guide::page(
                            neuromita::onboarding::guide::GuideLevel::Basic,
                            1,
                        )
                        .expect("second basic guide page exists");
                        window.set_onboarding_guide_wiki_target(
                            guide.wiki_target.expect("Guide Wiki link").into(),
                        );
                        window.set_onboarding_guide_title(guide.title("en").into());
                        window.set_onboarding_guide_image(guide.image_key.into());
                        window.set_onboarding_guide_targets(ModelRc::new(VecModel::from(
                            guide
                                .focus_steps()
                                .iter()
                                .map(|step| view::GuideFocusTarget {
                                    number: i32::from(step.number),
                                    left: step.rect.left,
                                    top: step.rect.top,
                                    width: step.rect.width,
                                    height: step.rect.height,
                                })
                                .collect::<Vec<_>>(),
                        )));
                        window.set_onboarding_guide_description_rich(
                            StyledText::from_markdown(&guide.description_markdown("en"))?,
                        );
                        window.set_onboarding_guide_count(
                            neuromita::onboarding::guide::count(
                                neuromita::onboarding::guide::GuideLevel::Basic,
                            ) as i32,
                        );
                        window.set_onboarding_guide_page(1);
                        window.set_onboarding_guide_visible(true);
                    }
                    27 => {
                        let image = shot("guide-before-wiki")?;
                        assert_bundled_guide_image(&image, ui_scale)?;
                    }
                    28 => click(&window, 220.0, 611.0),
                    29 => {
                        let _ = shot("guide-to-wiki")?;
                        if window.get_page() != view::PageId::Wiki
                            || window.get_onboarding_guide_visible()
                            || window.get_wiki_current_article().as_str() != "getting-started.md"
                        {
                            return Err("Guide to Wiki click did not navigate".into());
                        }
                    }
                    30 => click(&window, 558.0, 58.0),
                    31 => {
                        *reference.borrow_mut() = shot("wiki-back-to-guide")?;
                        if window.get_page() != view::PageId::Home
                            || !window.get_onboarding_guide_visible()
                            || window.get_onboarding_guide_page() != 1
                            || *reopened_guide.borrow() != 0
                        {
                            return Err(format!("Wiki Back did not restore the existing Guide page; callbacks={}", reopened_guide.borrow()).into());
                        }
                    }
                    32 => {
                        window.set_onboarding_guide_focus_step(2);
                        window.set_onboarding_guide_zoom(1.7);
                        window.set_onboarding_guide_focus_x(0.75);
                        window.set_onboarding_guide_focus_y(0.55);
                    }
                    33 => {
                        let image = shot("guide-image-focus-zoom")?;
                        assert_changed(
                            &reference.borrow(),
                            &image,
                            "Guide embedded asset zoom and numbered focus overlay",
                        )?;
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
