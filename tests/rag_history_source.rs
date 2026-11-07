#[cfg(feature = "sqlite-system")]
use neuromita::knowledge::history_retrieval::search_archived_history_fts;
use neuromita::{
    character::CharacterId,
    knowledge::{
        history_generation::{HistoryEvidenceError, render_archived_history_evidence},
        history_retrieval::{
            HistorySearchConfig, HistorySearchError, HistorySearchStatus, search_archived_history,
            search_history, search_history_fts,
        },
        history_window::HistoryWindow,
        retrieval_fusion::{RetrievalMethod, RetrievalSource},
    },
    timeline::{
        EventId,
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore},
        view::TimelineMessageQueries,
    },
};
use serde_json::{Value, json};

fn append(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    kind: EventKind,
    observers: &[CharacterId],
    content: Value,
) -> EventId {
    let mut event = EventDraft::new(kind);
    event.actor = Some("player".into());
    event.observers.extend(observers.iter().copied());
    if !content.is_null() {
        event.payload.insert("content".into(), content);
    }
    let id = event.id;
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(
        branch,
        head,
        &format!("history-test:{}", id.0),
        json!({}),
        vec![event],
    )
    .unwrap();
    id
}

fn update_message(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    character: CharacterId,
    target: EventId,
    action: EventKind,
    text: &str,
) {
    let mut event = EventDraft::new(action);
    event.observers.insert(character);
    if action == EventKind::MessageRevised {
        event
            .payload
            .insert("event_id".into(), json!(target.0.to_string()));
        event
            .payload
            .insert("message".into(), json!({"content":text}));
    } else {
        event
            .payload
            .insert("event_ids".into(), json!([target.0.to_string()]));
    }
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(
        branch,
        head,
        &format!("history-action:{}", event.id.0),
        json!({}),
        vec![event],
    )
    .unwrap();
}

fn search(
    repo: &dyn TimelineRepository,
    branch: BranchId,
    character: CharacterId,
    query: &str,
) -> neuromita::knowledge::history_retrieval::HistorySearchResult {
    let view = TimelineMessageQueries::capture(repo, branch, character, None).unwrap();
    search_history(character, &view, query, HistorySearchConfig::default()).unwrap()
}

#[test]
fn history_source_has_real_provenance_and_distinct_zero_result_states() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let empty = search(&repo, branch, CharacterId::Crazy, "green tea");
    assert_eq!(empty.status, HistorySearchStatus::EmptyCorpus);
    let first = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Green tea is in the kitchen"),
    );
    append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("New painting in the studio"),
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Crazy, "absent").status,
        HistorySearchStatus::NoMatches
    );
    let matches = search(&repo, branch, CharacterId::Crazy, "green tea");
    assert_eq!(matches.status, HistorySearchStatus::Selected);
    assert_eq!(matches.hits.len(), 1);
    let hit = &matches.hits[0];
    assert_eq!(hit.key.id, first.0.to_string());
    assert_eq!(hit.key.source, RetrievalSource::History);
    assert_eq!(hit.key.character, CharacterId::Crazy);
    assert_eq!(hit.metadata["role"], "user");
    assert_eq!(hit.metadata["branch_id"], branch.0.to_string());
    assert_eq!(hit.metadata["through"], "2");
    assert_eq!(hit.evidence[0].method, RetrievalMethod::Keyword);
    assert_eq!(hit.evidence[0].raw_score, Some(2.0));
}

