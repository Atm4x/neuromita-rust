//! Read-only, bundled articles from the historical Python Wiki revision 5f3d061f.
//! All navigation targets are resolved against this explicit inventory, not the filesystem.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WikiArticle {
    pub id: &'static str,
    pub title_ru: &'static str,
    pub title_en: &'static str,
    pub markdown_ru: &'static str,
    pub markdown_en: &'static str,
}

impl WikiArticle {
    pub fn title(self, locale: &str) -> &'static str {
        if locale.eq_ignore_ascii_case("ru") {
            self.title_ru
        } else {
            self.title_en
        }
    }

    pub fn markdown(self, locale: &str) -> &'static str {
        if locale.eq_ignore_ascii_case("ru") {
            self.markdown_ru
        } else {
            self.markdown_en
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WikiHomeCardInfo {
    pub title_ru: &'static str,
    pub title_en: &'static str,
    pub body_ru: &'static str,
    pub body_en: &'static str,
    pub destination: &'static str,
}

impl WikiHomeCardInfo {
    pub fn title(self, locale: &str) -> &'static str {
        if locale.eq_ignore_ascii_case("ru") { self.title_ru } else { self.title_en }
    }

    pub fn body(self, locale: &str) -> &'static str {
        if locale.eq_ignore_ascii_case("ru") { self.body_ru } else { self.body_en }
    }
}

pub const HOME_CARDS: &[WikiHomeCardInfo] = &[
    WikiHomeCardInfo {
        title_ru: "Первый запуск",
        title_en: "First setup",
        body_ru: "Установите NeuroMita и пройдите настройку приложения шаг за шагом.",
        body_en: "Install NeuroMita and complete the app setup step by step.",
        destination: "getting-started.md",
    },
    WikiHomeCardInfo {
        title_ru: "AI-провайдер",
        title_en: "AI provider",
        body_ru: "Подключите Gemini, OpenRouter или локальную модель для генерации ответов.",
        body_en: "Connect Gemini, OpenRouter or a local model for response generation.",
        destination: "app:settings/api",
    },
    WikiHomeCardInfo {
        title_ru: "Персонажи",
        title_en: "Characters",
        body_ru: "Выберите Миту, настройте её характер и промпты для диалога.",
        body_en: "Pick a Mita, configure her personality and dialogue prompts.",
        destination: "app:settings/characters",
    },
    WikiHomeCardInfo {
        title_ru: "Голос и микрофон",
        title_en: "Voice & Microphone",
        body_ru: "Включите озвучку и настройте распознавание речи для живого разговора.",
        body_en: "Enable voice output and set up speech recognition for live conversation.",
        destination: "app:settings/voice",
    },
    WikiHomeCardInfo {
        title_ru: "Память и RAG",
        title_en: "Memory & RAG",
        body_ru: "Управляйте памятью персонажа, эмбеддингами и графом знаний.",
        body_en: "Manage character memory, embeddings and the knowledge graph.",
        destination: "app:settings/models?section=RAG",
    },
    WikiHomeCardInfo {
        title_ru: "Диагностика",
        title_en: "Troubleshooting",
        body_ru: "Откройте логи, проверьте статус компонентов и найдите решение проблем.",
        body_en: "Open logs, check component status and find solutions to issues.",
        destination: "app:logs",
    },
];

pub const ARTICLES: &[WikiArticle] = &[
    WikiArticle {
        id: "index.md",
        title_ru: "Главная",
        title_en: "Home",
        markdown_ru: include_str!("../../docs/wiki/ru/index.md"),
        markdown_en: include_str!("../../docs/wiki/en/index.md"),
    },
    WikiArticle {
        id: "getting-started.md",
        title_ru: "Быстрый старт",
        title_en: "Getting started",
        markdown_ru: include_str!("../../docs/wiki/ru/getting-started.md"),
        markdown_en: include_str!("../../docs/wiki/en/getting-started.md"),
    },
    WikiArticle {
        id: "chat-and-characters.md",
        title_ru: "Чат и персонажи",
        title_en: "Chat & characters",
        markdown_ru: include_str!("../../docs/wiki/ru/chat-and-characters.md"),
        markdown_en: include_str!("../../docs/wiki/en/chat-and-characters.md"),
    },
    WikiArticle {
        id: "voice-microphone-camera-and-screen.md",
        title_ru: "Голос, микрофон и экран",
        title_en: "Voice, microphone & screen",
        markdown_ru: include_str!("../../docs/wiki/ru/voice-microphone-camera-and-screen.md"),
        markdown_en: include_str!("../../docs/wiki/en/voice-microphone-camera-and-screen.md"),
    },
    WikiArticle {
        id: "memory-data-ai-hub-and-debugging.md",
        title_ru: "Память и AI Hub",
        title_en: "Memory & AI Hub",
        markdown_ru: include_str!("../../docs/wiki/ru/memory-data-ai-hub-and-debugging.md"),
        markdown_en: include_str!("../../docs/wiki/en/memory-data-ai-hub-and-debugging.md"),
    },
    WikiArticle {
        id: "home-news-updates-and-launch.md",
        title_ru: "Главная и обновления",
        title_en: "Home & updates",
        markdown_ru: include_str!("../../docs/wiki/ru/home-news-updates-and-launch.md"),
        markdown_en: include_str!("../../docs/wiki/en/home-news-updates-and-launch.md"),
    },
];

