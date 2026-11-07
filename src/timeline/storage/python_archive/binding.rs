use super::{InspectedPythonArchive, PythonArchiveError, PythonIdentityPlan};
use crate::identity::CharacterId;
use crate::timeline::domain::{SessionId, TimelineStore};
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