#[test]
fn history_never_crosses_witness_or_message_visibility_boundaries() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let shared = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy, CharacterId::Kind],
        json!("Shared green tea"),
    );
    let private = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Private green tea"),
    );
    let crazy = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        search_history(
            CharacterId::Kind,
            &crazy,
            "tea",
            HistorySearchConfig::default()
        )
        .unwrap_err(),
        HistorySearchError::WrongCharacter
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Kind, "tea").hits.len(),
        1
    );
    let before_hide = search(&repo, branch, CharacterId::Crazy, "private tea");
    assert_eq!(before_hide.hits[0].key.id, private.0.to_string());
    update_message(
        &mut repo,
        branch,
        CharacterId::Crazy,
        private,
        EventKind::MessageHidden,
        "",
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Crazy, "private").status,
        HistorySearchStatus::NoMatches
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Kind, "tea").hits[0]
            .key
            .id,
        shared.0.to_string()
    );
    let historical = search_history(
        CharacterId::Crazy,
        &crazy,
        "private",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(historical.hits[0].key.id, private.0.to_string());
}

#[test]
fn history_source_obeys_revision_reset_and_character_specific_current_projection() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let first = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy, CharacterId::Kind],
        json!("green tea"),
    );
    update_message(
        &mut repo,
        branch,
        CharacterId::Crazy,
        first,
        EventKind::MessageRevised,
        "blue coffee",
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Crazy, "green").status,
        HistorySearchStatus::NoMatches
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Crazy, "coffee").hits[0].content,
        "blue coffee"
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Kind, "green").hits[0].content,
        "green tea"
    );
    append(
        &mut repo,
        branch,
        EventKind::CharacterContextReset,
        &[CharacterId::Crazy],
        Value::Null,
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Crazy, "coffee").status,
        HistorySearchStatus::EmptyCorpus
    );
    assert_eq!(
        search(&repo, branch, CharacterId::Kind, "tea").hits.len(),
        1
    );
}

#[test]
fn history_rejects_invalid_queries_and_bounds_without_reading_other_sources() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let view = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    for query in ["", " ", "a", "?", &"z".repeat(2049)] {
        assert!(matches!(
            search_history(
                CharacterId::Crazy,
                &view,
                query,
                HistorySearchConfig::default()
            ),
            Err(HistorySearchError::Configuration(_))
        ));
    }
    assert!(matches!(
        search_history(
            CharacterId::Crazy,
            &view,
            "tea",
            HistorySearchConfig {
                limit: 0,
                max_content_chars: 10
            }
        ),
        Err(HistorySearchError::Configuration(_))
    ));
}

#[cfg(feature = "sqlite-system")]
#[test]
fn history_source_reopens_with_same_provenance_and_no_derived_index() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let path = std::env::temp_dir().join(format!("rag-history-{}.db", uuid::Uuid::new_v4()));
    let branch = BranchId::new();
    let source;
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        source = append(
            &mut repo,
            branch,
            EventKind::PlayerMessage,
            &[CharacterId::Crazy],
            json!("Jasmine tea"),
        );
    }
    {
        let repo = SqliteTimelineRepository::open(&path).unwrap();
        let result = search(&repo, branch, CharacterId::Crazy, "jasmine");
        assert_eq!(result.hits[0].key.id, source.0.to_string());
        assert_eq!(result.hits[0].metadata["through"], "1");
    }
    std::fs::remove_file(&path).unwrap();
}

