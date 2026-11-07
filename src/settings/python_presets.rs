use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write},
    path::Path,
};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::generation::{StoredPresets, python_import::PythonPresetImport};

const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum PythonPresetStageError {
    #[error("invalid Python preset input: {0}")]
    Invalid(String),
    #[error("{0} Python preset(s) cannot be converted; nothing was written")]
    Blocked(usize),
    #[error("destination must be an absolute, new directory with a non-symlink parent")]
    UnsafeDestination,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("output published, but durability sync failed: {0}")]
    PublishedButNotSynced(io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonPresetStagePreview {
    pub converted_ids: Vec<u32>,
    pub blocked_ids: Vec<u32>,
    pub blocked_without_id: usize,
    pub template_count: usize,
    pub preserved_ids: Vec<u32>,
    pub python_order_not_imported: bool,
}

impl PythonPresetStagePreview {
    pub fn blocked_count(&self) -> usize {
        self.blocked_ids.len() + self.blocked_without_id
    }
}

pub struct PythonPresetStage {
    pub preview: PythonPresetStagePreview,
    files: Vec<(&'static str, Vec<u8>)>,
}

impl PythonPresetStage {
    pub fn prepare(presets: &[u8], templates: &[u8]) -> Result<Self, PythonPresetStageError> {
        if presets.len() > MAX_SOURCE_BYTES || templates.len() > MAX_SOURCE_BYTES {
            return Err(PythonPresetStageError::Invalid(
                "source JSON exceeds 8 MiB".into(),
            ));
        }
        let preset_json = parse_unique_json(presets)?;
        let template_json = parse_unique_json(templates)?;
        let sources = preset_json
            .get("presets")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                PythonPresetStageError::Invalid("expected current Python presets object".into())
            })?;
        let template_ids = template_json
            .get("templates")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                PythonPresetStageError::Invalid("expected current Python templates object".into())
            })?;
        if sources.is_empty() {
            return Err(PythonPresetStageError::Invalid(
                "no Python presets to import".into(),
            ));
        }
        let plan = PythonPresetImport::parse(presets, templates).map_err(|_| {
            PythonPresetStageError::Invalid("cannot parse Python preset format".into())
        })?;
        let mut blocked_ids = Vec::new();
        let mut blocked_without_id = 0;
        let mut valid_ids = Vec::new();
        let mut occurrences = BTreeMap::<u32, usize>::new();
        let template_numeric_ids: BTreeSet<u32> = template_ids
            .keys()
            .filter_map(|value| value.parse::<u32>().ok())
            .collect();
        for source_id in sources.keys() {
            if let Ok(id) = source_id.parse::<u32>() {
                *occurrences.entry(id).or_default() += 1;
            }
        }
        for (source_id, source_record) in sources {
            let source_record_id = source_record.get("id").and_then(Value::as_u64);
            let id = source_id.parse::<u32>().ok().filter(|id| *id > 0);
            if id.is_none()
                || id.is_some_and(|id| template_numeric_ids.contains(&id))
                || id.is_some_and(|id| occurrences.get(&id).copied().unwrap_or(0) != 1)
                || id.is_some_and(|id| source_record_id.is_some_and(|inner| inner != id as u64))
                || plan
                    .blockers
                    .iter()
                    .any(|blocker| blocker.source_id == *source_id)
                || !id.is_some_and(|id| plan.records.contains_key(&crate::generation::PresetId(id)))
            {
                if let Some(id) = id {
                    blocked_ids.push(id);
                } else {
                    blocked_without_id += 1;
                }
            } else if let Some(id) = id {
                valid_ids.push(id);
            }
        }
        valid_ids.sort_unstable();
        blocked_ids.sort_unstable();
        let preview = PythonPresetStagePreview {
            converted_ids: valid_ids,
            blocked_ids,
            blocked_without_id,
            template_count: template_ids.len(),
            preserved_ids: Vec::new(),
            python_order_not_imported: preset_json.get("order").is_some(),
        };
        let files = if preview.blocked_count() == 0 {
            vec![
                (
                    "presets.json",
                    plan.catalog_bytes().map_err(|_| {
                        PythonPresetStageError::Invalid("cannot encode preset catalog".into())
                    })?,
                ),
                (
                    "preset-credentials.json",
                    plan.credential_bytes().map_err(|_| {
                        PythonPresetStageError::Invalid("cannot encode credential store".into())
                    })?,
                ),
                (
                    "python-template-import.json",
                    serde_json::to_vec_pretty(&plan.templates).map_err(|_| {
                        PythonPresetStageError::Invalid("cannot encode template descriptors".into())
                    })?,
                ),
                ("python-source-api-presets.json", presets.to_vec()),
                ("python-source-api-templates.json", templates.to_vec()),
            ]
        } else {
            Vec::new()
        };
        Ok(Self { preview, files })
    }

    /// Merge into a new review directory, never into the running profile.
    /// Existing bytes are snapshots supplied by the stopped application's caller.
    pub fn merge_with_existing(
        &self,
        existing_catalog: Option<&[u8]>,
        existing_credentials: Option<&[u8]>,
    ) -> Result<Self, PythonPresetStageError> {
        if self.preview.blocked_count() != 0 {
            return Err(PythonPresetStageError::Blocked(
                self.preview.blocked_count(),
            ));
        }
        let staged_catalog = self.file("presets.json")?;
        let staged_credentials = self.file("preset-credentials.json")?;
        let imported: StoredPresets = serde_json::from_value(parse_unique_json(staged_catalog)?)
            .map_err(|_| PythonPresetStageError::Invalid("invalid staged preset catalog".into()))?;
        let existing: Option<StoredPresets> = existing_catalog
            .map(|bytes| {
                if bytes.len() > MAX_SOURCE_BYTES {
                    return Err(PythonPresetStageError::Invalid(
                        "existing catalog exceeds 8 MiB".into(),
                    ));
                }
                serde_json::from_value(parse_unique_json(bytes)?).map_err(|_| {
                    PythonPresetStageError::Invalid("invalid existing preset catalog".into())
                })
            })
            .transpose()?;
        let merged =
            StoredPresets::merge_for_import(existing.as_ref(), &imported).map_err(|_| {
                PythonPresetStageError::Invalid(
                    "preset IDs collide with existing or retired IDs".into(),
                )
            })?;
        let mut combined_secrets = parse_credentials(existing_credentials)?;
        let new_secrets = parse_credentials(Some(staged_credentials))?;
        for (name, secret) in new_secrets {
            if combined_secrets.insert(name.clone(), secret).is_some() {
                return Err(PythonPresetStageError::Invalid(
                    "imported credential name conflicts with an existing name".into(),
                ));
            }
        }
        if merged
            .local_credentials()
            .iter()
            .any(|name| !combined_secrets.contains_key(*name))
        {
            return Err(PythonPresetStageError::Invalid(
                "a preset references an unavailable local credential".into(),
            ));
        }
        let catalog_bytes = serde_json::to_vec_pretty(&merged).map_err(|_| {
            PythonPresetStageError::Invalid("cannot encode merged preset catalog".into())
        })?;
        let credential_bytes = serde_json::to_vec_pretty(&combined_secrets).map_err(|_| {
            PythonPresetStageError::Invalid("cannot encode merged credentials".into())
        })?;
        if credential_bytes.len() > 1_048_576 {
            return Err(PythonPresetStageError::Invalid(
                "merged credential store exceeds 1 MiB".into(),
            ));
        }
        let mut files = self.files.clone();
        for (name, bytes) in &mut files {
            match *name {
                "presets.json" => *bytes = catalog_bytes.clone(),
                "preset-credentials.json" => *bytes = credential_bytes.clone(),
                _ => {}
            }
        }
        if let Some(bytes) = existing_catalog {
            files.push(("existing-source-presets.json", bytes.to_vec()));
        }
        if let Some(bytes) = existing_credentials {
            files.push(("existing-source-preset-credentials.json", bytes.to_vec()));
        }
        let mut preview = self.preview.clone();
        preview.preserved_ids = existing
            .as_ref()
            .map_or_else(Vec::new, StoredPresets::id_values);
        Ok(Self { preview, files })
    }

    pub(super) fn file(&self, name: &str) -> Result<&[u8], PythonPresetStageError> {
        self.files
            .iter()
            .find_map(|(path, bytes)| (*path == name).then_some(bytes.as_slice()))
            .ok_or_else(|| PythonPresetStageError::Invalid("staged file missing".into()))
    }

    pub fn stage_new_directory(&self, destination: &Path) -> Result<(), PythonPresetStageError> {
        if self.preview.blocked_count() != 0 {
            return Err(PythonPresetStageError::Blocked(
                self.preview.blocked_count(),
            ));
        }
        if !destination.is_absolute()
            || destination.file_name().is_none()
            || symlink_or_exists(destination)
        {
            return Err(PythonPresetStageError::UnsafeDestination);
        }
        let parent = destination
            .parent()
            .ok_or(PythonPresetStageError::UnsafeDestination)?;
        let parent_meta =
            fs::symlink_metadata(parent).map_err(|_| PythonPresetStageError::UnsafeDestination)?;
        if !parent_meta.is_dir() || parent_meta.file_type().is_symlink() {
            return Err(PythonPresetStageError::UnsafeDestination);
        }
        let sibling = destination.with_file_name(format!(
            ".python-presets-stage-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&sibling)?;
        let result = (|| -> Result<(), PythonPresetStageError> {
            for (name, bytes) in &self.files {
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(sibling.join(name))?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            #[cfg(unix)]
            fs::File::open(&sibling)?.sync_all()?;
            if symlink_or_exists(destination) {
                return Err(PythonPresetStageError::UnsafeDestination);
            }
            fs::rename(&sibling, destination)?;
            #[cfg(unix)]
            fs::File::open(parent)
                .and_then(|file| file.sync_all())
                .map_err(PythonPresetStageError::PublishedButNotSynced)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&sibling);
        }
        result
    }
}

pub(super) fn parse_credentials(
    bytes: Option<&[u8]>,
) -> Result<BTreeMap<String, String>, PythonPresetStageError> {
    let Some(bytes) = bytes else {
        return Ok(BTreeMap::new());
    };
    if bytes.len() > 1_048_576 {
        return Err(PythonPresetStageError::Invalid(
            "credential store exceeds 1 MiB".into(),
        ));
    }
    serde_json::from_value(parse_unique_json(bytes)?)
        .map_err(|_| PythonPresetStageError::Invalid("invalid credential store".into()))
}

fn symlink_or_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

struct StrictJson(Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = StrictJson;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictJson(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictJson(Value::Number(v.into())))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictJson(Value::Number(v.into())))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .map(StrictJson)
                    .ok_or_else(|| E::custom("nonfinite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictJson(Value::String(v.into())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictJson(Value::String(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictJson(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictJson(Value::Null))
            }
            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                StrictJson::deserialize(deserializer)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(item) = seq.next_element::<StrictJson>()? {
                    values.push(item.0);
                }
                Ok(StrictJson(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, val)) = map.next_entry::<String, StrictJson>()? {
                    if values.insert(key, val.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(StrictJson(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

pub(super) fn parse_unique_json(bytes: &[u8]) -> Result<Value, PythonPresetStageError> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let parsed = StrictJson::deserialize(&mut decoder)
        .map_err(|_| PythonPresetStageError::Invalid("malformed or duplicate-key JSON".into()))?;
    decoder
        .end()
        .map_err(|_| PythonPresetStageError::Invalid("trailing JSON data".into()))?;
    Ok(parsed.0)
}
