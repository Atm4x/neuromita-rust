use std::{collections::HashSet, future::Future, pin::Pin};
use tokio::sync::OnceCell;

use crate::{
    updater::{GithubRelease, GithubUpdateCatalog, check::is_native_asset, valid_github_segment},
    version::RELEASE_REPOSITORY,
};

#[derive(Clone, Debug)]
pub struct ReleaseRecord {
    pub id: String,
    pub title: String,
    pub date: String,
    pub summary: String,
    pub details: String,
    pub url: String,
    pub prerelease: bool,
}

pub type ReleaseFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ReleaseRecord>, String>> + Send + 'a>>;

pub trait ReleaseFeed: Send + Sync {
    fn fetch(&self) -> ReleaseFuture<'_>;
}

#[derive(Default)]
pub struct GithubReleaseFeed {
    catalog: OnceCell<GithubUpdateCatalog>,
}

impl ReleaseFeed for GithubReleaseFeed {
    fn fetch(&self) -> ReleaseFuture<'_> {
        Box::pin(async move {
            let catalog = self
                .catalog
                .get_or_try_init(|| async { GithubUpdateCatalog::new(RELEASE_REPOSITORY) })
                .await
                .map_err(|error| error.to_string())?;
            catalog
                .releases()
                .await
                .map(project_releases)
                .map_err(|error| error.to_string())
        })
    }
}

fn project_releases(releases: Vec<GithubRelease>) -> Vec<ReleaseRecord> {
    let mut seen = HashSet::new();
    let mut cards = Vec::new();
    for release in releases {
        let tag = release.tag_name;
        if release.draft
            || !valid_github_segment(&tag)
            || !release
                .assets
                .iter()
                .any(|asset| is_native_asset(&asset.name))
            || !seen.insert(tag.clone())
        {
            continue;
        }
        let details = normalize_body(release.body.as_deref().unwrap_or_default());
        let url = format!("https://github.com/{RELEASE_REPOSITORY}/releases/tag/{tag}");
        let summary = summarize_body(&details);
        cards.push(ReleaseRecord {
            id: tag.clone(),
            title: release
                .name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("NeuroMita {tag}")),
            date: release
                .published_at
                .unwrap_or_default()
                .chars()
                .take(10)
                .collect(),
            summary,
            details,
            url,
            prerelease: release.prerelease,
        });
    }
    cards.sort_by(|a, b| b.date.cmp(&a.date));
    cards
}

fn normalize_body(body: &str) -> String {
    body.lines()
        .map(|line| {
            let mut line = line.trim();
            while line.starts_with('#') {
                line = &line[1..];
            }
            line = line.trim();
            if line.starts_with("```") {
                return String::new();
            }
            line.replace("**", "").replace('`', "")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn summarize_body(body: &str) -> String {
    let mut meaningful = body
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("Изменения")
                && !line.starts_with("Changes")
                && !line.starts_with("Установка:")
                && !line.starts_with("Installation:")
        })
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    if meaningful.is_empty() {
        return String::new();
    }
    if meaningful.chars().count() > 280 {
        meaningful = meaningful.chars().take(277).collect::<String>();
        if let Some(index) = meaningful.rfind(' ') {
            meaningful.truncate(index);
        }
        meaningful.push_str(" …");
    } else if body.lines().filter(|line| !line.trim().is_empty()).count() > 3 {
        meaningful.push_str(" …");
    }
    meaningful
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_lists_only_published_native_assets() {
        let asset = |name| {
            serde_json::json!({
                "name": name,
                "size": 1,
                "browser_download_url": "https://github.com/fixture",
                "digest": null
            })
        };
        let releases = serde_json::from_value(serde_json::json!([
            {"tag_name":"v1.0.0","assets":[asset("PythonBuild.zip")]},
            {"tag_name":"v0.2.0","assets":[asset("neuromita-gui-windows-x86_64.exe")]},
            {"tag_name":"v0.2.0","assets":[asset("neuromita-gui-windows-x86_64.exe")]},
            {"tag_name":"v0.3.0","draft":true,"assets":[asset("neuromita-gui-windows-x86_64.exe")]}
        ]))
        .unwrap();
        let result = project_releases(releases);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "v0.2.0");
        assert_eq!(
            result[0].url,
            format!("https://github.com/{RELEASE_REPOSITORY}/releases/tag/v0.2.0")
        );
    }

    #[test]
    fn summary_uses_valid_unicode_boundaries() {
        let input = "Привет ".repeat(100);
        let result = summarize_body(&input);
        assert!(result.chars().count() <= 280);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn markdown_heading_is_cleaned() {
        assert_eq!(
            normalize_body("## Исправления\n\n**Новый** чат"),
            "Исправления\n\nНовый чат"
        );
    }
}
