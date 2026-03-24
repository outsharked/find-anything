use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};

use find_common::api::FileKind;

use super::split_composite_path;
use super::{SQL_FTS_FILE_ID, SQL_FTS_FILENAME_ONLY, SQL_FTS_LINE_NUMBER};

/// Combined search filter: optional date range (mtime), optional kind allowlist,
/// optional path prefix, and optional filename-only restriction.
#[derive(Debug, Clone, Default)]
pub struct DateFilter {
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// Allowlist of file kinds. Empty = any kind.
    pub kinds: Vec<FileKind>,
    /// When true, restrict matches to line_number = 0 (filename-only search).
    pub filename_only: bool,
    /// When set, restrict results to files whose path equals this prefix or
    /// starts with `<prefix>/`.  Already normalised (no leading/trailing slashes).
    pub path_prefix: Option<String>,
}

impl DateFilter {
    pub fn is_active(&self) -> bool {
        self.from.is_some() || self.to.is_some() || !self.kinds.is_empty() || self.path_prefix.is_some()
    }
}

// ── ParamBinder ───────────────────────────────────────────────────────────────

/// Accumulates SQL parameters and auto-numbers their `?N` placeholders.
struct ParamBinder {
    params: Vec<Box<dyn rusqlite::ToSql>>,
}

impl ParamBinder {
    fn new() -> Self { Self { params: vec![] } }

    /// Append a value and return its `?N` placeholder string.
    fn push(&mut self, v: impl rusqlite::ToSql + 'static) -> String {
        self.params.push(Box::new(v));
        format!("?{}", self.params.len())
    }

    fn as_refs(&self) -> Vec<&dyn rusqlite::ToSql> {
        self.params.iter().map(|p| p.as_ref()).collect()
    }
}

// ── Search ────────────────────────────────────────────────────────────────────

pub struct CandidateRow {
    /// Full path, potentially composite ("archive.zip::member.txt").
    pub file_path: String,
    pub file_kind: FileKind,
    /// For archive members: the part after the first "::".
    /// For outer files: None.
    pub archive_path: Option<String>,
    pub line_number: usize,
    pub content: String,
    pub mtime: i64,
    pub size: Option<i64>,
    /// The file's row ID in the `files` table (used for duplicate lookup).
    pub file_id: i64,
}

/// Build an FTS5 match expression from a raw query string.
/// Returns None if the query produces no matchable terms.
pub(crate) fn build_fts_query(query: &str, phrase: bool) -> Option<String> {
    if phrase {
        if query.len() < 3 {
            return None;
        }
        Some(format!("\"{}\"", query.replace('"', "\"\"")))
    } else {
        let terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 3)
            .map(|w| w.to_string())
            .collect();
        if terms.is_empty() {
            return None;
        }
        Some(terms.join(" AND "))
    }
}

/// Count FTS5 matches, capped at `limit`.
/// When `date` is active or `filename_only` is set, adds JOINs and WHERE clauses.
pub fn fts_count(conn: &Connection, query: &str, limit: usize, phrase: bool, date: DateFilter) -> Result<usize> {
    let Some(fts_query) = build_fts_query(query, phrase) else {
        return Ok(0);
    };
    if !date.is_active() && !date.filename_only {
        // Fast path: pure FTS5, no ZIP reads, no JOINs.
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM (SELECT 1 FROM lines_fts WHERE lines_fts MATCH ?1 LIMIT ?2)",
            params![fts_query, limit as i64],
            |row| row.get(0),
        )?;
        return Ok(count as usize);
    }
    // Date/kind/filename filter active: need JOIN to files.
    let from = date.from.unwrap_or(i64::MIN);
    let to = date.to.unwrap_or(i64::MAX);
    let filename_clause = if date.filename_only { &format!("AND {SQL_FTS_FILENAME_ONLY}") } else { "" };

    let mut p = ParamBinder::new();
    let fts_ph   = p.push(fts_query);
    let limit_ph = p.push(limit as i64);
    let from_ph  = p.push(from);
    let to_ph    = p.push(to);
    let kind_clause = if date.kinds.is_empty() {
        String::new()
    } else {
        let phs = date.kinds.iter().map(|k| p.push(k.to_string())).collect::<Vec<_>>().join(", ");
        format!("AND f.kind IN ({phs})")
    };

    let sql = format!(
        "SELECT count(*) FROM (
             SELECT 1
             FROM lines_fts
             JOIN files f ON f.id = {SQL_FTS_FILE_ID}
             WHERE lines_fts MATCH {fts_ph}
               AND f.mtime BETWEEN {from_ph} AND {to_ph}
               {kind_clause}
               {filename_clause}
             LIMIT {limit_ph}
         )"
    );
    let refs = p.as_refs();
    let count: i64 = conn.query_row(&sql, refs.as_slice(), |row| row.get(0))?;
    Ok(count as usize)
}

