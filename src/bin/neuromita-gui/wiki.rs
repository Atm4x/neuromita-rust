use neuromita::onboarding::wiki;
use slint::{ModelRc, StyledText, VecModel};
use std::collections::HashMap;

use super::view::{AppWindow, WikiArticleEntry, WikiSection};

struct WikiVisit {
    id: &'static str,
    anchor: Option<String>,
}

pub struct WikiHistory {
    visited: Vec<WikiVisit>,
    cursor: usize,
    language: String,
}

impl WikiHistory {
    pub fn new(language: &str) -> Self {
        Self {
            visited: vec![WikiVisit {
                id: "index.md",
                anchor: None,
            }],
            cursor: 0,
            language: language.to_owned(),
        }
    }

    pub fn current(&self) -> &'static str {
        self.visited[self.cursor].id
    }

    pub fn current_anchor(&self) -> Option<&str> {
        self.visited[self.cursor].anchor.as_deref()
    }

    pub fn open(&mut self, id: &str) -> bool {
        self.open_with_anchor(id, None)
    }

    pub fn open_with_anchor(&mut self, id: &str, anchor: Option<&str>) -> bool {
        let Some(article) = wiki::article(id) else {
            return false;
        };
        if article.id != self.current() || anchor != self.current_anchor() {
            self.visited.truncate(self.cursor + 1);
            self.visited.push(WikiVisit {
                id: article.id,
                anchor: anchor.map(str::to_owned),
            });
            self.cursor += 1;
        }
        true
    }

    pub fn back(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn forward(&mut self) {
        if self.cursor + 1 < self.visited.len() {
            self.cursor += 1;
        }
    }

    pub fn set_language(&mut self, locale: &str) -> bool {
        match locale {
            "ru" | "en" => {
                self.language = locale.to_owned();
                true
            }
            _ => false,
        }
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn can_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.visited.len()
    }
}

fn slint_wiki_markdown(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            let heading = line.trim_start_matches('#');
            if heading.len() < line.len() && heading.starts_with(' ') {
                format!("**{}**", heading.trim())
            } else if let Some(quote) = line.strip_prefix("> ") {
                format!("│ {quote}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn heading(line: &str) -> Option<(i32, &str)> {
    let count = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&count) || line.as_bytes().get(count) != Some(&b' ') {
        return None;
    }
    Some((
        count as i32,
        line[count + 1..].trim().trim_end_matches('#').trim(),
    ))
}

fn heading_slug(title: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            slug.push(ch);
        } else if (ch.is_whitespace() || ch == '-') && !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

fn wiki_sections(markdown: &str) -> Result<Vec<WikiSection>, slint::StyledTextFromMarkdownError> {
    let mut sections = Vec::new();
    let mut duplicates: HashMap<String, usize> = HashMap::new();
    let mut title = String::new();
    let mut id = String::new();
    let mut level = 1;
    let mut body = String::new();
    let mut in_code = false;

    let flush = |sections: &mut Vec<WikiSection>, title: &str, id: &str, level, body: &str| {
        let text = StyledText::from_markdown(&slint_wiki_markdown(body))?;
        sections.push(WikiSection {
            id: id.into(),
            title: title.into(),
            level,
            text,
        });
        Ok::<(), slint::StyledTextFromMarkdownError>(())
    };

    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        if !in_code {
            if let Some((next_level, next_title)) = heading(line) {
                if !title.is_empty() || !body.is_empty() {
                    flush(&mut sections, &title, &id, level, &body)?;
                }
                title = next_title.to_owned();
                let base = heading_slug(next_title);
                let count = duplicates.entry(base.clone()).or_default();
                id = if *count == 0 {
                    base
                } else {
                    format!("{base}-{count}")
                };
                *count += 1;
                level = next_level;
                body.clear();
                continue;
            }
        }
        body.push_str(line);
        body.push('\n');
    }
    if !title.is_empty() || !body.is_empty() {
        flush(&mut sections, &title, &id, level, &body)?;
    }
    Ok(sections)
}

pub fn render(window: &AppWindow, history: &WikiHistory) {
    render_to_anchor(window, history, history.current_anchor());
}

