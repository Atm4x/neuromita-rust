//! Strict, deterministic, single-format transfer of one canonical Timeline session.
//! No migration chain, alternate history, compression codec or process-wide path.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, SecondsFormat, Utc};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::timeline::domain::{Branch, Receipt};
use crate::timeline::domain::{
    BranchId, BranchInfo, EventDraft, EventKind, SessionId, SessionProvenance, TimelineAssetRef,
    TimelineError, TimelineFact, TimelineStore, canonical_message_content, parse_message_content,
};

#[cfg(feature = "sqlite-system")]
use super::SqliteTimelineRepository;
use super::assets::{TimelineAssetError, TimelineAssetInventory, TimelineAssetStore};

const FORMAT: &str = "neuromita.timeline.session";
const VERSION: u32 = 1;
const MAX_ARCHIVE_BYTES: usize = 96 * 1024 * 1024;
const MAX_ASSETS: usize = 256;
const MAX_BRANCHES: usize = 256;
const MAX_FACTS: usize = 100_000;
const MAX_SINGLE_BINARY: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BINARY: u64 = 64 * 1024 * 1024;

type ArchiveBinaries = Vec<(TimelineAssetRef, Vec<u8>)>;
type PreparedImport = (
    SessionId,
    Vec<Branch>,
    ArchiveBinaries,
    Option<SessionProvenance>,
);

