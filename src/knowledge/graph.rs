//! Queryable, source-versioned graph projection over canonical Timeline memories.
//! Extraction is a proposed MemoryCommand, never a second mutable graph store.

use std::{collections::BTreeSet, future::Future, pin::Pin};

use crate::memory::{
    CharacterMemories, MAX_SOURCE_RELATIONS, MemoryCommand, MemoryKind, MemoryRecord,
    MemoryRelation, MemoryState, ProvenancedRelation,
};

use super::memory_context::tokens;

pub type GraphExtractionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<MemoryRelation>, GraphError>> + Send + 'a>>;

pub trait GraphExtractor: Send + Sync {
    fn extract<'a>(&'a self, source: &'a MemoryRecord) -> GraphExtractionFuture<'a>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    InvalidSource,
    InvalidRelations,
    InvalidQuery,
    Provider(String),
}

/// Captured together with the character-specific Timeline memory snapshot.
/// Callers must apply the proposed command against that original snapshot,
/// and the character domain validates the source revision a second time.
#[derive(Debug, Clone)]
pub struct GraphExtractionSource {
    pub record: MemoryRecord,
    pub revision: u64,
}

impl GraphExtractionSource {
    pub fn new(record: MemoryRecord, revision: u64) -> Result<Self, GraphError> {
        if revision == 0
            || record.state != MemoryState::Active
            || !matches!(record.kind, MemoryKind::Fact | MemoryKind::RagOnly)
            || !record.is_valid()
        {
            return Err(GraphError::InvalidSource);
        }
        Ok(Self { record, revision })
    }
}

/// A typed, bounded extractor result; does not write until CharacterHandler
/// resolves and atomically appends it through the ordinary memory boundary.
pub async fn propose_extraction(
    extractor: &dyn GraphExtractor,
    source: &GraphExtractionSource,
) -> Result<MemoryCommand, GraphError> {
    propose_relations(source, extractor.extract(&source.record).await?)
}

/// Decode Python structured-response triples from the same generation turn.
/// The caller supplies the captured memory source; no unrelated event or
/// unsourced graph relation is invented when the response lacks one.
pub fn propose_inline_relations(
    source: &GraphExtractionSource,
    triples: &[String],
) -> Result<MemoryCommand, GraphError> {
    if triples.len() > MAX_SOURCE_RELATIONS {
        return Err(GraphError::InvalidRelations);
    }
    let mut relations = Vec::with_capacity(triples.len());
    for triple in triples {
        let mut fields = triple.split('|');
        let (Some(subject), Some(predicate), Some(object), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(GraphError::InvalidRelations);
        };
        relations.push(MemoryRelation {
            subject: subject.trim().to_owned(),
            predicate: predicate.trim().to_owned(),
            object: object.trim().to_owned(),
        });
    }
    propose_relations(source, relations)
}