pub fn render_to_anchor(window: &AppWindow, history: &WikiHistory, anchor: Option<&str>) -> bool {
    let page =
        wiki::article(history.current()).expect("Wiki history contains only registered pages");
    let sections =
        wiki_sections(page.markdown(history.language())).expect("bundled Wiki Markdown is valid");
    let target = anchor.and_then(|name| sections.iter().position(|section| section.id == name));
    window.set_wiki_current_article(page.id.into());
    window.set_wiki_current_title(page.title(history.language()).into());
    window.set_wiki_language(history.language().into());
    window.set_wiki_can_back(history.can_back());
    window.set_wiki_can_forward(history.can_forward());
    window.set_wiki_article_text(
        StyledText::from_markdown(&slint_wiki_markdown(page.markdown(history.language())))
            .expect("bundled Wiki Markdown is valid"),
    );
    window.set_wiki_sections(ModelRc::new(VecModel::from(sections)));
    window.set_wiki_articles(ModelRc::new(VecModel::from(
        wiki::ARTICLES
            .iter()
            .map(|entry| WikiArticleEntry {
                id: entry.id.into(),
                title: entry.title(history.language()).into(),
            })
            .collect::<Vec<_>>(),
    )));
    window.set_wiki_scroll_target(target.unwrap_or(0) as i32);
    window.set_wiki_scroll_revision(window.get_wiki_scroll_revision().wrapping_add(1));
    anchor.is_none() || target.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_sections_preserve_bilingual_unicode_anchors_and_repeated_headings() {
        let sections = wiki_sections("# Быстрый старт\nТекст\n## Настройка API\n[Link](#быстрый-старт)\n## Настройка API\nDuplicate\n").unwrap();
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].id.as_str(), "быстрый-старт");
        assert_eq!(sections[1].id.as_str(), "настройка-api");
        assert_eq!(sections[2].id.as_str(), "настройка-api-1");
        assert_eq!(sections[0].level, 1);
        assert_eq!(sections[1].level, 2);
    }

    #[test]
    fn every_bundled_anchor_link_targets_a_real_heading() {
        for page in wiki::ARTICLES {
            for language in ["ru", "en"] {
                let source = page.markdown(language);
                let sections = wiki_sections(source).unwrap();
                for part in source.split("](").skip(1) {
                    let Some((link, _)) = part.split_once(')') else {
                        continue;
                    };
                    if let Ok(wiki::WikiLink::Article {
                        id,
                        anchor: Some(anchor),
                    }) = wiki::resolve_link(link)
                    {
                        let target = wiki::article(id).unwrap();
                        assert!(
                            wiki_sections(target.markdown(language))
                                .unwrap()
                                .iter()
                                .any(|section| section.id == anchor),
                            "missing anchor {link} in {} {language}",
                            page.id
                        );
                    }
                }
                assert!(!sections.is_empty());
            }
        }
    }

    #[test]
    fn all_guide_and_wiki_markdown_renders() {
        for page in neuromita::onboarding::guide::PAGES {
            for language in ["ru", "en"] {
                StyledText::from_markdown(&page.description_markdown(language))
                    .unwrap_or_else(|error| panic!("Guide {} {language}: {error}", page.id));
            }
        }
        for page in wiki::ARTICLES {
            for language in ["ru", "en"] {
                StyledText::from_markdown(&slint_wiki_markdown(page.markdown(language)))
                    .unwrap_or_else(|error| panic!("Wiki {} {language}: {error}", page.id));
            }
        }
    }

    #[test]
    fn wiki_projection_preserves_links_and_rewrites_unsupported_blocks() {
        let source = "# Title\n\n> **New?** [Start](./getting-started.md)\n\n## Section\n\n- [Settings](app:settings)";
        let projected = slint_wiki_markdown(source);
        assert!(projected.contains("**Title**"));
        assert!(projected.contains("│ **New?** [Start](./getting-started.md)"));
        assert!(projected.contains("**Section**"));
        assert!(projected.contains("- [Settings](app:settings)"));
        StyledText::from_markdown(&projected).unwrap();
    }

    #[test]
    fn history_truncates_forward_branch_and_language_does_not_navigate() {
        let mut state = WikiHistory::new("ru");
        assert_eq!(state.current(), "index.md");
        assert!(!state.can_back());
        assert!(state.open("getting-started.md"));
        assert!(state.open("chat-and-characters.md"));
        state.back();
        assert_eq!(state.current(), "getting-started.md");
        assert!(state.can_forward());
        assert!(state.set_language("en"));
        assert_eq!(state.language(), "en");
        assert!(state.can_forward());
        assert!(state.open_with_anchor(
            "voice-microphone-camera-and-screen.md",
            Some("voice-output")
        ));
        assert_eq!(state.current_anchor(), Some("voice-output"));
        assert!(!state.can_forward());
        state.back();
        assert_eq!(state.current(), "getting-started.md");
        assert_eq!(state.current_anchor(), None);
        state.forward();
        assert_eq!(state.current_anchor(), Some("voice-output"));
        assert!(!state.open("../../outside.md"));
        assert_eq!(state.current(), "voice-microphone-camera-and-screen.md");
    }
}
