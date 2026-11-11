use super::{InspectedPythonArchive, PythonArchiveError, PythonIdentityPlan};
use crate::identity::CharacterId;
use crate::timeline::domain::{
    ExternalEventProvenance, ExternalSessionSource, SessionId, SessionProvenance, TimelineStore,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[cfg(feature = "sqlite-system")]
use crate::timeline::storage::SqliteTimelineRepository;

/// A detached, explicitly authorized mapping. This does not reserve identities
/// or write to the destination; a future importer must repeat collision checks
/// inside the same transaction as canonical publication.
#[derive(Debug)]
pub struct BoundPythonArchive<'a> {
    pub source: &'a InspectedPythonArchive,
    pub observers: BTreeMap<String, CharacterId>,
}

impl<'a> BoundPythonArchive<'a> {
    pub fn identities(&self) -> &PythonIdentityPlan {
        &self.source.identities
    }

    pub fn native_session(&self) -> SessionId {
        SessionId(self.source.identities.session.candidate_uuid)
    }

    /// Detached session-owned metadata for the future atomic native import.
    /// Event payloads are intentionally not duplicated here.
    pub fn provenance(&self) -> SessionProvenance {
        let identities = &self.source.identities;
        SessionProvenance {
            source: ExternalSessionSource::PythonSessionArchive,
            external_session_id: identities.session.source.clone(),
            external_branches: identities
                .branches
                .iter()
                .map(|id| (id.candidate_uuid, id.source.clone()))
                .collect(),
            external_events: identities
                .events
                .iter()
                .map(|id| {
                    let time = &self.source.event_times[&id.source];
                    (
                        id.candidate_uuid,
                        ExternalEventProvenance {
                            external_event_id: id.source.clone(),
                            occurred_at_iso8601: time.source_iso8601.clone(),
                            occurred_at_unix_us: time.unix_microseconds,
                        },
                    )
                })
                .collect(),
            observer_bindings: self.observers.clone(),
        }
    }

    /// Stage the verified Python binary closure in the canonical content-addressed
    /// AssetStore. Files are deduplicated; no session is published. Failed later
    /// Timeline transactions can leave only harmless unreferenced assets.
    pub fn stage_assets(
        &self,
        destination: &crate::timeline::storage::assets::TimelineAssetStore,
    ) -> Result<BTreeMap<String, crate::timeline::domain::TimelineAssetRef>, PythonArchiveError>
    {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use std::io::Cursor;
        let mut uploaded = BTreeMap::new();
        for asset in &self.source.envelope.assets {
            let data = STANDARD
                .decode(&asset.base64)
                .map_err(|_| PythonArchiveError::Invalid("verified asset could not be decoded"))?;
            let source_size = data.len() as u64;
            let reference = destination
                .import(&asset.media_type, Cursor::new(data))
                .map_err(|_| PythonArchiveError::Invalid("asset staging failed"))?;
            if asset.asset_id.strip_prefix("sha256:") != Some(reference.digest.as_str())
                || asset.media_type != reference.media_type
                || reference.size != source_size
            {
                return Err(PythonArchiveError::Invalid(
                    "staged asset identity mismatch",
                ));
            }
            if uploaded.insert(asset.asset_id.clone(), reference).is_some() {
                return Err(PythonArchiveError::Invalid("duplicate staged binary"));
            }
        }
        Ok(uploaded)
    }
}

impl InspectedPythonArchive {
    /// Bind every Python observer explicitly to one Rust character. No guessed
    /// aliases, missing observers, or two source characters sharing one target.
    pub fn bind_memory<'a>(
        &'a self,
        destination: &TimelineStore,
        observers: &BTreeMap<String, CharacterId>,
    ) -> Result<BoundPythonArchive<'a>, PythonArchiveError> {
        let existing = destination.persisted_branches();
        bind(
            self,
            observers,
            existing.iter().map(|branch| {
                (
                    branch.info.session_id.0,
                    branch.info.id.0,
                    branch
                        .events
                        .iter()
                        .map(|fact| fact.draft.id.0)
                        .collect::<Vec<_>>(),
                )
            }),
        )
    }

    /// Reopen/snapshot validation is read-only. The final SQLite transaction
    /// must revalidate the bound identities to avoid a time-of-check race.
    #[cfg(feature = "sqlite-system")]
    pub fn bind_sqlite<'a>(
        &'a self,
        destination: &SqliteTimelineRepository,
        observers: &BTreeMap<String, CharacterId>,
    ) -> Result<BoundPythonArchive<'a>, PythonArchiveError> {
        let snapshot = destination
            .archive_snapshot()
            .map_err(|_| PythonArchiveError::Invalid("destination SQLite snapshot unavailable"))?;
        self.bind_memory(&snapshot, observers)
    }
}

fn bind<'a>(
    source: &'a InspectedPythonArchive,
    observers: &BTreeMap<String, CharacterId>,
    occupied: impl IntoIterator<Item = (uuid::Uuid, uuid::Uuid, Vec<uuid::Uuid>)>,
) -> Result<BoundPythonArchive<'a>, PythonArchiveError> {
    let declared = source
        .identities
        .observer_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if observers.keys().cloned().collect::<BTreeSet<_>>() != declared {
        return Err(PythonArchiveError::Invalid(
            "Python observers require an exact explicit binding",
        ));
    }
    if observers.values().copied().collect::<HashSet<_>>().len() != observers.len() {
        return Err(PythonArchiveError::Invalid(
            "multiple Python observers mapped to one character",
        ));
    }
    for event in &source.envelope.events {
        if matches!(
            event.kind.as_str(),
            "CharacterMessage" | "CharacterObservation" | "CharacterJoined" | "CharacterLeft"
        ) && !event
            .actor_id
            .as_ref()
            .is_some_and(|actor| observers.contains_key(actor))
        {
            return Err(PythonArchiveError::Invalid(
                "character event actor requires an explicit binding",
            ));
        }
        if event.kind == "PlayerMessage"
            && !matches!(event.actor_id.as_deref(), Some("Player" | "player"))
        {
            return Err(PythonArchiveError::Invalid(
                "Python player actor has unsupported identity",
            ));
        }
    }
    let mut all = Vec::new();
    for (session, branch, events) in occupied {
        all.push(session);
        all.push(branch);
        all.extend(events);
    }
    source.identities.ensure_unoccupied(all)?;
    Ok(BoundPythonArchive {
        source,
        observers: observers.clone(),
    })
}