pub fn article(id: &str) -> Option<&'static WikiArticle> {
    ARTICLES.iter().find(|entry| entry.id == id)
}

#[derive(Debug, PartialEq, Eq)]
pub enum WikiLink<'a> {
    Article {
        id: &'static str,
        anchor: Option<&'a str>,
    },
    SamePageAnchor(&'a str),
    AppHome,
    AppLogs,
    AppSandbox,
    AppSettings,
    AppSettingsSection(WikiSettingsSection),
    External(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WikiSettingsSection {
    Api,
    Characters,
    Voice,
    Language,
    Microphone,
    Models,
    ModelsRag,
    Screen,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WikiLinkError {
    UnsupportedPath,
    UnsupportedDeepLink,
    InvalidUrl,
}

pub fn resolve_link(link: &str) -> Result<WikiLink<'_>, WikiLinkError> {
    if link.is_empty() || link.chars().any(char::is_control) {
        return Err(WikiLinkError::InvalidUrl);
    }
    match link {
        "app:home" => return Ok(WikiLink::AppHome),
        "app:logs" => return Ok(WikiLink::AppLogs),
        "app:sandbox" => return Ok(WikiLink::AppSandbox),
        "app:settings" => return Ok(WikiLink::AppSettings),
        "app:settings/api" => return Ok(WikiLink::AppSettingsSection(WikiSettingsSection::Api)),
        "app:settings/characters" => {
            return Ok(WikiLink::AppSettingsSection(
                WikiSettingsSection::Characters,
            ));
        }
        "app:settings/voice" => {
            return Ok(WikiLink::AppSettingsSection(WikiSettingsSection::Voice));
        }
        "app:settings/language" => {
            return Ok(WikiLink::AppSettingsSection(WikiSettingsSection::Language));
        }
        "app:settings/microphone" => {
            return Ok(WikiLink::AppSettingsSection(
                WikiSettingsSection::Microphone,
            ));
        }
        "app:settings/models" => {
            return Ok(WikiLink::AppSettingsSection(WikiSettingsSection::Models));
        }
        "app:settings/models?section=RAG" => {
            return Ok(WikiLink::AppSettingsSection(WikiSettingsSection::ModelsRag));
        }
        "app:settings/screen" => {
            return Ok(WikiLink::AppSettingsSection(WikiSettingsSection::Screen));
        }
        _ => {}
    }
    if let Some(anchor) = link.strip_prefix('#') {
        return if anchor.is_empty() || anchor.contains('#') {
            Err(WikiLinkError::InvalidUrl)
        } else {
            Ok(WikiLink::SamePageAnchor(anchor))
        };
    }
    if link.starts_with("app:") {
        return Err(WikiLinkError::UnsupportedDeepLink);
    }
    if link.starts_with("https://") || link.starts_with("http://") {
        return Ok(WikiLink::External(link));
    }
    let (path, anchor) = match link.split_once('#') {
        Some((path, anchor)) if !anchor.is_empty() && !anchor.contains('#') => (path, Some(anchor)),
        Some(_) => return Err(WikiLinkError::InvalidUrl),
        None => (link, None),
    };
    let file = path.strip_prefix("./").unwrap_or(path);
    let Some(entry) = article(file) else {
        return Err(WikiLinkError::UnsupportedPath);
    };
    Ok(WikiLink::Article {
        id: entry.id,
        anchor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_wiki_is_bilingual_and_exact() {
        assert_eq!(ARTICLES.len(), 6);
        for page in ARTICLES {
            for locale in ["ru", "en"] {
                assert!(page.markdown(locale).starts_with("# "));
                assert!(!page.title(locale).is_empty());
            }
            assert!(article(page.id).is_some());
        }
        assert!(article("../index.md").is_none());
        assert!(article("other.md").is_none());
    }

    #[test]
    fn only_embedded_relative_articles_are_navigable() {
        assert_eq!(
            resolve_link("./index.md"),
            Ok(WikiLink::Article {
                id: "index.md",
                anchor: None
            })
        );
        assert_eq!(
            resolve_link("./chat-and-characters.md#setup"),
            Ok(WikiLink::Article {
                id: "chat-and-characters.md",
                anchor: Some("setup")
            })
        );
        for link in [
            "../../RAG_Guide.md",
            "../ru/index.md",
            "/etc/passwd",
            "file:///etc/passwd",
            "../../TROUBLESHOOTING.md",
            "unknown.md",
            "././index.md",
        ] {
            assert_eq!(
                resolve_link(link),
                Err(WikiLinkError::UnsupportedPath),
                "{link}"
            );
        }
        assert_eq!(resolve_link("./index.md#"), Err(WikiLinkError::InvalidUrl));
        assert_eq!(
            resolve_link("#быстрый-старт"),
            Ok(WikiLink::SamePageAnchor("быстрый-старт"))
        );
        assert_eq!(resolve_link("#"), Err(WikiLinkError::InvalidUrl));
        assert_eq!(resolve_link("\0"), Err(WikiLinkError::InvalidUrl));
    }

    #[test]
    fn python_wiki_home_cards_are_bilingual_and_navigable() {
        assert_eq!(HOME_CARDS.len(), 6);
        for card in HOME_CARDS {
            assert!(!card.title("ru").is_empty());
            assert!(!card.title("en").is_empty());
            assert!(!card.body("ru").is_empty());
            assert!(!card.body("en").is_empty());
            assert!(resolve_link(card.destination).is_ok(), "{}", card.destination);
        }
    }

    #[test]
    fn bundled_app_links_never_silently_open_unrelated_pages() {
        let mut supported = std::collections::BTreeSet::new();
        let mut unsupported = std::collections::BTreeSet::new();
        for article in ARTICLES {
            for locale in ["ru", "en"] {
                for tail in article.markdown(locale).split("](").skip(1) {
                    let Some(link) = tail.split_once(')').map(|(link, _)| link) else {
                        continue;
                    };
                    if !link.starts_with("app:") {
                        continue;
                    }
                    match resolve_link(link) {
                        Ok(WikiLink::AppSettingsSection(_))
                        | Ok(WikiLink::AppSettings)
                        | Ok(WikiLink::AppHome)
                        | Ok(WikiLink::AppLogs)
                        | Ok(WikiLink::AppSandbox) => {
                            supported.insert(link);
                        }
                        Err(WikiLinkError::UnsupportedDeepLink)
                            if link == "app:settings/data_collection" =>
                        {
                            unsupported.insert(link);
                        }
                        result => panic!("unexpected bundled Wiki destination {link}: {result:?}"),
                    }
                }
            }
        }
        assert_eq!(supported.len(), 12);
        assert_eq!(unsupported.len(), 1);
        assert!(supported.contains("app:settings/models?section=RAG"));
    }

    #[test]
    fn app_links_have_explicit_destinations() {
        assert_eq!(resolve_link("app:settings"), Ok(WikiLink::AppSettings));
        assert_eq!(resolve_link("app:home"), Ok(WikiLink::AppHome));
        assert_eq!(resolve_link("app:logs"), Ok(WikiLink::AppLogs));
        assert_eq!(resolve_link("app:sandbox"), Ok(WikiLink::AppSandbox));
        for (link, section) in [
            ("app:settings/api", WikiSettingsSection::Api),
            ("app:settings/characters", WikiSettingsSection::Characters),
            ("app:settings/voice", WikiSettingsSection::Voice),
            ("app:settings/language", WikiSettingsSection::Language),
            ("app:settings/microphone", WikiSettingsSection::Microphone),
            ("app:settings/models", WikiSettingsSection::Models),
            (
                "app:settings/models?section=RAG",
                WikiSettingsSection::ModelsRag,
            ),
            ("app:settings/screen", WikiSettingsSection::Screen),
        ] {
            assert_eq!(
                resolve_link(link),
                Ok(WikiLink::AppSettingsSection(section))
            );
        }
        for link in [
            "app:settings/data_collection",
            "app:settings/models?section=Unknown",
            "app:settings/models?section=rag",
            "app:settings/api?section=RAG",
            "app:settings/api/extra",
            "app:settings/api#unknown",
            "app:settings/%2e%2e",
            "app:settings/../api",
        ] {
            assert_eq!(
                resolve_link(link),
                Err(WikiLinkError::UnsupportedDeepLink),
                "{link}"
            );
        }
        assert_eq!(
            resolve_link("https://github.com/"),
            Ok(WikiLink::External("https://github.com/"))
        );
    }
}