#[cfg(feature = "sqlite-system")]
#[test]
fn fts5_source_tracks_authorized_timeline_revision_and_provenance() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let empty = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    let result = search_history_fts(
        CharacterId::Crazy,
        &empty,
        "jasmine",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(result.status, HistorySearchStatus::EmptyCorpus);
    let source = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Jasmine jasmine tea in the garden"),
    );
    append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Small flower studio"),
    );
    let crazy = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    let matches = search_history_fts(
        CharacterId::Crazy,
        &crazy,
        "jasmine",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(matches.status, HistorySearchStatus::Selected);
    assert_eq!(matches.hits.len(), 1);
    assert_eq!(matches.hits[0].key.id, source.0.to_string());
    assert_eq!(matches.hits[0].evidence[0].method, RetrievalMethod::Fts);
    assert_eq!(matches.hits[0].metadata["branch_id"], branch.0.to_string());
    assert!(matches.hits[0].evidence[0].raw_score.unwrap().is_finite());
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &crazy,
            "notfound",
            HistorySearchConfig::default()
        )
        .unwrap()
        .status,
        HistorySearchStatus::NoMatches
    );
    assert_eq!(
        search_history_fts(
            CharacterId::Kind,
            &crazy,
            "jasmine",
            HistorySearchConfig::default()
        )
        .unwrap_err(),
        HistorySearchError::WrongCharacter
    );

    update_message(
        &mut repo,
        branch,
        CharacterId::Crazy,
        source,
        EventKind::MessageRevised,
        "Garden blue iris",
    );
    let current = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &current,
            "jasmine",
            HistorySearchConfig::default()
        )
        .unwrap()
        .status,
        HistorySearchStatus::NoMatches
    );
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &current,
            "iris",
            HistorySearchConfig::default()
        )
        .unwrap()
        .hits[0]
            .key
            .id,
        source.0.to_string()
    );
    update_message(
        &mut repo,
        branch,
        CharacterId::Crazy,
        source,
        EventKind::MessageHidden,
        "",
    );
    let hidden = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &hidden,
            "iris",
            HistorySearchConfig::default()
        )
        .unwrap()
        .status,
        HistorySearchStatus::NoMatches
    );
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &crazy,
            "jasmine",
            HistorySearchConfig::default()
        )
        .unwrap()
        .hits[0]
            .key
            .id,
        source.0.to_string()
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn fts5_quoted_input_is_data_not_query_syntax_and_character_scopes_never_merge() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Kind],
        json!("Privately held orange tea"),
    );
    let shared = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy, CharacterId::Kind],
        json!("Зелёный чай на кухне"),
    );
    let crazy = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    let kind = TimelineMessageQueries::capture(&repo, branch, CharacterId::Kind, None).unwrap();
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &crazy,
            "orange",
            HistorySearchConfig::default()
        )
        .unwrap()
        .status,
        HistorySearchStatus::NoMatches
    );
    assert_eq!(
        search_history_fts(
            CharacterId::Kind,
            &kind,
            "orange",
            HistorySearchConfig::default()
        )
        .unwrap()
        .hits
        .len(),
        1
    );
    for query in [
        "зелёный",
        "чай",
        "\"чай\" OR something",
        "чай'; DROP TABLE history;--",
    ] {
        let selected = search_history_fts(
            CharacterId::Crazy,
            &crazy,
            query,
            HistorySearchConfig::default(),
        )
        .unwrap();
        assert_eq!(selected.status, HistorySearchStatus::Selected);
        assert_eq!(selected.hits[0].key.id, shared.0.to_string());
    }
    append(
        &mut repo,
        branch,
        EventKind::CharacterContextReset,
        &[CharacterId::Crazy],
        Value::Null,
    );
    let reset = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &reset,
            "чай",
            HistorySearchConfig::default()
        )
        .unwrap()
        .status,
        HistorySearchStatus::EmptyCorpus
    );
    assert_eq!(
        search_history_fts(
            CharacterId::Kind,
            &TimelineMessageQueries::capture(&repo, branch, CharacterId::Kind, None).unwrap(),
            "чай",
            HistorySearchConfig::default()
        )
        .unwrap()
        .hits
        .len(),
        1
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn fts5_rejects_invalid_query_and_overlarge_corpus_before_building_index() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let empty = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    for query in ["", "a", " ", "?", &"z".repeat(2049)] {
        assert!(matches!(
            search_history_fts(
                CharacterId::Crazy,
                &empty,
                query,
                HistorySearchConfig::default()
            ),
            Err(HistorySearchError::Configuration(_))
        ));
    }
    for number in 0..1025 {
        append(
            &mut repo,
            branch,
            EventKind::PlayerMessage,
            &[CharacterId::Crazy],
            json!(format!("history tea {number}")),
        );
    }
    let view = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &view,
            "tea",
            HistorySearchConfig::default()
        )
        .unwrap_err(),
        HistorySearchError::CorpusLimit {
            found: 1025,
            maximum: 1024
        }
    );
}