#[derive(Debug, Error)]
pub enum SessionArchiveError {
    #[error("invalid or incompatible session archive: {0}")]
    Invalid(&'static str),
    #[error("session archive identity already exists in destination")]
    Conflict,
    #[error("pinned external archive is missing")]
    MissingPinnedArchive,
    #[error("pinned external archive bytes differ from their registered digest")]
    PinnedArchiveChanged,
    #[error("pinned external archive I/O failed: {0}")]
    ArchiveIo(#[from] std::io::Error),
    #[error("session archive serialization failed: {0}")]
    Encoding(String),
    #[error(transparent)]
    Timeline(#[from] TimelineError),
    #[error(transparent)]
    Asset(#[from] TimelineAssetError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    version: u32,
    session_id: SessionId,
    root: BranchId,
    created_at_utc: String,
    branches: Vec<ArchivedBranch>,
    assets: Vec<ArchivedAsset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<SessionProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivedBranch {
    info: BranchInfo,
    events: Vec<TimelineFact>,
    receipts: Vec<ArchivedReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivedReceipt {
    operation_key: String,
    command: Value,
    drafts: Vec<EventDraft>,
    result: Vec<TimelineFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivedAsset {
    reference: TimelineAssetRef,
    data_base64: String,
}

impl ArchivedBranch {
    fn from_branch(branch: Branch) -> Self {
        let mut receipts = branch
            .receipts
            .into_iter()
            .map(|(operation_key, receipt)| ArchivedReceipt {
                operation_key,
                command: receipt.command,
                drafts: receipt.drafts,
                result: receipt.result,
            })
            .collect::<Vec<_>>();
        receipts.sort_by(|a, b| a.operation_key.cmp(&b.operation_key));
        Self {
            info: branch.info,
            events: branch.events,
            receipts,
        }
    }

    fn to_branch(&self) -> Branch {
        Branch {
            info: self.info,
            events: self.events.clone(),
            receipts: self
                .receipts
                .iter()
                .map(|receipt| {
                    (
                        receipt.operation_key.clone(),
                        Receipt::new(
                            receipt.command.clone(),
                            receipt.drafts.clone(),
                            receipt.result.clone(),
                        ),
                    )
                })
                .collect(),
        }
    }
}

fn topological(mut branches: Vec<Branch>) -> Result<Vec<Branch>, SessionArchiveError> {
    if branches.is_empty() || branches.len() > MAX_BRANCHES {
        return Err(SessionArchiveError::Invalid("branch count"));
    }
    branches.sort_by_key(|branch| branch.info.id.0);
    let mut ordered = Vec::with_capacity(branches.len());
    let mut seen = HashSet::new();
    while !branches.is_empty() {
        let position = branches
            .iter()
            .position(|branch| {
                branch
                    .info
                    .parent
                    .is_none_or(|parent| seen.contains(&parent))
            })
            .ok_or(SessionArchiveError::Invalid("cyclic or missing parent"))?;
        let branch = branches.remove(position);
        seen.insert(branch.info.id);
        ordered.push(branch);
    }
    Ok(ordered)
}

fn archive_timestamp(branches: &[Branch]) -> Result<String, SessionArchiveError> {
    let earliest = branches
        .iter()
        .flat_map(|branch| &branch.events)
        .map(|fact| fact.draft.occurred_at_unix_ms)
        .min()
        .unwrap_or(0);
    let earliest =
        i64::try_from(earliest).map_err(|_| SessionArchiveError::Invalid("invalid timestamp"))?;
    let utc = DateTime::<Utc>::from_timestamp_millis(earliest)
        .ok_or(SessionArchiveError::Invalid("invalid timestamp"))?;
    Ok(utc.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn local_content(draft: &EventDraft) -> Option<&Value> {
    if !matches!(
        draft.kind,
        EventKind::PlayerMessage
            | EventKind::CharacterMessage
            | EventKind::CharacterObservation
            | EventKind::MessageRevised
            | EventKind::SessionNote
            | EventKind::ReminderToolResult
    ) {
        return None;
    }
    canonical_message_content(draft)
}

fn collect_references(
    branches: &[ArchivedBranch],
) -> Result<BTreeMap<String, TimelineAssetRef>, SessionArchiveError> {
    let mut refs = BTreeMap::new();
    for branch in branches {
        for fact in &branch.events {
            if let Some(content) = local_content(&fact.draft) {
                let parts = parse_message_content(content)
                    .map_err(|_| SessionArchiveError::Invalid("invalid message parts"))?;
                for reference in parts.iter().filter_map(|part| part.asset()) {
                    let previous = refs.insert(reference.digest.clone(), reference.clone());
                    if previous.is_some_and(|previous| previous != *reference) {
                        return Err(SessionArchiveError::Invalid("inconsistent asset identity"));
                    }
                }
            }
        }
    }
    Ok(refs)
}

fn references(
    branches: &[ArchivedBranch],
) -> Result<BTreeMap<String, TimelineAssetRef>, SessionArchiveError> {
    let refs = collect_references(branches)?;
    if refs.len() > MAX_ASSETS {
        return Err(SessionArchiveError::Invalid("asset count"));
    }
    Ok(refs)
}

fn content_digest(reference: &TimelineAssetRef, bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hash = Context::new(&SHA256);
    hash.update(reference.media_type.as_bytes());
    hash.update(&[0]);
    hash.update(bytes);
    let mut digest = String::with_capacity(64);
    for byte in hash.finish().as_ref() {
        write!(&mut digest, "{byte:02x}").expect("cannot fail to write to String");
    }
    digest
}

fn checked_binaries(
    envelope: &Envelope,
    references: &BTreeMap<String, TimelineAssetRef>,
) -> Result<Vec<(TimelineAssetRef, Vec<u8>)>, SessionArchiveError> {
    if envelope.assets.len() != references.len() {
        return Err(SessionArchiveError::Invalid("missing or extra asset"));
    }
    let mut budget = 0_u64;
    let mut decoded = Vec::with_capacity(envelope.assets.len());
    let mut seen = HashSet::new();
    for asset in &envelope.assets {
        if references.get(&asset.reference.digest) != Some(&asset.reference)
            || !seen.insert(asset.reference.digest.clone())
        {
            return Err(SessionArchiveError::Invalid("invalid or duplicate asset"));
        }
        if asset.reference.size > MAX_SINGLE_BINARY {
            return Err(SessionArchiveError::Invalid("asset size"));
        }
        budget = budget
            .checked_add(asset.reference.size)
            .filter(|size| *size <= MAX_TOTAL_BINARY)
            .ok_or(SessionArchiveError::Invalid("asset budget"))?;
        let bytes = STANDARD
            .decode(&asset.data_base64)
            .map_err(|_| SessionArchiveError::Invalid("invalid asset encoding"))?;
        if bytes.len() as u64 != asset.reference.size
            || content_digest(&asset.reference, &bytes) != asset.reference.digest
        {
            return Err(SessionArchiveError::Invalid("asset digest mismatch"));
        }
        decoded.push((asset.reference.clone(), bytes));
    }
    Ok(decoded)
}

fn validate_receipts(branch: &ArchivedBranch) -> Result<(), SessionArchiveError> {
    let events_by_sequence = branch
        .events
        .iter()
        .map(|f| (f.sequence, f))
        .collect::<BTreeMap<_, _>>();
    let mut covered = HashSet::new();
    let mut operation_keys = HashSet::new();
    for receipt in &branch.receipts {
        if receipt.operation_key.is_empty()
            || receipt.operation_key.trim() != receipt.operation_key
            || !operation_keys.insert(&receipt.operation_key)
            || !receipt.command.is_object()
            || receipt.drafts.len() != receipt.result.len()
        {
            return Err(SessionArchiveError::Invalid("receipt shape"));
        }
        let mut previous = 0;
        for (draft, fact) in receipt.drafts.iter().zip(&receipt.result) {
            if fact.draft != *draft
                || events_by_sequence.get(&fact.sequence) != Some(&fact)
                || !covered.insert(fact.sequence)
                || (previous != 0 && fact.sequence != previous + 1)
            {
                return Err(SessionArchiveError::Invalid("receipt event mismatch"));
            }
            previous = fact.sequence;
        }
    }
    for fact in &branch.events {
        if fact.draft.kind != EventKind::BranchCreated && !covered.contains(&fact.sequence) {
            return Err(SessionArchiveError::Invalid("event has no receipt"));
        }
    }
    Ok(())
}

fn validate_envelope(
    envelope: &Envelope,
) -> Result<(Vec<Branch>, ArchiveBinaries), SessionArchiveError> {
    if envelope.format != FORMAT || envelope.version != VERSION {
        return Err(SessionArchiveError::Invalid(
            "unknown archive format or version",
        ));
    }
    let utc = DateTime::parse_from_rfc3339(&envelope.created_at_utc)
        .map_err(|_| SessionArchiveError::Invalid("timestamp not ISO 8601"))?;
    if utc.offset().local_minus_utc() != 0 || !envelope.created_at_utc.ends_with('Z') {
        return Err(SessionArchiveError::Invalid("timestamp must be UTC Z"));
    }
    if envelope.branches.is_empty() || envelope.branches.len() > MAX_BRANCHES {
        return Err(SessionArchiveError::Invalid("branch count"));
    }
    let total_events = envelope
        .branches
        .iter()
        .map(|branch| branch.events.len())
        .sum::<usize>();
    if total_events > MAX_FACTS {
        return Err(SessionArchiveError::Invalid("event count"));
    }
    let mut roots = 0;
    let mut global_ids = HashSet::new();
    let mut previous_branches = Vec::new();
    for branch in &envelope.branches {
        if branch.info.session_id != envelope.session_id {
            return Err(SessionArchiveError::Invalid("foreign session branch"));
        }
        if branch.info.parent.is_none() {
            roots += 1;
            if branch.info.id != envelope.root {
                return Err(SessionArchiveError::Invalid("root mismatch"));
            }
        }
        validate_receipts(branch)?;
        for fact in &branch.events {
            if !global_ids.insert(fact.draft.id) {
                return Err(SessionArchiveError::Invalid("duplicate event identity"));
            }
            let timestamp = i64::try_from(fact.draft.occurred_at_unix_ms)
                .map_err(|_| SessionArchiveError::Invalid("invalid event timestamp"))?;
            if DateTime::<Utc>::from_timestamp_millis(timestamp).is_none() {
                return Err(SessionArchiveError::Invalid("invalid event timestamp"));
            }
        }
        previous_branches.push(branch.to_branch());
    }
    if roots != 1 {
        return Err(SessionArchiveError::Invalid("expected exactly one root"));
    }
    let ordered = topological(previous_branches)?;
    let detached = TimelineStore::load_persisted(ordered.clone())?;
    for branch in &ordered {
        let info = branch.info;
        let synthetic_markers = branch
            .events
            .iter()
            .filter(|fact| fact.draft.kind == EventKind::BranchCreated)
            .count();
        if synthetic_markers != usize::from(info.parent.is_some()) {
            return Err(SessionArchiveError::Invalid("unexpected branch marker"));
        }
        let inherited = if let (Some(parent), Some(checkpoint)) = (info.parent, info.checkpoint) {
            let fact = detached.event(parent, checkpoint)?;
            if fact.sequence != info.fork_sequence {
                return Err(SessionArchiveError::Invalid("invalid fork anchor sequence"));
            }
            let marker = branch
                .events
                .first()
                .ok_or(SessionArchiveError::Invalid("missing fork marker"))?;
            let mut expected_payload = serde_json::Map::new();
            expected_payload.insert(
                "parent_branch_id".into(),
                Value::String(parent.0.to_string()),
            );
            match fact.draft.kind {
                EventKind::Checkpoint => {
                    expected_payload.insert(
                        "checkpoint_id".into(),
                        Value::String(checkpoint.0.to_string()),
                    );
                }
                EventKind::PlayerMessage => {
                    let character = marker
                        .draft
                        .payload
                        .get("character_id")
                        .and_then(Value::as_str)
                        .and_then(crate::identity::CharacterId::parse)
                        .ok_or(SessionArchiveError::Invalid("invalid input fork recipient"))?;
                    if fact.draft.actor.as_deref() != Some("player")
                        || !fact.draft.observers.contains(&character)
                    {
                        return Err(SessionArchiveError::Invalid("invalid input fork witness"));
                    }
                    expected_payload.insert(
                        "input_event_id".into(),
                        Value::String(checkpoint.0.to_string()),
                    );
                    expected_payload
                        .insert("character_id".into(), Value::String(character.to_string()));
                }
                _ => return Err(SessionArchiveError::Invalid("invalid fork anchor kind")),
            }
            if marker.draft.kind != EventKind::BranchCreated
                || marker.draft.payload != expected_payload
            {
                return Err(SessionArchiveError::Invalid("invalid branch marker"));
            }
            detached
                .world(parent)?
                .into_iter()
                .take_while(|fact| fact.sequence <= info.fork_sequence)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut known_ids = inherited
            .iter()
            .map(|fact| fact.draft.id)
            .collect::<HashSet<_>>();
        let mut external_ids = inherited
            .iter()
            .filter_map(|fact| fact.draft.external_message_id.as_deref())
            .collect::<HashSet<_>>();
        for fact in &branch.events {
            if !known_ids.insert(fact.draft.id) {
                return Err(SessionArchiveError::Invalid("reused event in branch world"));
            }
            for link in &fact.draft.links {
                if !known_ids.contains(&link.event_id) || link.event_id == fact.draft.id {
                    return Err(SessionArchiveError::Invalid("broken causal reference"));
                }
            }
            if let Some(external) = fact.draft.external_message_id.as_deref()
                && (external.is_empty()
                    || external.trim() != external
                    || !external_ids.insert(external))
            {
                return Err(SessionArchiveError::Invalid("duplicate external identity"));
            }
            if fact.draft.schema_version != 1 {
                return Err(SessionArchiveError::Invalid("unsupported event schema"));
            }
        }
    }
    TimelineStore::validate_archive_receipts(&ordered)?;
    if let Some(provenance) = &envelope.provenance {
        TimelineStore::load_persisted_with_provenance(
            ordered.clone(),
            vec![(envelope.session_id, provenance.clone())],
        )?;
    }
    if archive_timestamp(&ordered)? != envelope.created_at_utc {
        return Err(SessionArchiveError::Invalid(
            "inconsistent archive timestamp",
        ));
    }
    let refs = references(&envelope.branches)?;
    let binaries = checked_binaries(envelope, &refs)?;
    Ok((ordered, binaries))
}

/// Read-only retention manifest across every canonical session/branch and each
/// explicitly supplied, integrity-checked external archive. Includes hidden,
/// revised and historical facts; never derives retention from current MitaView.
/// Does not delete files or claim that the caller supplied every external archive.
pub fn retention_manifest_memory(
    store: &TimelineStore,
    retained_archives: &[&[u8]],
) -> Result<Vec<TimelineAssetRef>, SessionArchiveError> {
    let local = store
        .persisted_branches()
        .into_iter()
        .map(ArchivedBranch::from_branch)
        .collect::<Vec<_>>();
    let mut retained = collect_references(&local)?;
    for archive in retained_archives {
        let (_, _, binaries, _) = prepare_import(archive)?;
        for (reference, _) in binaries {
            if retained
                .insert(reference.digest.clone(), reference.clone())
                .is_some_and(|previous| previous != reference)
            {
                return Err(SessionArchiveError::Invalid("inconsistent asset identity"));
            }
        }
    }
    Ok(retained.into_values().collect())
}

#[cfg(feature = "sqlite-system")]
pub fn retention_manifest_sqlite(
    store: &SqliteTimelineRepository,
    retained_archives: &[&[u8]],
) -> Result<Vec<TimelineAssetRef>, SessionArchiveError> {
    retention_manifest_memory(&store.archive_snapshot()?, retained_archives)
}

/// A caller-owned external archive location, pinned to its exact serialized bytes.
/// This is a *retention input*, not proof that every external archive was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedExternalArchive {
    path: PathBuf,
    sha256: String,
}

impl PinnedExternalArchive {
    pub fn new(
        path: impl AsRef<Path>,
        sha256: impl Into<String>,
    ) -> Result<Self, SessionArchiveError> {
        let path = path.as_ref();
        let sha256 = sha256.into();
        if !path.is_absolute()
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(SessionArchiveError::Invalid("invalid external archive pin"));
        }
        Ok(Self {
            path: path.to_owned(),
            sha256,
        })
    }

    /// Pin the original archive bytes at an explicitly supplied absolute path.
    /// The bytes are independently verified against the path at every inventory.
    pub fn for_export(path: impl AsRef<Path>, bytes: &[u8]) -> Result<Self, SessionArchiveError> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(SessionArchiveError::Invalid("external archive too large"));
        }
        let sha256 = archive_sha256(bytes);
        Self::new(path, sha256)
    }

    fn read_checked(&self) -> Result<Vec<u8>, SessionArchiveError> {
        let metadata = fs::symlink_metadata(&self.path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => SessionArchiveError::MissingPinnedArchive,
            _ => SessionArchiveError::ArchiveIo(error),
        })?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_ARCHIVE_BYTES as u64 {
            return Err(SessionArchiveError::Invalid("unsafe external archive file"));
        }
        let file = File::open(&self.path)?;
        let opened = file.metadata()?;
        if !opened.is_file() || opened.len() > MAX_ARCHIVE_BYTES as u64 {
            return Err(SessionArchiveError::Invalid("unsafe external archive file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_ARCHIVE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(SessionArchiveError::Invalid("external archive too large"));
        }
        let after = fs::symlink_metadata(&self.path)
            .map_err(|_| SessionArchiveError::MissingPinnedArchive)?;
        if !after.file_type().is_file() || after.len() != bytes.len() as u64 {
            return Err(SessionArchiveError::Invalid(
                "external archive changed during scan",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (metadata.dev(), metadata.ino()) != (opened.dev(), opened.ino())
                || (opened.dev(), opened.ino()) != (after.dev(), after.ino())
            {
                return Err(SessionArchiveError::Invalid(
                    "external archive changed during scan",
                ));
            }
        }
        if archive_sha256(&bytes) != self.sha256 {
            return Err(SessionArchiveError::PinnedArchiveChanged);
        }
        Ok(bytes)
    }
}

fn archive_sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_pinned_archives(
    pins: &[PinnedExternalArchive],
) -> Result<Vec<Vec<u8>>, SessionArchiveError> {
    let mut known = BTreeMap::new();
    let mut documents = Vec::with_capacity(pins.len());
    for pin in pins {
        if known
            .insert(pin.path.clone(), pin.sha256.as_str())
            .is_some_and(|previous| previous != pin.sha256)
        {
            return Err(SessionArchiveError::Invalid(
                "conflicting external archive pins",
            ));
        }
        documents.push(pin.read_checked()?);
    }
    Ok(documents)
}

/// Reverify every pinned on-disk archive, then inventory all local branches and
/// those external archive roots. A missing, replaced or corrupt pin fails closed.
/// Unknown external archives, pending imports and concurrent writers still make
/// the result unsuitable for *any* physical GC, even if no candidates are seen.
pub fn inventory_pinned_memory(
    store: &TimelineStore,
    assets: &TimelineAssetStore,
    pins: &[PinnedExternalArchive],
) -> Result<TimelineAssetInventory, SessionArchiveError> {
    let documents = read_pinned_archives(pins)?;
    let borrowed = documents.iter().map(Vec::as_slice).collect::<Vec<_>>();
    inventory_memory(store, assets, &borrowed)
}

#[cfg(feature = "sqlite-system")]
pub fn inventory_pinned_sqlite(
    store: &SqliteTimelineRepository,
    assets: &TimelineAssetStore,
    pins: &[PinnedExternalArchive],
) -> Result<TimelineAssetInventory, SessionArchiveError> {
    let documents = read_pinned_archives(pins)?;
    let borrowed = documents.iter().map(Vec::as_slice).collect::<Vec<_>>();
    inventory_sqlite(store, assets, &borrowed)
}

/// Observe filesystem candidates after collecting every canonical branch and
/// the supplied, verified external archives. It is deliberately read-only:
/// neither this function nor its result proves that the list of external
/// archives is exhaustive or that no concurrent append/import is occurring.
pub fn inventory_memory(
    store: &TimelineStore,
    assets: &TimelineAssetStore,
    retained_archives: &[&[u8]],
) -> Result<TimelineAssetInventory, SessionArchiveError> {
    let roots = retention_manifest_memory(store, retained_archives)?;
    Ok(assets.inventory(&roots)?)
}

#[cfg(feature = "sqlite-system")]
pub fn inventory_sqlite(
    store: &SqliteTimelineRepository,
    assets: &TimelineAssetStore,
    retained_archives: &[&[u8]],
) -> Result<TimelineAssetInventory, SessionArchiveError> {
    let roots = retention_manifest_sqlite(store, retained_archives)?;
    Ok(assets.inventory(&roots)?)
}

/// Export a selected session, including its own branch facts and receipts (not
/// an expanded copy of ancestor worlds), and only assets referenced by events.
pub fn export_memory(
    store: &TimelineStore,
    session: SessionId,
    assets: &TimelineAssetStore,
) -> Result<Vec<u8>, SessionArchiveError> {
    let branches = topological(
        store
            .persisted_branches()
            .into_iter()
            .filter(|branch| branch.info.session_id == session)
            .collect(),
    )?;
    let root = branches
        .iter()
        .find(|branch| branch.info.parent.is_none())
        .ok_or(SessionArchiveError::Invalid("session root missing"))?
        .info
        .id;
    let timestamp = archive_timestamp(&branches)?;
    let branches = branches
        .into_iter()
        .map(ArchivedBranch::from_branch)
        .collect::<Vec<_>>();
    let references = references(&branches)?;
    let mut budget = 0_u64;
    let mut archive_assets = Vec::with_capacity(references.len());
    for reference in references.values() {
        if reference.size > MAX_SINGLE_BINARY {
            return Err(SessionArchiveError::Invalid("asset size"));
        }
        budget = budget
            .checked_add(reference.size)
            .filter(|size| *size <= MAX_TOTAL_BINARY)
            .ok_or(SessionArchiveError::Invalid("asset budget"))?;
        let bytes = assets.read_for_archive(reference)?;
        archive_assets.push(ArchivedAsset {
            reference: reference.clone(),
            data_base64: STANDARD.encode(bytes),
        });
    }
    let envelope = Envelope {
        format: FORMAT.into(),
        version: VERSION,
        session_id: session,
        root,
        created_at_utc: timestamp,
        branches,
        assets: archive_assets,
        provenance: store.session_provenance(session).cloned(),
    };
    validate_envelope(&envelope)?;
    let bytes = serde_json::to_vec(&envelope)
        .map_err(|error| SessionArchiveError::Encoding(error.to_string()))?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(SessionArchiveError::Invalid("archive byte limit"));
    }
    Ok(bytes)
}

fn prepare_import(bytes: &[u8]) -> Result<PreparedImport, SessionArchiveError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(SessionArchiveError::Invalid("archive byte limit"));
    }
    let archive = serde_json::from_slice::<Envelope>(bytes)
        .map_err(|_| SessionArchiveError::Invalid("archive JSON or unknown field"))?;
    let (branches, assets) = validate_envelope(&archive)?;
    Ok((archive.session_id, branches, assets, archive.provenance))
}

fn import_assets(
    binaries: Vec<(TimelineAssetRef, Vec<u8>)>,
    assets: &TimelineAssetStore,
) -> Result<(), SessionArchiveError> {
    for (expected, bytes) in binaries {
        let actual = assets.import(&expected.media_type, Cursor::new(bytes))?;
        if actual != expected {
            return Err(SessionArchiveError::Invalid("asset identity changed"));
        }
    }
    Ok(())
}

/// Reject all existing session identities instead of overwriting them. Assets
/// may remain as unreachable, content-addressed blobs if the Timeline commit
/// subsequently fails; canonical partial sessions are never published.
pub fn import_memory(
    store: &mut TimelineStore,
    bytes: &[u8],
    assets: &TimelineAssetStore,
) -> Result<SessionId, SessionArchiveError> {
    let (session, branches, binaries, provenance) = prepare_import(bytes)?;
    if store
        .persisted_branches()
        .iter()
        .any(|branch| branch.info.session_id == session)
    {
        return Err(SessionArchiveError::Conflict);
    }
    import_assets(binaries, assets)?;
    store.install_archive_branches_with_provenance(branches, provenance)?;
    Ok(session)
}

#[cfg(feature = "sqlite-system")]
pub fn export_sqlite(
    store: &SqliteTimelineRepository,
    session: SessionId,
    assets: &TimelineAssetStore,
) -> Result<Vec<u8>, SessionArchiveError> {
    export_memory(&store.archive_snapshot()?, session, assets)
}

#[cfg(feature = "sqlite-system")]
pub fn import_sqlite(
    store: &mut SqliteTimelineRepository,
    bytes: &[u8],
    assets: &TimelineAssetStore,
) -> Result<SessionId, SessionArchiveError> {
    let (session, branches, binaries, provenance) = prepare_import(bytes)?;
    if store
        .archive_snapshot()?
        .persisted_branches()
        .iter()
        .any(|branch| branch.info.session_id == session)
    {
        return Err(SessionArchiveError::Conflict);
    }
    import_assets(binaries, assets)?;
    store.archive_install_with_provenance(branches, provenance)?;
    Ok(session)
}
