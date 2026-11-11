use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::digest::{Context, SHA256};

const MAX_SINGLE_BINARY: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BINARY: usize = 64 * 1024 * 1024;

pub(super) fn validate_assets(envelope: &PythonEnvelope) -> Result<(), PythonArchiveError> {
    let mut seen = HashMap::<&str, (&str, usize)>::new();
    let mut budget = 0_usize;
    for asset in &envelope.assets {
        let (major, minor) = asset
            .media_type
            .split_once('/')
            .ok_or(PythonArchiveError::Invalid("invalid asset MIME"))?;
        let starts_alphanumeric = |part: &str| {
            part.as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        };
        if !starts_alphanumeric(major)
            || !starts_alphanumeric(minor)
            || asset.media_type == "image/jpg"
            || asset.media_type.len() > 127
            || !major.bytes().chain(minor.bytes()).all(|ch| {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || b"!#$&^_.+-".contains(&ch)
            })
        {
            return Err(PythonArchiveError::Invalid("invalid asset MIME"));
        }
        if !asset.asset_id.starts_with("sha256:")
            || asset.asset_id.len() != 71
            || !asset.asset_id[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(PythonArchiveError::Invalid("asset identity"));
        }
        let bytes = STANDARD
            .decode(&asset.base64)
            .map_err(|_| PythonArchiveError::Invalid("invalid asset base64"))?;
        if bytes.len() > MAX_SINGLE_BINARY {
            return Err(PythonArchiveError::Invalid("asset too large"));
        }
        budget = budget
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_TOTAL_BINARY)
            .ok_or(PythonArchiveError::Invalid("asset budget"))?;
        let mut content = Context::new(&SHA256);
        content.update(asset.media_type.as_bytes());
        content.update(&[0]);
        content.update(&bytes);
        let hex = content
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if asset.asset_id != format!("sha256:{hex}")
            || seen
                .insert(&asset.asset_id, (&asset.media_type, bytes.len()))
                .is_some()
        {
            return Err(PythonArchiveError::Invalid("invalid or duplicate binary"));
        }
    }
    let mut references = HashMap::new();
    fn visit<'a>(
        value: &'a Value,
        refs: &mut HashMap<&'a str, (&'a str, usize)>,
    ) -> Result<(), PythonArchiveError> {
        match value {
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) == Some("image_ref") {
                    let id = map
                        .get("asset_id")
                        .and_then(Value::as_str)
                        .ok_or(PythonArchiveError::Invalid("asset reference"))?;
                    let mime = map
                        .get("media_type")
                        .and_then(Value::as_str)
                        .ok_or(PythonArchiveError::Invalid("asset reference"))?;
                    let size = map
                        .get("byte_size")
                        .and_then(Value::as_u64)
                        .and_then(|size| usize::try_from(size).ok())
                        .ok_or(PythonArchiveError::Invalid("asset reference"))?;
                    if let Some(previous) = refs.insert(id, (mime, size))
                        && previous != (mime, size)
                    {
                        return Err(PythonArchiveError::Invalid("conflicting reference"));
                    }
                }
                for item in map.values() {
                    visit(item, refs)?;
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, refs)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    for event in &envelope.events {
        visit(&event.payload, &mut references)?;
    }
    for (id, reference) in &references {
        if seen.get(id).copied() != Some(*reference) {
            return Err(PythonArchiveError::Invalid(
                "missing or mismatched referenced asset",
            ));
        }
    }
    if seen.len() != references.len() {
        return Err(PythonArchiveError::Invalid("unreferenced archive asset"));
    }
    Ok(())
}