/// FTS5 trigram pre-filter. Returns up to `limit` candidate rows.
/// Content is intentionally left empty; callers that need content must fetch it separately.
pub fn fts_candidates(
    conn: &Connection,
    query: &str,
    limit: usize,
    phrase: bool,
    date: DateFilter,
) -> Result<Vec<CandidateRow>> {
    // When FTS terms are empty (e.g. `regex:.*`) but a path_prefix filter is
    // active, fall back to a direct files-table scan so that path-scoped queries
    // with trivial regex patterns still return results.  The caller's LIMIT is
    // respected, so performance is bounded even for catch-all patterns.
    let fts_query = match build_fts_query(query, phrase) {
        Some(q) => q,
        None if date.path_prefix.is_some() => {
            let prefix = date.path_prefix.as_deref().unwrap_or("");
            let filename_clause = if date.filename_only {
                "AND f.id NOT LIKE '%::%'"   // not used for path_prefix fallback, but safe
            } else { "" };
            let mut p = ParamBinder::new();
            let eq_ph   = p.push(prefix.to_string());
            let like_ph = p.push(format!("{prefix}/%"));
            let limit_ph = p.push(limit as i64);
            let from_ph = p.push(date.from.unwrap_or(i64::MIN));
            let to_ph   = p.push(date.to.unwrap_or(i64::MAX));
            let kind_clause = if date.kinds.is_empty() {
                String::new()
            } else {
                let phs = date.kinds.iter().map(|k| p.push(k.to_string())).collect::<Vec<_>>().join(", ");
                format!("AND f.kind IN ({phs})")
            };
            // Return the filename row (line_number=0) for each matching file.
            let sql = format!(
                "SELECT f.path, f.kind, 0 AS line_number, f.id, f.mtime, f.size
                 FROM files f
                 WHERE (f.path = {eq_ph} OR f.path LIKE {like_ph})
                   AND f.mtime BETWEEN {from_ph} AND {to_ph}
                   {kind_clause}
                   {filename_clause}
                 LIMIT {limit_ph}"
            );
            let refs = p.as_refs();
            let mut stmt = conn.prepare(&sql)?;
            let raw: Vec<_> = stmt.query_map(refs.as_slice(), |row| {
                let file_kind_str: String = row.get(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    FileKind::from(file_kind_str.as_str()),
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            })?.collect::<rusqlite::Result<_>>()?;
            let mut results = Vec::with_capacity(raw.len());
            for (file_path, file_kind, file_id, mtime, size) in raw {
                let (fp, ap) = split_composite_path(&file_path);
                results.push(CandidateRow {
                    file_path: fp, file_kind, archive_path: ap,
                    line_number: 0, content: String::new(),
                    mtime, size, file_id,
                });
            }
            return Ok(results);
        }
        None => return Ok(vec![]),
    };

    struct RawRow {
        file_path: String,
        file_kind: FileKind,
        line_number: usize,
        file_id: i64,
        mtime: i64,
        size: Option<i64>,
    }

    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<RawRow> {
        let file_kind_str: String = row.get(1)?;
        Ok(RawRow {
            file_path:   row.get(0)?,
            file_kind:   FileKind::from(file_kind_str.as_str()),
            line_number: row.get::<_, i64>(2)? as usize,
            file_id:     row.get(3)?,
            mtime:       row.get(4)?,
            size:        row.get(5)?,
        })
    };

    let filename_clause = if date.filename_only { &format!("AND {SQL_FTS_FILENAME_ONLY}") } else { "" };

    let raw: Vec<RawRow> = if date.is_active() || date.filename_only {
        let from = date.from.unwrap_or(i64::MIN);
        let to = date.to.unwrap_or(i64::MAX);

        let mut p = ParamBinder::new();
        let fts_ph   = p.push(fts_query.clone());
        let limit_ph = p.push(limit as i64);
        let from_ph  = p.push(from);
        let to_ph    = p.push(to);
        let kind_clause = if date.kinds.is_empty() {
            String::new()
        } else {
            let phs = date.kinds.iter().map(|k| p.push(k.to_string())).collect::<Vec<_>>().join(", ");
            format!("AND f.kind IN ({phs})")
        };
        let path_prefix_clause = if let Some(ref prefix) = date.path_prefix {
            let eq_ph   = p.push(prefix.clone());
            let like_ph = p.push(format!("{prefix}/%"));
            format!("AND (f.path = {eq_ph} OR f.path LIKE {like_ph})")
        } else {
            String::new()
        };

        let sql = format!(
            "SELECT f.path, f.kind, {SQL_FTS_LINE_NUMBER} AS line_number,
                    f.id, f.mtime, f.size
             FROM lines_fts
             JOIN files f ON f.id = {SQL_FTS_FILE_ID}
             WHERE lines_fts MATCH {fts_ph}
               AND f.mtime BETWEEN {from_ph} AND {to_ph}
               {kind_clause}
               {path_prefix_clause}
               {filename_clause}
             LIMIT {limit_ph}"
        );
        let refs = p.as_refs();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(refs.as_slice(), map_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    } else {
        let mut stmt = conn.prepare(&format!(
            "SELECT f.path, f.kind, {SQL_FTS_LINE_NUMBER} AS line_number,
                    f.id, f.mtime, f.size
             FROM lines_fts
             JOIN files f ON f.id = {SQL_FTS_FILE_ID}
             WHERE lines_fts MATCH ?1
             LIMIT ?2",
        ))?;
        let rows = stmt.query_map(params![fts_query, limit as i64], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut results = Vec::with_capacity(raw.len());
    for row in raw {
        let (file_path, archive_path) = split_composite_path(&row.file_path);
        results.push(CandidateRow {
            file_path,
            file_kind:   row.file_kind,
            archive_path,
            line_number: row.line_number,
            content:     String::new(),
            mtime:       row.mtime,
            size:        row.size,
            file_id:     row.file_id,
        });
    }

    Ok(results)
}

/// One document-mode result group: the top FTS-ranked line plus additional
/// lines that cover query terms not present in the representative.
pub(crate) struct DocumentGroup {
    pub representative: CandidateRow,
    pub members:        Vec<CandidateRow>,
}

/// Return type for `document_candidates`: total qualifying files + per-file groups.
pub type DocumentCandidates = (usize, Vec<DocumentGroup>);

/// Document-level fuzzy candidate search.
/// Content is intentionally left empty; callers that need content must fetch it separately.
pub fn document_candidates(
    conn: &Connection,
    query: &str,
    limit: usize,
    date: DateFilter,
) -> Result<DocumentCandidates> {
    use std::collections::HashSet;

    let tokens: Vec<String> = query
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect();

    if tokens.is_empty() {
        return Ok((0, vec![]));
    }

    // For each token, collect the set of file_ids that have at least one matching line.
    let mut per_token_ids: Vec<HashSet<i64>> = Vec::new();
    for token in &tokens {
        let fts_expr = format!("\"{}\"", token.replace('"', "\"\""));
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT {SQL_FTS_FILE_ID} AS file_id
             FROM lines_fts
             JOIN files f ON f.id = {SQL_FTS_FILE_ID}
             WHERE lines_fts MATCH ?1
             LIMIT 100000",
        ))?;
        let ids: HashSet<i64> = stmt
            .query_map(params![fts_expr], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        per_token_ids.push(ids);
    }

    // Intersect: files that have ALL tokens somewhere.
    let mut qualifying_ids: HashSet<i64> = per_token_ids
        .into_iter()
        .reduce(|a, b| a.intersection(&b).copied().collect())
        .unwrap_or_default();

    // Apply date/kind/path_prefix filter.
    if date.is_active() && !qualifying_ids.is_empty() {
        let from = date.from.unwrap_or(i64::MIN);
        let to = date.to.unwrap_or(i64::MAX);

        let mut p = ParamBinder::new();
        let from_ph = p.push(from);
        let to_ph   = p.push(to);
        let id_phs  = qualifying_ids.iter().map(|&id| p.push(id)).collect::<Vec<_>>().join(", ");
        let kind_clause = if date.kinds.is_empty() {
            String::new()
        } else {
            let phs = date.kinds.iter().map(|k| p.push(k.to_string())).collect::<Vec<_>>().join(", ");
            format!("AND kind IN ({phs})")
        };
        let path_prefix_clause = if let Some(ref prefix) = date.path_prefix {
            let eq_ph   = p.push(prefix.clone());
            let like_ph = p.push(format!("{prefix}/%"));
            format!("AND (path = {eq_ph} OR path LIKE {like_ph})")
        } else {
            String::new()
        };

        let sql = format!(
            "SELECT id FROM files WHERE id IN ({id_phs}) AND mtime BETWEEN {from_ph} AND {to_ph} {kind_clause} {path_prefix_clause}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let refs = p.as_refs();
        let filtered: HashSet<i64> = stmt
            .query_map(refs.as_slice(), |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        qualifying_ids = filtered;
    }

    let total = qualifying_ids.len();
    if total == 0 {
        return Ok((0, vec![]));
    }

    let or_expr = tokens
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");

    let per_file_cap = tokens.len().max(1);
    let fetch_limit = (limit * 20 * per_file_cap).max(10_000) as i64;

    struct RawRow {
        file_path: String,
        file_kind: FileKind,
        line_number: usize,
        file_id: i64,
        mtime: i64,
        size: Option<i64>,
    }

    let mut stmt = conn.prepare(&format!(
        "SELECT f.path, f.kind, {SQL_FTS_LINE_NUMBER} AS line_number,
                f.id, f.mtime, f.size
         FROM lines_fts
         JOIN files f ON f.id = {SQL_FTS_FILE_ID}
         WHERE lines_fts MATCH ?1
         ORDER BY lines_fts.rank
         LIMIT ?2",
    ))?;

    // Collect up to `per_file_cap` raw rows per qualifying file.
    let mut file_rows: HashMap<i64, Vec<RawRow>> = HashMap::new();
    let mut file_order: Vec<i64> = Vec::new();

    let mut rows = stmt.query(params![or_expr, fetch_limit])?;
    while let Some(row) = rows.next()? {
        let file_id: i64 = row.get(3)?;
        if !qualifying_ids.contains(&file_id) {
            continue;
        }
        let entry = file_rows.entry(file_id).or_insert_with(|| {
            file_order.push(file_id);
            Vec::new()
        });
        if entry.len() < per_file_cap {
            let file_kind_str: String = row.get(1)?;
            entry.push(RawRow {
                file_path:   row.get(0)?,
                file_kind:   FileKind::from(file_kind_str.as_str()),
                line_number: row.get::<_, i64>(2)? as usize,
                file_id,
                mtime:       row.get(4)?,
                size:        row.get(5)?,
            });
        }
        if file_order.len() >= limit && file_rows.get(&file_order[file_order.len()-1]).map_or(0, |v| v.len()) >= per_file_cap {
            break;
        }
    }

    let mut results = Vec::new();
    for file_id in file_order.into_iter().take(limit) {
        let rows = match file_rows.remove(&file_id) {
            Some(r) => r,
            None => continue,
        };
        let rep_row = &rows[0];
        let (file_path, archive_path) = split_composite_path(&rep_row.file_path);
        let representative = CandidateRow {
            file_path,
            file_kind:   rep_row.file_kind.clone(),
            archive_path,
            line_number: rep_row.line_number,
            content:     String::new(),
            mtime:       rep_row.mtime,
            size:        rep_row.size,
            file_id,
        };
        results.push(DocumentGroup { representative, members: vec![] });
    }

    Ok((total, results))
}

/// Fetch duplicate paths grouped by file ID.
/// Returns a map of file_id → list of other paths sharing the same content_hash.
pub fn fetch_duplicates_for_file_ids(
    conn: &Connection,
    file_ids: &[i64],
) -> Result<HashMap<i64, Vec<String>>> {
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    if file_ids.is_empty() { return Ok(map); }

    let mut p = ParamBinder::new();
    let id_phs = file_ids.iter().map(|&id| p.push(id)).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT d1.file_id, f2.path
         FROM duplicates d1
         JOIN duplicates d2 ON d2.file_hash = d1.file_hash AND d2.file_id != d1.file_id
         JOIN files f2 ON f2.id = d2.file_id
         WHERE d1.file_id IN ({id_phs})
         ORDER BY d1.file_id, f2.path"
    );
    let refs = p.as_refs();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(refs.as_slice(), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (fid, path) = row?;
        map.entry(fid).or_default().push(path);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::encode_fts_rowid;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(include_str!("../schema_v4.sql")).unwrap();
        crate::db::register_scalar_functions(&conn).unwrap();
        conn
    }

    /// Insert a file with FTS entries. Returns the file_id.
    /// `lines` is `(line_number, content)` pairs in order.
    fn insert_inline_file(conn: &Connection, path: &str, mtime: i64, kind: &str, lines: &[(usize, &str)]) -> i64 {
        conn.execute(
            "INSERT INTO files (path, mtime, kind, line_count) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![path, mtime, kind, lines.len() as i64],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        for (line_number, line_content) in lines.iter() {
            let rowid = encode_fts_rowid(file_id, *line_number as i64);
            conn.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![rowid, line_content],
            ).unwrap();
        }

        file_id
    }

    // ── fts_candidates SQL tests ─────────────────────────────────────────────

    #[test]
    fn fts_candidates_finds_matching_content() {
        let conn = test_conn();


        insert_inline_file(&conn, "docs/readme.txt", 1000, "text", &[
            (0, "[PATH] docs/readme.txt"),
            (1, ""),
            (2, "hello world information here"),
        ]);
        insert_inline_file(&conn, "docs/other.txt", 1000, "text", &[
            (0, "[PATH] docs/other.txt"),
            (1, ""),
            (2, "unrelated content"),
        ]);

        let results = fts_candidates(&conn, "hello world", 100, false, DateFilter::default()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "docs/readme.txt");
        assert_eq!(results[0].line_number, 2);
        // content is not populated by fts_candidates (plan 080: no ZIP reads for non-regex modes)
    }

    #[test]
    fn fts_candidates_no_match_returns_empty() {
        let conn = test_conn();


        insert_inline_file(&conn, "file.txt", 1000, "text", &[
            (0, "[PATH] file.txt"),
            (1, ""),
            (2, "some content here"),
        ]);

        let results = fts_candidates(&conn, "xyznonexistent", 100, false, DateFilter::default()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn fts_count_matches_candidate_count() {
        let conn = test_conn();


        insert_inline_file(&conn, "a.txt", 1000, "text", &[
            (0, "[PATH] a.txt"),
            (1, ""),
            (2, "searchable term here"),
        ]);
        insert_inline_file(&conn, "b.txt", 1000, "text", &[
            (0, "[PATH] b.txt"),
            (1, ""),
            (2, "searchable term there"),
        ]);

        let count = fts_count(&conn, "searchable term", 100, false, DateFilter::default()).unwrap();
        let candidates = fts_candidates(&conn, "searchable term", 100, false, DateFilter::default()).unwrap();
        assert_eq!(count, candidates.len());
        assert_eq!(count, 2);
    }

    #[test]
    fn fts_candidates_date_filter_restricts_by_mtime() {
        let conn = test_conn();


        insert_inline_file(&conn, "old.txt", 100, "text", &[
            (0, "[PATH] old.txt"),
            (1, ""),
            (2, "matching content here"),
        ]);
        insert_inline_file(&conn, "new.txt", 9000, "text", &[
            (0, "[PATH] new.txt"),
            (1, ""),
            (2, "matching content here"),
        ]);

        let filter = DateFilter { from: Some(5000), to: Some(i64::MAX), ..Default::default() };
        let results = fts_candidates(&conn, "matching content", 100, false, filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "new.txt");
    }

    #[test]
    fn fts_candidates_kind_filter() {
        let conn = test_conn();


        insert_inline_file(&conn, "doc.pdf", 1000, "pdf", &[
            (0, "[PATH] doc.pdf"),
            (1, ""),
            (2, "common search term"),
        ]);
        insert_inline_file(&conn, "note.txt", 1000, "text", &[
            (0, "[PATH] note.txt"),
            (1, ""),
            (2, "common search term"),
        ]);

        let filter = DateFilter { kinds: vec![FileKind::Pdf], ..Default::default() };
        let results = fts_candidates(&conn, "common search", 100, false, filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_kind, FileKind::Pdf);
    }

    #[test]
    fn fts_candidates_filename_only_restricts_to_line_zero() {
        let conn = test_conn();


        insert_inline_file(&conn, "docs/needle.txt", 1000, "text", &[
            (0, "[PATH] docs/needle.txt"),
            (1, ""),
            (2, "content that also has needle"),
        ]);

        let filter = DateFilter { filename_only: true, ..Default::default() };
        let results = fts_candidates(&conn, "needle", 100, false, filter).unwrap();
        // filename_only=true restricts to line_number=0; the content line is excluded.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line_number, 0);
    }

    #[test]
    fn fts_candidates_respects_limit() {
        let conn = test_conn();


        for i in 0..10i64 {
            insert_inline_file(&conn, &format!("file_{i}.txt"), 1000 + i, "text", &[
                (0, &format!("[PATH] file_{i}.txt")),
                (1, ""),
                (2, "common content term here"),
            ]);
        }

        let results = fts_candidates(&conn, "common content", 3, false, DateFilter::default()).unwrap();
        assert_eq!(results.len(), 3);
    }

    // ── build_fts_query ──────────────────────────────────────────────────────

    #[test]
    fn fts_phrase_wraps_in_quotes() {
        assert_eq!(build_fts_query("hello world", true).as_deref(), Some("\"hello world\""));
    }

    #[test]
    fn fts_phrase_too_short_returns_none() {
        assert!(build_fts_query("ab", true).is_none());
    }

    #[test]
    fn fts_phrase_exactly_3_chars_ok() {
        assert!(build_fts_query("abc", true).is_some());
    }

    #[test]
    fn fts_fuzzy_joins_terms_with_and() {
        let q = build_fts_query("foo bar", false).unwrap();
        assert!(q.contains("foo"));
        assert!(q.contains("AND"));
        assert!(q.contains("bar"));
    }

    #[test]
    fn fts_fuzzy_filters_short_terms() {
        // All terms < 3 chars → None
        assert!(build_fts_query("to go", false).is_none());
    }

    #[test]
    fn fts_fuzzy_mixed_length_keeps_long_terms() {
        // "to" (2 chars) is filtered, "foo" (3 chars) is kept
        let q = build_fts_query("to foo", false).unwrap();
        assert!(q.contains("foo"));
        assert!(!q.contains("to"));
    }

    #[test]
    fn fts_fuzzy_splits_on_dot() {
        let q = build_fts_query("plan.index", false).unwrap();
        assert!(!q.contains('.'));
        assert!(q.contains("plan") && q.contains("index"));
    }

    #[test]
    fn fts_fuzzy_splits_on_special_chars() {
        let q = build_fts_query("test^query", false).unwrap();
        assert!(!q.contains('^'));
        assert!(q.contains("test") && q.contains("query"));
    }

    // ── document_candidates ──────────────────────────────────────────────────

    #[test]
    fn document_candidates_empty_query_returns_empty() {
        let conn = test_conn();
        insert_inline_file(&conn, "file.txt", 1000, "text", &[
            (0, "[PATH] file.txt"),
            (2, "hello world content"),
        ]);

        let (total, groups) = document_candidates(&conn, "", 100, DateFilter::default()).unwrap();
        assert_eq!(total, 0);
        assert!(groups.is_empty());

        // Also test with all-short tokens (< 3 chars).
        let (total, groups) = document_candidates(&conn, "ab", 100, DateFilter::default()).unwrap();
        assert_eq!(total, 0);
        assert!(groups.is_empty());
    }

    #[test]
    fn document_candidates_returns_file_for_matching_token() {
        let conn = test_conn();
        insert_inline_file(&conn, "match.txt", 1000, "text", &[
            (0, "[PATH] match.txt"),
            (2, "hello world information here"),
        ]);
        insert_inline_file(&conn, "other.txt", 1000, "text", &[
            (0, "[PATH] other.txt"),
            (2, "unrelated stuff here"),
        ]);

        let (total, groups) = document_candidates(&conn, "hello", 100, DateFilter::default()).unwrap();
        assert_eq!(total, 1, "only one file contains 'hello'");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].representative.file_path, "match.txt");
    }

    #[test]
    fn document_candidates_multi_token_requires_all_tokens() {
        let conn = test_conn();
        // File A has both "alpha" and "beta".
        insert_inline_file(&conn, "both.txt", 1000, "text", &[
            (0, "[PATH] both.txt"),
            (2, "alpha term here"),
            (3, "beta term here"),
        ]);
        // File B has only "alpha".
        insert_inline_file(&conn, "alpha_only.txt", 1000, "text", &[
            (0, "[PATH] alpha_only.txt"),
            (2, "alpha term here"),
        ]);

        let (total, groups) = document_candidates(&conn, "alpha beta", 100, DateFilter::default()).unwrap();
        assert_eq!(total, 1, "only 'both.txt' has both tokens");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].representative.file_path, "both.txt");
    }

    #[test]
    fn document_candidates_per_file_cap() {
        let conn = test_conn();
        // Insert a file with the keyword on 20 lines.
        let owned: Vec<(usize, String)> = std::iter::once((0usize, "[PATH] dense.txt".to_string()))
            .chain((2usize..22).map(|i| (i, "keyword content line".to_string())))
            .collect();
        let refs: Vec<(usize, &str)> = owned.iter().map(|(n, s)| (*n, s.as_str())).collect();
        insert_inline_file(&conn, "dense.txt", 1000, "text", &refs);

        // One-token query → per_file_cap = 1.
        let (_total, groups) = document_candidates(&conn, "keyword", 100, DateFilter::default()).unwrap();
        assert_eq!(groups.len(), 1, "should return exactly one group for the file");
        // The group has no members because members is always empty in current impl.
        assert!(groups[0].members.is_empty());
    }

    #[test]
    fn document_candidates_respects_limit() {
        let conn = test_conn();
        for i in 0..10i64 {
            insert_inline_file(&conn, &format!("file_{i}.txt"), 1000 + i, "text", &[
                (0, &format!("[PATH] file_{i}.txt")),
                (2, "common keyword content"),
            ]);
        }

        let (total, groups) = document_candidates(&conn, "keyword", 3, DateFilter::default()).unwrap();
        assert_eq!(total, 10, "total should count all qualifying files");
        assert_eq!(groups.len(), 3, "groups should be capped at limit");
    }

    #[test]
    fn document_candidates_date_filter_restricts_results() {
        let conn = test_conn();
        insert_inline_file(&conn, "old.txt", 100, "text", &[
            (0, "[PATH] old.txt"),
            (2, "keyword present here"),
        ]);
        insert_inline_file(&conn, "new.txt", 9000, "text", &[
            (0, "[PATH] new.txt"),
            (2, "keyword present here"),
        ]);

        let filter = DateFilter { from: Some(5000), to: Some(i64::MAX), ..Default::default() };
        let (total, groups) = document_candidates(&conn, "keyword", 100, filter).unwrap();
        assert_eq!(total, 1);
        assert_eq!(groups[0].representative.file_path, "new.txt");
    }

    #[test]
    fn document_candidates_kind_filter() {
        let conn = test_conn();
        insert_inline_file(&conn, "doc.pdf", 1000, "pdf", &[
            (0, "[PATH] doc.pdf"),
            (2, "common keyword content"),
        ]);
        insert_inline_file(&conn, "note.txt", 1000, "text", &[
            (0, "[PATH] note.txt"),
            (2, "common keyword content"),
        ]);

        let filter = DateFilter { kinds: vec![FileKind::Text], ..Default::default() };
        let (total, groups) = document_candidates(&conn, "keyword", 100, filter).unwrap();
        assert_eq!(total, 1);
        assert_eq!(groups[0].representative.file_kind, FileKind::Text);
    }

    // ── ParamBinder ────────────────────────────────────────────────────────────

    #[test]
    fn param_binder_sequential_placeholders() {
        let mut p = ParamBinder::new();
        assert_eq!(p.push("first"),  "?1");
        assert_eq!(p.push("second"), "?2");
        assert_eq!(p.push(42i64),    "?3");
        assert_eq!(p.push("fourth"), "?4");
    }

    #[test]
    fn param_binder_as_refs_length_matches_push_count() {
        let mut p = ParamBinder::new();
        p.push("a");
        p.push(1i64);
        p.push("b");
        assert_eq!(p.as_refs().len(), 3);
    }

    #[test]
    fn param_binder_empty_has_no_refs() {
        let p = ParamBinder::new();
        assert!(p.as_refs().is_empty());
    }
}
