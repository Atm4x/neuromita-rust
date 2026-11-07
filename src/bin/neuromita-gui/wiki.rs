use neuromita::onboarding::wiki;
use slint::{ModelRc, StyledText, VecModel};

use super::view::{AppWindow, WikiArticleEntry};

pub struct WikiHistory {
    visited: Vec<&'static str>,
    cursor: usize,
    language: String,
}

impl WikiHistory {
    pub fn new(language: &str) -> Self {
        Self {
            visited: vec!["index.md"],
            cursor: 0,
            language: language.to_owned(),
        }
    }

    pub fn current(&self) -> &'static str {
        self.visited[self.cursor]
    }

    pub fn open(&mut self, id: &str) -> bool {
        let Some(article) = wiki::article(id) else {
            return false;
        };
        if article.id != self.current() {
            self.visited.truncate(self.cursor + 1);
            self.visited.push(article.id);
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

pub fn render(window: &AppWindow, history: &WikiHistory) {
    let page =
        wiki::article(history.current()).expect("Wiki history contains only registered pages");
    window.set_wiki_current_article(page.id.into());
    window.set_wiki_current_title(page.title(history.language()).into());
    window.set_wiki_language(history.language().into());
    window.set_wiki_can_back(history.can_back());
    window.set_wiki_can_forward(history.can_forward());
    window.set_wiki_article_text(
        StyledText::from_markdown(page.markdown(history.language()))
            .expect("bundled Wiki Markdown is valid"),
    );
    window.set_wiki_articles(ModelRc::new(VecModel::from(
        wiki::ARTICLES
            .iter()
            .map(|entry| WikiArticleEntry {
                id: entry.id.into(),
                title: entry.title(history.language()).into(),
            })
            .collect::<Vec<_>>(),
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

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
                StyledText::from_markdown(page.markdown(language))
                    .unwrap_or_else(|error| panic!("Wiki {} {language}: {error}", page.id));
            }
        }
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
        assert!(state.open("voice-microphone-camera-and-screen.md"));
        assert!(!state.can_forward());
        assert!(!state.open("../../outside.md"));
        assert_eq!(state.current(), "voice-microphone-camera-and-screen.md");
    }
}