fn propose_relations(
    source: &GraphExtractionSource,
    relations: Vec<MemoryRelation>,
) -> Result<MemoryCommand, GraphError> {
    if source.revision == 0
        || source.record.state != MemoryState::Active
        || !matches!(source.record.kind, MemoryKind::Fact | MemoryKind::RagOnly)
        || !source.record.is_valid()
        || relations.len() > MAX_SOURCE_RELATIONS
        || relations.iter().any(|relation| !relation.is_valid())
        || relations.iter().collect::<BTreeSet<_>>().len() != relations.len()
    {
        return Err(GraphError::InvalidRelations);
    }
    Ok(MemoryCommand::ReplaceExtractedRelations {
        source: source.record.id,
        expected_revision: source.revision,
        relations,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphHit {
    pub provenance: ProvenancedRelation,
    pub matching_terms: usize,
}

/// Deterministic lexical graph baseline. The caller supplies one captured
/// character projection; expired, deleted and reprocessed sources are excluded.
pub fn query_extracted_graph(
    snapshot: &CharacterMemories,
    query: &str,
    as_of_unix_ms: u64,
    limit: usize,
) -> Result<Vec<GraphHit>, GraphError> {
    let terms = tokens(query);
    if terms.is_empty() || !(1..=64).contains(&limit) {
        return Err(GraphError::InvalidQuery);
    }
    let mut hits = snapshot
        .extracted_relations_at(as_of_unix_ms)
        .into_iter()
        .filter_map(|relation| {
            let combined = format!(
                "{} {} {}",
                relation.relation.subject, relation.relation.predicate, relation.relation.object
            );
            let matching_terms = terms.intersection(&tokens(&combined)).count();
            (matching_terms > 0).then_some(GraphHit {
                provenance: relation,
                matching_terms,
            })
        })
        .collect::<Vec<_>>();
    hits.sort_by(|a, b| {
        b.matching_terms
            .cmp(&a.matching_terms)
            .then_with(|| a.provenance.source.cmp(&b.provenance.source))
            .then_with(|| a.provenance.relation.cmp(&b.provenance.relation))
    });
    hits.truncate(limit);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        character::{CharacterId, CharacterRegistry},
        memory::{MemoryPriority, MemoryState},
        timeline::SessionTimeline,
    };
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    struct Extractor(Vec<MemoryRelation>);

    impl GraphExtractor for Extractor {
        fn extract<'a>(&'a self, _: &'a crate::memory::MemoryRecord) -> GraphExtractionFuture<'a> {
            Box::pin(async { Ok(self.0.clone()) })
        }
    }

    fn relation(object: &str) -> MemoryRelation {
        MemoryRelation {
            subject: "Mita".into(),
            predicate: "likes".into(),
            object: object.into(),
        }
    }

    #[tokio::test]
    async fn inline_structured_triples_are_validated_and_committed_with_source_provenance() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let mita = registry.get(CharacterId::Crazy).unwrap();
        let added = mita
            .edit_memories(
                mita.memories().await.unwrap(),
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Mita likes tea".into(),
                }],
            )
            .await
            .unwrap();
        let record = added.snapshot.records()[0].clone();
        let source = GraphExtractionSource::new(
            record.clone(),
            added.snapshot.source_revision(record.id).unwrap(),
        )
        .unwrap();

        let correct = [" Mita | likes | tea ".to_owned()];
        let proposed = propose_inline_relations(&source, &correct).unwrap();
        let accepted = mita
            .edit_memories(added.snapshot, vec![proposed.clone()])
            .await
            .unwrap();
        assert_eq!(accepted.snapshot.extracted_relations().len(), 1);
        assert_eq!(accepted.snapshot.extracted_relations()[0].source, record.id);
        assert_eq!(
            accepted.snapshot.extracted_relations()[0].source_revision,
            source.revision
        );
        assert_eq!(
            accepted.snapshot.extracted_relations()[0].relation,
            relation("tea")
        );
        let repeated = mita
            .edit_memories(accepted.snapshot, vec![proposed.clone()])
            .await
            .unwrap();
        assert!(repeated.applied.is_empty());
        for malformed in [
            vec!["Mita|likes|tea".into(), "missing|value".into()],
            vec!["Mita|likes|tea".into(), "Mita | likes | tea".into()],
            vec!["Mita|likes|tea|extra".into()],
            vec!["Mita||tea".into()],
            vec!["Mita|likes|".into()],
            vec!["Mita|likes|tea".into(); MAX_SOURCE_RELATIONS + 1],
        ] {
            assert!(matches!(
                propose_inline_relations(&source, &malformed),
                Err(GraphError::InvalidRelations)
            ));
        }
        assert!(
            registry
                .get(CharacterId::Kind)
                .unwrap()
                .edit_memories(
                    registry
                        .get(CharacterId::Kind)
                        .unwrap()
                        .memories()
                        .await
                        .unwrap(),
                    vec![proposed.clone()]
                )
                .await
                .is_err()
        );
        let updated = mita
            .edit_memories(
                repeated.snapshot,
                vec![MemoryCommand::Update {
                    id: record.id,
                    priority: None,
                    content: "Mita likes coffee".into(),
                }],
            )
            .await
            .unwrap();
        assert!(
            mita.edit_memories(updated.snapshot, vec![proposed])
                .await
                .is_err()
        );
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn graph_extraction_is_source_versioned_and_replays_without_resurrecting_stale_links() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let mita = registry.get(CharacterId::Crazy).unwrap();
        let kind = registry.get(CharacterId::Kind).unwrap();
        let added = mita
            .edit_memories(
                mita.memories().await.unwrap(),
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Mita likes tea".into(),
                }],
            )
            .await
            .unwrap();
        let id = added.snapshot.records()[0].id;
        let old_revision = added.snapshot.source_revision(id).unwrap();
        let source =
            GraphExtractionSource::new(added.snapshot.records()[0].clone(), old_revision).unwrap();
        let extractor = Extractor(vec![relation("tea")]);
        let old_command = propose_extraction(&extractor, &source).await.unwrap();
        assert!(
            kind.edit_memories(kind.memories().await.unwrap(), vec![old_command.clone()])
                .await
                .is_err()
        );
        let mut book = CharacterMemories::default();
        for change in &added.applied {
            book.apply_committed(change).unwrap();
        }
        let extracted = mita
            .edit_memories(added.snapshot, vec![old_command.clone()])
            .await
            .unwrap();
        assert_eq!(extracted.applied.len(), 1);
        assert_eq!(extracted.snapshot.relations(), &[relation("tea")]);
        for change in &extracted.applied {
            book.apply_committed(change).unwrap();
        }
        let found = query_extracted_graph(&book, "Mita tea", 0, 8).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provenance.source, id);
        assert_eq!(found[0].provenance.source_revision, old_revision);
        assert_eq!(found[0].matching_terms, 2);

        let repeated = mita
            .edit_memories(extracted.snapshot, vec![old_command.clone()])
            .await
            .unwrap();
        assert!(repeated.applied.is_empty());
        let updated = mita
            .edit_memories(
                repeated.snapshot,
                vec![MemoryCommand::Update {
                    id,
                    priority: None,
                    content: "Mita likes coffee".into(),
                }],
            )
            .await
            .unwrap();
        assert!(updated.snapshot.relations().is_empty());
        for change in &updated.applied {
            book.apply_committed(change).unwrap();
        }
        assert!(
            query_extracted_graph(&book, "tea", 0, 8)
                .unwrap()
                .is_empty()
        );
        assert!(updated.snapshot.source_revision(id).unwrap() > old_revision);
        assert!(
            mita.edit_memories(updated.snapshot.clone(), vec![old_command])
                .await
                .is_err()
        );

        let fresh_source = GraphExtractionSource::new(
            updated.snapshot.records()[0].clone(),
            updated.snapshot.source_revision(id).unwrap(),
        )
        .unwrap();
        let fresh_command = propose_extraction(&Extractor(vec![relation("coffee")]), &fresh_source)
            .await
            .unwrap();
        let reprocessed = mita
            .edit_memories(updated.snapshot, vec![fresh_command])
            .await
            .unwrap();
        for change in &reprocessed.applied {
            book.apply_committed(change).unwrap();
        }
        assert_eq!(
            query_extracted_graph(&book, "coffee", 0, 8).unwrap().len(),
            1
        );
        let deleted = mita
            .edit_memories(reprocessed.snapshot, vec![MemoryCommand::SoftDelete { id }])
            .await
            .unwrap();
        for change in &deleted.applied {
            book.apply_committed(change).unwrap();
        }
        assert_eq!(deleted.snapshot.records()[0].state, MemoryState::Deleted);
        assert!(deleted.snapshot.relations().is_empty());
        assert!(
            query_extracted_graph(&book, "coffee", 0, 8)
                .unwrap()
                .is_empty()
        );
        let restored = mita
            .edit_memories(deleted.snapshot, vec![MemoryCommand::Restore { id }])
            .await
            .unwrap();
        for change in &restored.applied {
            book.apply_committed(change).unwrap();
        }
        assert!(restored.snapshot.relations().is_empty());
        assert!(
            query_extracted_graph(&book, "coffee", 0, 8)
                .unwrap()
                .is_empty()
        );
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[test]
    fn expired_sources_are_hidden_even_before_expiry_maintenance() {
        let mut book = CharacterMemories::default();
        let id = crate::memory::MemoryId::new();
        let mut record =
            crate::memory::MemoryRecord::fact(id, MemoryPriority::Normal, "Mita likes tea");
        record.expires_at_unix_ms = Some(100);
        book.apply_committed(&crate::memory::MemoryChange::Upsert(record))
            .unwrap();
        let revision = book.source_revision(id).unwrap();
        book.apply_committed(&crate::memory::MemoryChange::ReplaceExtractedRelations(
            crate::memory::ExtractedRelations {
                source: id,
                source_revision: revision,
                relations: vec![relation("tea")],
            },
        ))
        .unwrap();
        assert_eq!(query_extracted_graph(&book, "tea", 99, 8).unwrap().len(), 1);
        assert!(
            query_extracted_graph(&book, "tea", 100, 8)
                .unwrap()
                .is_empty()
        );
        assert!(book.relations_at(100).is_empty());
    }

    #[test]
    fn physical_purge_and_identity_restore_never_reuse_source_revision() {
        let id = crate::memory::MemoryId::new();
        let record =
            crate::memory::MemoryRecord::fact(id, MemoryPriority::Normal, "Mita likes tea");
        let mut book = CharacterMemories::default();
        book.apply_committed(&crate::memory::MemoryChange::Upsert(record.clone()))
            .unwrap();
        let stale_revision = book.source_revision(id).unwrap();
        book.apply_committed(&crate::memory::MemoryChange::Delete(id))
            .unwrap();
        assert_eq!(book.source_revision(id), None);
        book.apply_committed(&crate::memory::MemoryChange::Upsert(record))
            .unwrap();
        assert!(book.source_revision(id).unwrap() > stale_revision);
        assert!(
            book.resolve(&[MemoryCommand::ReplaceExtractedRelations {
                source: id,
                expected_revision: stale_revision,
                relations: vec![relation("tea")],
            }])
            .is_err()
        );
    }

    #[tokio::test]
    async fn extractor_rejects_invalid_or_duplicate_relations_without_mutation() {
        let record = crate::memory::MemoryRecord::fact(
            crate::memory::MemoryId::new(),
            MemoryPriority::Normal,
            "Mita likes tea",
        );
        let source = GraphExtractionSource::new(record, 1).unwrap();
        let duplicates = Extractor(vec![relation("tea"), relation("tea")]);
        assert!(matches!(
            propose_extraction(&duplicates, &source).await,
            Err(GraphError::InvalidRelations)
        ));
        let blank = Extractor(vec![relation(" ")]);
        assert!(matches!(
            propose_extraction(&blank, &source).await,
            Err(GraphError::InvalidRelations)
        ));
        assert!(matches!(
            query_extracted_graph(&CharacterMemories::default(), "", 0, 8),
            Err(GraphError::InvalidQuery)
        ));
    }
}
