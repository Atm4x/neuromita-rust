//! Ephemeral FTS5 ranking of caller-owned, already authorized source snapshots.
//! Input indices are ephemeral; no mutable search corpus or derived index survives this call.

#[cfg(feature = "sqlite-system")]
use super::memory_context::tokens;

pub(crate) const MAX_SCOPED_FTS_ROWS: usize = 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ScopedFtsError {
    #[error("SQLite FTS5 backend is not available")]
    Unavailable,
    #[error("derived FTS corpus exceeds bounded capacity of {MAX_SCOPED_FTS_ROWS} rows")]
    CorpusLimit,
    #[cfg(feature = "sqlite-system")]
    #[error("derived FTS retrieval failed: {0}")]
    Failure(&'static str),
}

pub(crate) fn rank(query: &str, documents: &[&str]) -> Result<Vec<(usize, f64)>, ScopedFtsError> {
    if documents.len() > MAX_SCOPED_FTS_ROWS {
        return Err(ScopedFtsError::CorpusLimit);
    }
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    #[cfg(not(feature = "sqlite-system"))]
    {
        let _ = query;
        Err(ScopedFtsError::Unavailable)
    }
    #[cfg(feature = "sqlite-system")]
    {
        rank_sqlite(query, documents)
    }
}

#[cfg(feature = "sqlite-system")]
fn rank_sqlite(query: &str, documents: &[&str]) -> Result<Vec<(usize, f64)>, ScopedFtsError> {
    use rusqlite::{Connection, params};

    let mut connection = Connection::open_in_memory()
        .map_err(|_| ScopedFtsError::Failure("cannot open derived index"))?;
    connection
        .execute_batch("CREATE VIRTUAL TABLE scoped_fts USING fts5(content, tokenize='unicode61');")
        .map_err(|_| ScopedFtsError::Unavailable)?;
    {
        let transaction = connection
            .transaction()
            .map_err(|_| ScopedFtsError::Failure("cannot start derived indexing"))?;
        {
            let mut insert = transaction
                .prepare("INSERT INTO scoped_fts(rowid, content) VALUES (?1, ?2)")
                .map_err(|_| ScopedFtsError::Failure("cannot prepare derived indexing"))?;
            for (index, content) in documents.iter().enumerate() {
                insert
                    .execute(params![index as i64 + 1, content])
                    .map_err(|_| ScopedFtsError::Failure("cannot build derived index"))?;
            }
        }
        transaction
            .commit()
            .map_err(|_| ScopedFtsError::Failure("cannot finalize derived index"))?;
    }
    let match_query = tokens(query)
        .into_iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    if match_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT rowid, bm25(scoped_fts) AS rank FROM scoped_fts WHERE scoped_fts MATCH ?1 ORDER BY rank, rowid LIMIT 1024")
        .map_err(|_| ScopedFtsError::Failure("cannot prepare FTS query"))?;
    let rows = statement
        .query_map([match_query], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(|_| ScopedFtsError::Failure("FTS query failed"))?;
    let mut ranked = Vec::new();
    for result in rows {
        let (row_id, bm25) = result.map_err(|_| ScopedFtsError::Failure("invalid FTS row"))?;
        let index = row_id
            .checked_sub(1)
            .and_then(|id| usize::try_from(id).ok())
            .ok_or(ScopedFtsError::Failure("invalid derived source identity"))?;
        if index >= documents.len() {
            return Err(ScopedFtsError::Failure(
                "out-of-range derived source identity",
            ));
        }
        if !bm25.is_finite() {
            return Err(ScopedFtsError::Failure("invalid FTS rank"));
        }
        ranked.push((index, bm25));
    }
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn unicode61_diacritic_ranking_is_not_keyword_token_overlap() {
        let ranked = rank("cafe", &["A quiet café", "A quiet garden"]).unwrap();
        assert_eq!(ranked.len(), 1);
        assert!(tokens("cafe").is_disjoint(&tokens("café")));
        assert_eq!(ranked[0].0, 0);
        assert!(ranked[0].1.is_finite());
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn user_supplied_fts_syntax_is_literal_input_not_a_query_operator() {
        let ranked = rank("cafe\" OR (", &["The cafe is open", "A quiet forest"]).unwrap();
        assert_eq!(
            ranked.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[cfg(not(feature = "sqlite-system"))]
    #[test]
    fn disabled_fts_fails_closed_on_nonempty_source_instead_of_keyword_fallback() {
        assert_eq!(
            rank("cafe", &["cafe is open"]),
            Err(ScopedFtsError::Unavailable)
        );
    }

    #[test]
    fn source_capacity_is_enforced_before_index_allocation() {
        assert_eq!(
            rank("cafe", &vec!["cafe"; MAX_SCOPED_FTS_ROWS + 1]),
            Err(ScopedFtsError::CorpusLimit)
        );
    }
}
