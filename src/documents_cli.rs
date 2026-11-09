use std::{ffi::OsString, io, path::PathBuf};

use neuromita::{
    character::CharacterId,
    document_revision::DocumentRevisionApi,
    knowledge::document_index::{self, SearchStatus},
};

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Reindex {
        root: PathBuf,
        character: CharacterId,
        revisions_directory: PathBuf,
    },
    Build {
        root: PathBuf,
        character: CharacterId,
        index: PathBuf,
    },
    Verify {
        root: PathBuf,
        character: CharacterId,
        index: PathBuf,
    },
    Query {
        root: PathBuf,
        character: CharacterId,
        index: PathBuf,
        text: String,
        limit: usize,
    },
    Help,
}

fn parse(args: Vec<OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let mode = args
        .next()
        .ok_or("expected build, reindex, verify or query")?;
    if mode == "--help" {
        return if args.next().is_none() {
            Ok(Action::Help)
        } else {
            Err("unknown help arguments".into())
        };
    }
    if mode != "build" && mode != "reindex" && mode != "verify" && mode != "query" {
        return Err("expected build, reindex, verify or query".into());
    }
    let mut root = None;
    let mut character = None;
    let mut index = None;
    let mut revisions_directory = None;
    let mut text = None;
    let mut limit = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing option value")?;
        match flag.to_str() {
            Some("--root") if root.is_none() => root = Some(PathBuf::from(value)),
            Some("--index") if index.is_none() => index = Some(PathBuf::from(value)),
            Some("--revisions-dir") if mode == "reindex" && revisions_directory.is_none() => {
                revisions_directory = Some(PathBuf::from(value));
            }
            Some("--character") if character.is_none() => {
                let name = value.into_string().map_err(|_| "invalid character")?;
                character = Some(CharacterId::parse(&name).ok_or("unknown character")?);
            }
            Some("--text") if mode == "query" && text.is_none() => {
                text = Some(value.into_string().map_err(|_| "invalid query text")?);
            }
            Some("--limit") if mode == "query" && limit.is_none() => {
                let value = value.into_string().map_err(|_| "invalid limit")?;
                limit = Some(value.parse::<usize>().map_err(|_| "invalid limit")?);
            }
            _ => return Err("unknown or duplicate documents option".into()),
        }
    }
    let root = root.ok_or("--root is required")?;
    let character = character.ok_or("--character is required")?;
    if !root.is_absolute() {
        return Err("source path must be absolute".into());
    }
    if mode == "reindex" {
        if index.is_some() || text.is_some() || limit.is_some() {
            return Err("reindex accepts only a revisions directory, not an index path".into());
        }
        let revisions_directory = revisions_directory.ok_or("--revisions-dir is required")?;
        if !revisions_directory.is_absolute() {
            return Err("revisions directory must be absolute".into());
        }
        return Ok(Action::Reindex {
            root,
            character,
            revisions_directory,
        });
    }
    let index = index.ok_or("--index is required")?;
    if !index.is_absolute() {
        return Err("index path must be absolute".into());
    }
    match mode.to_str() {
        Some("build") => Ok(Action::Build {
            root,
            character,
            index,
        }),
        Some("verify") => Ok(Action::Verify {
            root,
            character,
            index,
        }),
        Some("query") => Ok(Action::Query {
            root,
            character,
            index,
            text: text.ok_or("--text is required")?,
            limit: limit.unwrap_or(5),
        }),
        _ => Err("unknown action".into()),
    }
}

pub fn run(args: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let action = parse(args).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    match action {
        Action::Help => println!(
            "neuromita documents reindex --root ABSOLUTE_CHARACTER_DOCUMENT_DIRECTORY --character Kind --revisions-dir EXISTING_ABSOLUTE_DIRECTORY\nneuromita documents build --root ABSOLUTE_CHARACTER_DOCUMENT_DIRECTORY --character Kind --index NEW_ABSOLUTE_PATH\nneuromita documents verify --root ABSOLUTE_CHARACTER_DOCUMENT_DIRECTORY --character Kind --index ABSOLUTE_PATH\nneuromita documents query --root ABSOLUTE_CHARACTER_DOCUMENT_DIRECTORY --character Kind --index ABSOLUTE_PATH --text QUERY [--limit 5]\n\nExplicit lexical-only Documents source. .txt/.md only, single directory, source-verified derived index; reindex never activates. Activate a receipt through DocumentRevisionApi and SettingsApi in a trusted running application."
        ),
        Action::Reindex {
            root,
            character,
            revisions_directory,
        } => {
            let receipt = DocumentRevisionApi::prepare(&root, character, &revisions_directory)?;
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Action::Build {
            root,
            character,
            index,
        } => {
            let built = document_index::build_new(&root, character, &index)?;
            println!(
                "indexed {} documents for {character}: {}",
                built.documents.len(),
                index.display()
            );
        }
        Action::Verify {
            root,
            character,
            index,
        } => {
            let verified = document_index::load_verified(&root, character, &index)?;
            println!(
                "verified {} documents for {character}",
                verified.documents.len()
            );
        }
        Action::Query {
            root,
            character,
            index,
            text,
            limit,
        } => {
            let result = document_index::search_verified(&root, character, &index, &text, limit)?;
            let hits = result
                .hits
                .into_iter()
                .map(|hit| {
                    serde_json::json!({
                        "id":hit.key.id,
                        "source":"Document",
                        "content":hit.content,
                        "score":hit.score,
                        "metadata":hit.metadata,
                    })
                })
                .collect::<Vec<_>>();
            let status = match result.status {
                SearchStatus::EmptyCorpus => "empty_corpus",
                SearchStatus::NoMatches => "no_matches",
                SearchStatus::Selected => "selected",
            };
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"status":status,"documents":result.documents,"hits":hits})
                )?
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_rejects_ambiguous_or_unsafe_requests() {
        let p = |values: &[&str]| parse(values.iter().map(OsString::from).collect());
        assert_eq!(p(&["--help"]), Ok(Action::Help));
        for args in [
            vec![
                "query",
                "--root",
                "/tmp/docs",
                "--character",
                "Kind",
                "--index",
                "/tmp/index",
            ],
            vec![
                "build",
                "--root",
                "docs",
                "--character",
                "Kind",
                "--index",
                "/tmp/index",
            ],
            vec![
                "build",
                "--root",
                "/tmp/docs",
                "--character",
                "Wrong",
                "--index",
                "/tmp/index",
            ],
            vec![
                "build",
                "--root",
                "/tmp/docs",
                "--character",
                "Kind",
                "--index",
                "/tmp/index",
                "--index",
                "/tmp/index2",
            ],
            vec!["verify", "--root", "/tmp/docs", "--character", "Kind"],
        ] {
            assert!(p(&args).is_err(), "{args:?}");
        }
    }
}
