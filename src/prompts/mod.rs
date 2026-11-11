mod dsl;
mod expression;
mod post_dsl;
pub use post_dsl::{PostDslProgram, PostDslResult};

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

use serde_json::Value;
use thiserror::Error;

use crate::character::CharacterId;

#[derive(Debug, Error)]
pub enum PromptError {
    #[error("prompt filesystem: {0}")]
    Io(#[from] std::io::Error),
    #[error("prompt metadata: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("invalid prompt selection: {0}")]
    InvalidSelection(String),
    #[error("prompt set is missing: {0}")]
    MissingSet(String),
    #[error("prompt DSL: {0}")]
    Dsl(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSetInfo {
    pub character: CharacterId,
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptInitializationDeclaration {
    Memory {
        priority: String,
        content: String,
        entities: Vec<String>,
        rag_only: bool,
    },
    Island {
        kind: String,
        content: String,
    },
    Relation {
        subject: String,
        relation: String,
        object: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPrompt {
    pub set_name: String,
    pub stable: Vec<String>,
    pub context: Vec<String>,
    /// Declarative initialization operations; rendering never writes memory.
    pub initialization: Vec<PromptInitializationDeclaration>,
}

impl RenderedPrompt {
    pub fn system_text(&self) -> String {
        self.stable
            .iter()
            .chain(&self.context)
            .filter(|x| !x.trim().is_empty())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

const MAX_SNAPSHOT_FILES: usize = 2048;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SNAPSHOT_FILE_BYTES: usize = 1024 * 1024;

/// Request-scoped, fully owned prompt sources; no disk reads during execution.
#[derive(Debug, Clone)]
pub struct PromptSetSnapshot {
    root: PathBuf,
    base: PathBuf,
    set_name: String,
    defaults: BTreeMap<String, Value>,
    files: Arc<BTreeMap<PathBuf, Arc<str>>>,
    post_dsl: Option<PostDslProgram>,
}

impl PromptSetSnapshot {
    pub fn name(&self) -> &str {
        &self.set_name
    }

    pub fn has_template(&self, template: &str) -> bool {
        self.files.contains_key(&self.base.join(template))
    }

    pub fn post_dsl(&self) -> Option<PostDslProgram> {
        self.post_dsl.clone()
    }

    pub fn render(
        &self,
        context_variables: BTreeMap<String, Value>,
        template: &str,
    ) -> Result<RenderedPrompt, PromptError> {
        let mut vars = self.defaults.clone();
        vars.extend(context_variables);
        for feature in [
            "SEED_INITIAL_MEMORIES",
            "SEED_CHARACTER_MEMORIES",
            "SEED_WORLD_LORE",
            "SEED_ROOMS_LORE",
            "SEED_MITAS_LORE",
            "SEED_ENTITY_LINKS",
        ] {
            vars.entry(feature.to_owned()).or_insert(Value::Bool(false));
        }
        vars.insert(
            "PROMPT_SET_NAME".into(),
            Value::String(self.set_name.clone()),
        );
        let mut runtime = dsl::Interpreter::from_snapshot(
            self.root.clone(),
            self.base.clone(),
            Arc::clone(&self.files),
            vars,
        );
        let (stable, context) = runtime.render_main(template)?;
        Ok(RenderedPrompt {
            set_name: self.set_name.clone(),
            stable,
            context,
            initialization: runtime.declarations(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct PromptCatalog {
    root: PathBuf,
    admitted: Arc<Mutex<BTreeMap<(CharacterId, String), Weak<PromptSetSnapshot>>>>,
}

impl PromptCatalog {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            admitted: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn validate_set_name(name: &str) -> Result<(), PromptError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.starts_with('.')
            || name.contains('/')
            || name.contains('\\')
            || name.chars().any(char::is_control)
        {
            return Err(PromptError::InvalidSelection(name.to_owned()));
        }
        Ok(())
    }

    pub fn list(&self, character: CharacterId) -> Result<Vec<PromptSetInfo>, PromptError> {
        let directory = self.root.join(character.to_string());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut sets = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if Self::validate_set_name(&name).is_err() || name == "System" || name == "__pycache__"
            {
                continue;
            }
            if !entry.file_type()?.is_dir() || !entry.path().join("main_template.txt").is_file() {
                continue;
            }
            let mut author = String::new();
            let mut version = String::new();
            let mut description = String::new();
            for file in ["info.json", "config.json"] {
                let path = entry.path().join(file);
                if path.is_file() {
                    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
                    let metadata = value.get("metadata").unwrap_or(&value);
                    if author.is_empty() {
                        author = metadata
                            .get("author")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                    }
                    if version.is_empty() {
                        version = metadata
                            .get("version")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                    }
                    if description.is_empty() {
                        description = metadata
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                    }
                }
            }
            sets.push(PromptSetInfo {
                character,
                name,
                author,
                version,
                description,
            });
        }
        sets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(sets)
    }

    pub fn resolve_set(
        &self,
        character: CharacterId,
        selected: Option<&str>,
    ) -> Result<String, PromptError> {
        let options = self.list(character)?;
        let name = if let Some(name) = selected {
            Self::validate_set_name(name)?;
            name.to_owned()
        } else {
            options
                .iter()
                .find(|set| set.name.eq_ignore_ascii_case("Default"))
                .or_else(|| options.first())
                .ok_or_else(|| PromptError::MissingSet(character.to_string()))?
                .name
                .clone()
        };
        if !options.iter().any(|set| set.name == name) {
            return Err(PromptError::MissingSet(format!("{character}/{name}")));
        }
        Ok(name)
    }

    /// Captures the selected character's prompt resources and shared includes
    /// before a request is placed in the queue.
    pub fn capture(
        &self,
        character: CharacterId,
        selected: Option<&str>,
    ) -> Result<Option<Arc<PromptSetSnapshot>>, PromptError> {
        if selected.is_none() && self.list(character)?.is_empty() {
            return Ok(None);
        }
        let name = self.resolve_set(character, selected)?;
        let root = self.root.canonicalize()?;
        let base = root
            .join(character.to_string())
            .join(&name)
            .canonicalize()?;
        if !base.starts_with(root.join(character.to_string())) {
            return Err(PromptError::InvalidSelection(name));
        }
        let mut files = BTreeMap::new();
        let mut total = 0;
        // A set may include scripts from another set belonging to the same
        // character, or the shared directories, so those are captured together.
        for directory in [
            root.join(character.to_string()),
            root.join("Common"),
            root.join("Structural"),
            root.join("_CommonPrompts"),
            root.join("_CommonScripts"),
        ] {
            if directory.exists() {
                capture_directory(&directory, &root, &mut files, &mut total)?;
            }
        }
        if !files.contains_key(&base.join("main_template.txt")) {
            return Err(PromptError::MissingSet(format!(
                "{character}/{name}/main_template.txt"
            )));
        }
        let key = (character, name.clone());
        if let Some(previous) = self
            .admitted
            .lock()
            .expect("prompt cache poisoned")
            .get(&key)
            .and_then(Weak::upgrade)
            && previous.files.as_ref() == &files
        {
            return Ok(Some(previous));
        }
        let config = files
            .get(&base.join("config.json"))
            .map(|content| serde_json::from_str::<Value>(content))
            .transpose()?;
        let defaults = config_defaults(config.as_ref())?;
        let params = config
            .as_ref()
            .and_then(|v| v.get("custom_params"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let post_script = files.get(&base.join("PostScripts/main_rules.postscript"));
        let post_dsl = if post_script.is_some() || !params.is_empty() {
            Some(
                PostDslProgram::parse(post_script.map_or("", |s| s.as_ref()), defaults.clone())?
                    .with_custom_params(&params)?,
            )
        } else {
            None
        };
        let snapshot = Arc::new(PromptSetSnapshot {
            root,
            base,
            set_name: name,
            defaults,
            files: Arc::new(files),
            post_dsl,
        });
        self.admitted
            .lock()
            .expect("prompt cache poisoned")
            .insert(key, Arc::downgrade(&snapshot));
        Ok(Some(snapshot))
    }

    pub fn load_post_rules(
        &self,
        character: CharacterId,
        set_name: &str,
    ) -> Result<Option<PostDslProgram>, PromptError> {
        Self::validate_set_name(set_name)?;
        let dir = self.root.join(character.to_string()).join(set_name);
        let path = dir.join("PostScripts/main_rules.postscript");
        let script = match fs::read_to_string(path) {
            Ok(script) => Some(script),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let config = dir.join("config.json");
        let config: Option<Value> = if config.is_file() {
            Some(serde_json::from_slice(&fs::read(config)?)?)
        } else {
            None
        };
        let params = config
            .as_ref()
            .and_then(|data| data.get("custom_params"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if script.is_none() && params.is_empty() {
            return Ok(None);
        }
        let defaults = self.read_defaults(&dir)?;
        let parsed = PostDslProgram::parse(script.as_deref().unwrap_or(""), defaults)?
            .with_custom_params(&params)?;
        Ok(Some(parsed))
    }

    fn read_defaults(&self, dir: &Path) -> Result<BTreeMap<String, Value>, PromptError> {
        let config_path = dir.join("config.json");
        let value: Option<Value> = if config_path.is_file() {
            Some(serde_json::from_slice(&fs::read(config_path)?)?)
        } else {
            None
        };
        config_defaults(value.as_ref())
    }

    pub fn render(
        &self,
        character: CharacterId,
        selected: Option<&str>,
        context_variables: BTreeMap<String, Value>,
        template: &str,
    ) -> Result<RenderedPrompt, PromptError> {
        let set_name = self.resolve_set(character, selected)?;
        let dir = self.root.join(character.to_string()).join(&set_name);
        let mut vars = self.read_defaults(&dir)?;
        vars.extend(context_variables);
        for feature in [
            "SEED_INITIAL_MEMORIES",
            "SEED_CHARACTER_MEMORIES",
            "SEED_WORLD_LORE",
            "SEED_ROOMS_LORE",
            "SEED_MITAS_LORE",
            "SEED_ENTITY_LINKS",
        ] {
            vars.entry(feature.to_owned()).or_insert(Value::Bool(false));
        }
        vars.insert("PROMPT_SET_NAME".into(), Value::String(set_name.clone()));
        let mut runtime = dsl::Interpreter::new(&self.root, &dir, vars)?;
        let rendered = runtime.render_main(template)?;
        Ok(RenderedPrompt {
            set_name,
            stable: rendered.0,
            context: rendered.1,
            initialization: runtime.declarations(),
        })
    }
}

fn config_defaults(config: Option<&Value>) -> Result<BTreeMap<String, Value>, PromptError> {
    let mut vars = BTreeMap::new();
    if let Some(value) = config {
        let config = value
            .as_object()
            .ok_or_else(|| PromptError::Dsl("config.json must be an object".into()))?;
        let defaults = config
            .get("variables")
            .and_then(Value::as_object)
            .unwrap_or(config);
        for (key, value) in defaults {
            if key != "metadata"
                && key != "custom_params"
                && !matches!(key.as_str(), "author" | "version" | "description")
            {
                vars.insert(key.clone(), value.clone());
            }
        }
        if let Some(params) = config.get("custom_params").and_then(Value::as_array) {
            for param in params {
                if let (Some(name), Some(initial)) = (
                    param.get("name").and_then(Value::as_str),
                    param.get("initial"),
                ) {
                    vars.entry(name.to_owned()).or_insert(initial.clone());
                }
            }
        }
    }
    Ok(vars)
}

fn capture_directory(
    directory: &Path,
    root: &Path,
    files: &mut BTreeMap<PathBuf, Arc<str>>,
    total: &mut usize,
) -> Result<(), PromptError> {
    if !directory.starts_with(root) {
        return Err(PromptError::Dsl("prompt directory escaped root".into()));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(PromptError::Dsl(format!(
                "prompt symlink is unsupported: {}",
                entry.path().display()
            )));
        }
        if kind.is_dir() {
            capture_directory(&entry.path(), root, files, total)?;
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        if !matches!(
            entry.path().extension().and_then(|s| s.to_str()),
            Some("txt" | "script" | "system" | "postscript" | "json")
        ) {
            continue;
        }
        if files.len() >= MAX_SNAPSHOT_FILES {
            return Err(PromptError::Dsl("too many prompt files in snapshot".into()));
        }
        let length = entry.metadata()?.len() as usize;
        if length > MAX_SNAPSHOT_FILE_BYTES || length > MAX_SNAPSHOT_BYTES.saturating_sub(*total) {
            return Err(PromptError::Dsl(
                "prompt snapshot size limit exceeded".into(),
            ));
        }
        let data = fs::read_to_string(entry.path())?
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .trim_end()
            .to_owned();
        *total += data.len();
        if *total > MAX_SNAPSHOT_BYTES {
            return Err(PromptError::Dsl(
                "prompt snapshot size limit exceeded".into(),
            ));
        }
        files.insert(entry.path(), Arc::from(data));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::CharacterId;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "neuromita-prompt-test-{}-{unique}",
            std::process::id()
        ));
        let set = root.join("Crazy/Default");
        fs::create_dir_all(set.join("Main")).unwrap();
        fs::write(set.join("config.json"), r#"{"attitude":60,"player_name":"Игрок","metadata":{"author":"A","version":"2","description":"Example"}}"#).unwrap();
        fs::write(
            set.join("main_template.txt"),
            "[<Main/identity.txt>]\n[<@ Main/state.script>]\n",
        )
        .unwrap();
        fs::write(set.join("Main/identity.txt"), "Привет [{player_name}].").unwrap();
        fs::write(
            set.join("Main/state.script"),
            "IF attitude < 40 THEN\n RETURN 'Злюсь'\nELSE\n RETURN 'Спокойна'\nENDIF\n",
        )
        .unwrap();
        root
    }
    #[test]
    fn admitted_snapshot_is_immutable_after_files_change_or_disappear() {
        let root = root();
        let set = root.join("Crazy/Default");
        fs::create_dir_all(set.join("PostScripts")).unwrap();
        fs::create_dir_all(root.join("_CommonPrompts")).unwrap();
        fs::write(set.join("config.json"), r#"{"variables":{"player_name":"Old"},"custom_params":[{"name":"Love","initial":4,"change_command":"love_change","op":"add"}]}"#).unwrap();
        fs::write(
            set.join("main_template.txt"),
            "[<_CommonPrompts/shared.txt>]\n[<Main/identity.txt>]\n[<@Main/state.script>]",
        )
        .unwrap();
        fs::write(root.join("_CommonPrompts/shared.txt"), "Shared-old").unwrap();
        fs::write(set.join("Main/identity.txt"), "Hello [{player_name}]").unwrap();
        fs::write(set.join("Main/state.script"), "RETURN 'State-old'").unwrap();
        fs::write(set.join("PostScripts/main_rules.postscript"),
            "RULE text\n MATCH TEXT \"old\"\n ACTIONS\n REPLACE_MATCH WITH \"archived\"\n END_ACTIONS\nEND_RULE").unwrap();
        let catalog = PromptCatalog::new(&root);
        let admitted = catalog.capture(CharacterId::Crazy, None).unwrap().unwrap();
        let another = catalog.capture(CharacterId::Crazy, None).unwrap().unwrap();
        assert!(Arc::ptr_eq(&admitted, &another));
        fs::write(
            set.join("config.json"),
            r#"{"variables":{"player_name":"New"}}"#,
        )
        .unwrap();
        fs::write(set.join("Main/state.script"), "RETURN 'State-new'").unwrap();
        fs::write(root.join("_CommonPrompts/shared.txt"), "Shared-new").unwrap();
        fs::write(
            set.join("PostScripts/main_rules.postscript"),
            "broken new postscript",
        )
        .unwrap();
        fs::remove_file(set.join("Main/identity.txt")).unwrap();
        let rendered = admitted
            .render(BTreeMap::new(), "main_template.txt")
            .unwrap();
        assert_eq!(rendered.stable, vec!["Shared-old", "Hello Old"]);
        assert_eq!(rendered.context, vec!["State-old"]);
        let old = admitted
            .post_dsl()
            .unwrap()
            .apply(
                crate::generation::StructuredResponse::spoken("old"),
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(old.response.full_text().as_ref(), "archived");
        assert!(catalog.capture(CharacterId::Crazy, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_prompt_is_rejected_before_admission_and_symlinks_do_not_escape() {
        let root = root();
        let catalog = PromptCatalog::new(&root);
        assert!(matches!(
            catalog.capture(CharacterId::Crazy, Some("missing")),
            Err(PromptError::MissingSet(_))
        ));
        fs::write(root.join("Crazy/Default/config.json"), "not JSON").unwrap();
        assert!(
            catalog
                .capture(CharacterId::Crazy, Some("Default"))
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn list_select_and_render_are_isolated_by_request() {
        let dir = root();
        let catalog = PromptCatalog::new(&dir);
        let options = catalog.list(CharacterId::Crazy).unwrap();
        assert_eq!(options[0].author, "A");
        let first = catalog
            .render(
                CharacterId::Crazy,
                None,
                BTreeMap::new(),
                "main_template.txt",
            )
            .unwrap();
        assert_eq!(first.stable, vec!["Привет Игрок."]);
        assert_eq!(first.context, vec!["Спокойна"]);
        let second = catalog
            .render(
                CharacterId::Crazy,
                Some("Default"),
                BTreeMap::from([("attitude".into(), Value::from(10))]),
                "main_template.txt",
            )
            .unwrap();
        assert_eq!(second.context, vec!["Злюсь"]);
        assert!(
            catalog
                .render(
                    CharacterId::Crazy,
                    Some("../Default"),
                    BTreeMap::new(),
                    "main_template.txt"
                )
                .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn python_keyword_casing_and_local_updates_match_active_dsl() {
        // NeuroMitaPython src/DSL/dsl_engine.py (97059b0a): SET LOCAL
        // and the optional IF/ELSEIF THEN keyword are case-insensitive.
        let root = root();
        let script = root.join("Crazy/Default/Main/state.script");
        fs::write(
            &script,
            "SET local score = 3\nSET score = score + 2\nIF score == 5 then\n RETURN str(score)\nELSE\n RETURN 'incorrect'\nENDIF\n",
        )
        .unwrap();
        let catalog = PromptCatalog::new(&root);
        let rendered = catalog
            .render(
                CharacterId::Crazy,
                None,
                BTreeMap::new(),
                "main_template.txt",
            )
            .unwrap();
        assert_eq!(rendered.context, vec!["5".to_owned()]);

        // The Python engine rejects an ELSE with arguments rather than
        // silently treating it as an unconditional branch.
        fs::write(
            &script,
            "IF False\n RETURN 'never'\nELSE unexpected\n RETURN 'incorrect'\nENDIF\n",
        )
        .unwrap();
        let error = catalog
            .render(
                CharacterId::Crazy,
                None,
                BTreeMap::new(),
                "main_template.txt",
            )
            .unwrap_err();
        assert!(error.to_string().contains("ELSE must not have arguments"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_variables_are_script_scoped_and_reset_on_each_render() {
        let root = root();
        let set = root.join("Crazy/Default");
        fs::write(set.join("config.json"), r#"{"variables":{"score":99}}"#).unwrap();
        fs::write(
            set.join("main_template.txt"),
            "[<@Main/state.script>]\n[<@Main/next.script>]\n",
        )
        .unwrap();
        fs::write(
            set.join("Main/state.script"),
            "SET LOCAL score = 3\nSET score = score + 2\nRETURN str(score)\n",
        )
        .unwrap();
        fs::write(set.join("Main/next.script"), "RETURN str(score)\n").unwrap();
        let snapshot = PromptCatalog::new(&root)
            .capture(CharacterId::Crazy, None)
            .unwrap()
            .unwrap();
        for _ in 0..2 {
            let result = snapshot
                .render(BTreeMap::new(), "main_template.txt")
                .unwrap();
            assert_eq!(result.context, vec!["5".to_owned(), "99".to_owned()]);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_b_a_character_switch_keeps_snapshots_and_defaults_isolated() {
        let root = root();
        let kind = root.join("Kind/Default");
        fs::create_dir_all(kind.join("Main")).unwrap();
        fs::write(
            kind.join("config.json"),
            r#"{"variables":{"player_name":"KindName"}}"#,
        )
        .unwrap();
        fs::write(kind.join("main_template.txt"), "[<Main/identity.txt>]\n").unwrap();
        let kind_identity = kind.join("Main/identity.txt");
        fs::write(&kind_identity, "Kind [{player_name}]").unwrap();
        let catalog = PromptCatalog::new(&root);
        let a = catalog.capture(CharacterId::Crazy, None).unwrap().unwrap();
        let b = catalog.capture(CharacterId::Kind, None).unwrap().unwrap();
        assert_eq!(
            a.render(BTreeMap::new(), "main_template.txt")
                .unwrap()
                .stable,
            vec!["Привет Игрок.".to_owned()]
        );
        assert_eq!(
            b.render(BTreeMap::new(), "main_template.txt")
                .unwrap()
                .stable,
            vec!["Kind KindName".to_owned()]
        );
        assert_eq!(
            a.render(BTreeMap::new(), "main_template.txt")
                .unwrap()
                .stable,
            vec!["Привет Игрок.".to_owned()]
        );

        fs::write(&kind_identity, "Changed [{player_name}]").unwrap();
        assert_eq!(
            b.render(BTreeMap::new(), "main_template.txt")
                .unwrap()
                .stable,
            vec!["Kind KindName".to_owned()]
        );
        let newly_admitted = catalog.capture(CharacterId::Kind, None).unwrap().unwrap();
        assert_eq!(
            newly_admitted
                .render(BTreeMap::new(), "main_template.txt")
                .unwrap()
                .stable,
            vec!["Changed KindName".to_owned()]
        );
        assert_eq!(
            a.render(BTreeMap::new(), "main_template.txt")
                .unwrap()
                .stable,
            vec!["Привет Игрок.".to_owned()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn post_rules_use_selected_character_set_and_capture_config() {
        let root = root();
        let default_dir = root.join("Crazy/Default");
        fs::create_dir_all(default_dir.join("PostScripts")).unwrap();
        fs::write(
            default_dir.join("config.json"),
            r#"{"custom_params":[{"name":"Love","change_command":"love_change","initial":10,"op":"add","type":"int","change_min":-2,"change_max":2}]}"#,
        ).unwrap();
        fs::write(
            default_dir.join("PostScripts/main_rules.postscript"),
            "RULE stars\n MATCH REGEX \"\\*\\*\"\n ACTIONS\n REMOVE_MATCH\n END_ACTIONS\nEND_RULE\n",
        ).unwrap();
        let other_dir = root.join("Crazy/Other");
        fs::create_dir_all(other_dir.join("PostScripts")).unwrap();
        fs::write(other_dir.join("main_template.txt"), "Other template").unwrap();
        fs::write(other_dir.join("PostScripts/main_rules.postscript"),
            "RULE rewrite\n MATCH TEXT \"hello\"\n ACTIONS\n REPLACE_MATCH WITH \"world\"\n END_ACTIONS\nEND_RULE\n").unwrap();
        let catalog = PromptCatalog::new(&root);
        let defaults = catalog
            .load_post_rules(CharacterId::Crazy, "Default")
            .unwrap()
            .unwrap();
        let other = catalog
            .load_post_rules(CharacterId::Crazy, "Other")
            .unwrap()
            .unwrap();
        let mut text = crate::generation::StructuredResponse::spoken("**hello**");
        text.custom_fields = Some(serde_json::Map::from_iter([(
            "love_change".into(),
            Value::from(100),
        )]));
        let first = defaults.apply(text.clone(), &BTreeMap::new()).unwrap();
        assert_eq!(first.response.full_text().as_ref(), "hello");
        assert_eq!(first.variables["Love"], 12.0);
        let second = other
            .apply(
                crate::generation::StructuredResponse::spoken("hello"),
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(second.response.full_text().as_ref(), "world");
        fs::write(default_dir.join("config.json"), "{broken JSON").unwrap();
        let captured = defaults.apply(text, &BTreeMap::new()).unwrap();
        assert_eq!(captured.variables["Love"], 12.0);
        assert!(
            catalog
                .load_post_rules(CharacterId::Crazy, "Default")
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn captures_initialization_without_mutating_timeline() {
        let root = root();
        let scripts = root.join("Crazy/Default");
        fs::write(scripts.join("main_template.txt"), "[<Main/init.script>]\n").unwrap();
        fs::write(scripts.join("Main/init.script"), "SEED_MEMORY High | Initial setting ENTITIES Mita, Player\nLINK_ENTITIES Mita -> knows -> Player\nSEED_RAG_MEMORY Normal | Project facts\nRETURN f\"\"\"hello\nworld\"\"\"\n").unwrap();
        let rendered = PromptCatalog::new(&root)
            .render(
                CharacterId::Crazy,
                None,
                BTreeMap::new(),
                "main_template.txt",
            )
            .unwrap();
        assert_eq!(rendered.stable, vec!["hello\nworld"]);
        assert_eq!(rendered.initialization.len(), 3);
        assert_eq!(
            rendered.initialization[0],
            PromptInitializationDeclaration::Memory {
                priority: "High".into(),
                content: "Initial setting".into(),
                entities: vec!["Mita".into(), "Player".into()],
                rag_only: false,
            }
        );
        assert!(
            matches!(&rendered.initialization[1], PromptInitializationDeclaration::Relation { relation, .. } if relation == "knows")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(test)]
    mod prompt_seed_compatibility_tests {
        use super::*;
        use crate::identity::CharacterId;

        fn root() -> PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "neuromita-prompt-seed-test-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(dir.join("Crazy/Default/Main")).unwrap();
            dir
        }

        #[test]
        fn config_controls_seed_feature() {
            let root = root();
            let set = root.join("Crazy/Default");
            fs::write(set.join("main_template.txt"), "[<Main/seed.script>]").unwrap();
            fs::write(set.join("Main/seed.script"), "IF SEED_CHARACTER_MEMORIES != False THEN\nSEED_MEMORY High | seeded from config\nENDIF\n").unwrap();
            let catalog = PromptCatalog::new(&root);
            assert!(
                catalog
                    .render(
                        CharacterId::Crazy,
                        None,
                        BTreeMap::new(),
                        "main_template.txt"
                    )
                    .unwrap()
                    .initialization
                    .is_empty()
            );
            fs::write(
                set.join("config.json"),
                r#"{"SEED_CHARACTER_MEMORIES":true}"#,
            )
            .unwrap();
            assert_eq!(
                catalog
                    .render(
                        CharacterId::Crazy,
                        None,
                        BTreeMap::new(),
                        "main_template.txt"
                    )
                    .unwrap()
                    .initialization
                    .len(),
                1
            );
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn local_assignment_uses_first_local_value() {
            let root = root();
            let set = root.join("Crazy/Default");
            fs::write(set.join("main_template.txt"), "[<@ Main/seed.script>]").unwrap();
            fs::write(set.join("Main/seed.script"), "SET LOCAL label = 'first'\nSET LOCAL label = 'second'\nSET label = label + '-updated'\nRETURN label\n").unwrap();
            let result = PromptCatalog::new(&root)
                .render(
                    CharacterId::Crazy,
                    None,
                    BTreeMap::new(),
                    "main_template.txt",
                )
                .unwrap();
            assert_eq!(result.context, vec!["first-updated"]);
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn seed_strings_interpolate_config_variables() {
            let root = root();
            let set = root.join("Crazy/Default");
            fs::write(set.join("config.json"), r#"{"player_name":"Игрок"}"#).unwrap();
            fs::write(set.join("main_template.txt"), "[<Main/seed.script>]").unwrap();
            fs::write(set.join("Main/seed.script"), "SEED_MEMORY High | Mita knows {player_name}\nSEED_ISLAND relationship | Friend of {player_name}\n").unwrap();
            let result = PromptCatalog::new(&root)
                .render(
                    CharacterId::Crazy,
                    None,
                    BTreeMap::new(),
                    "main_template.txt",
                )
                .unwrap();
            assert!(
                matches!(&result.initialization[0], PromptInitializationDeclaration::Memory {content, ..} if content == "Mita knows Игрок")
            );
            assert!(
                matches!(&result.initialization[1], PromptInitializationDeclaration::Island {content, ..} if content == "Friend of Игрок")
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}