#[cfg(not(feature = "sqlite-system"))]
#[test]
fn explicit_fts_mode_rejects_missing_backend_without_lexical_substitution() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("jasmine tea"),
    );
    let view = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        search_history_fts(
            CharacterId::Crazy,
            &view,
            "jasmine",
            HistorySearchConfig::default(),
        )
        .unwrap_err(),
        HistorySearchError::FtsUnavailable
    );
}

#[test]
fn archived_history_is_absent_from_active_window_and_retrievable_once() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let old = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Old jasmine recipe hidden from active context"),
    );
    let recent = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Current schedule"),
    );
    let view = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    let window = HistoryWindow::capture(&view, None, 1);
    assert_eq!(window.archived_count(), 1);
    assert!(window.is_archived(old));
    assert!(window.is_active(recent));
    let result = search_archived_history(
        CharacterId::Crazy,
        &view,
        &window,
        "jasmine recipe",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(result.status, HistorySearchStatus::Selected);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key.id, old.0.to_string());
    let evidence =
        render_archived_history_evidence(CharacterId::Crazy, &view, &window, &result, 4, 1200)
            .unwrap()
            .unwrap();
    assert!(evidence.contains("Old jasmine recipe"));
    let active_query = search_archived_history(
        CharacterId::Crazy,
        &view,
        &window,
        "schedule",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(active_query.status, HistorySearchStatus::NoMatches);

    let old_focus = HistoryWindow::capture(&view, Some(old), 1);
    assert!(old_focus.is_active(old));
    assert_eq!(old_focus.archived_count(), 0);
    let focused = search_archived_history(
        CharacterId::Crazy,
        &view,
        &old_focus,
        "jasmine",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(focused.status, HistorySearchStatus::EmptyCorpus);
    assert_eq!(
        render_archived_history_evidence(CharacterId::Crazy, &view, &old_focus, &result, 4, 1200),
        Err(HistoryEvidenceError::InvalidEvidence)
    );
}

#[test]
fn archived_history_scope_is_invalidated_by_revision_and_privacy() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let old = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Private tea archive"),
    );
    append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Current session"),
    );
    let before = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    let window = HistoryWindow::capture(&before, None, 1);
    let other = TimelineMessageQueries::capture(&repo, branch, CharacterId::Kind, None).unwrap();
    assert!(!window.matches(&other));
    assert_eq!(
        search_archived_history(
            CharacterId::Kind,
            &other,
            &window,
            "tea",
            HistorySearchConfig::default()
        )
        .unwrap_err(),
        HistorySearchError::StaleWindow
    );
    update_message(
        &mut repo,
        branch,
        CharacterId::Crazy,
        old,
        EventKind::MessageRevised,
        "Updated private tea archive",
    );
    let after = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    assert!(!window.matches(&after));
    assert_eq!(
        search_archived_history(
            CharacterId::Crazy,
            &after,
            &window,
            "tea",
            HistorySearchConfig::default()
        )
        .unwrap_err(),
        HistorySearchError::StaleWindow
    );
    let current = HistoryWindow::capture(&after, None, 1);
    let result = search_archived_history(
        CharacterId::Crazy,
        &after,
        &current,
        "Updated private",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key.id, old.0.to_string());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn archived_history_fts_indexes_only_messages_outside_active_context() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let old = append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Cinnamon archive menu"),
    );
    append(
        &mut repo,
        branch,
        EventKind::PlayerMessage,
        &[CharacterId::Crazy],
        json!("Cinnamon today fresh"),
    );
    let view = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    let window = HistoryWindow::capture(&view, None, 1);
    let result = search_archived_history_fts(
        CharacterId::Crazy,
        &view,
        &window,
        "cinnamon",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key.id, old.0.to_string());
}
