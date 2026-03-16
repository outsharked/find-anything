use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::archive::ArchiveManager;

use super::read_chunk_lines;
use super::split_composite_path;

/// Combined search filter: optional date range (mtime), optional kind allowlist,
/// and optional filename-only restriction.
#[derive(Debug, Clone, Default)]
pub struct DateFilter {
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// Allowlist of `files.kind` values (e.g. "pdf", "image"). Empty = any kind.
    pub kinds: Vec<String>,
    /// When true, restrict matches to line_number = 0 (filename-only search).
    pub filename_only: bool,
}

impl DateFilter {
    pub fn is_active(&self) -> bool {
        self.from.is_some() || self.to.is_some() || !self.kinds.is_empty()
    }
}

/// Build `AND f.kind IN (?{start}, ...)` for `n` kind values, or empty if n == 0.
fn kind_in_clause(n: usize, start: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let placeholders = (start..start + n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("AND f.kind IN ({placeholders})")
}

// ── Search ────────────────────────────────────────────────────────────────────

pub struct CandidateRow {
    /// Full path, potentially composite ("archive.zip::member.txt").
    pub file_path: String,
    pub file_kind: String,
    /// For archive members: the part after the first "::".
    /// For outer files: None.
    pub archive_path: Option<String>,
    pub line_number: usize,
    pub content: String,
    pub mtime: i64,
    pub size: Option<i64>,
    /// The file's row ID in the `files` table (used for alias lookup).
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
        // Use unquoted terms so FTS5 treats each word as a token query rather
        // than a phrase query.  Quoted phrases require ≥3 trigrams to match
        // (i.e. the term must be ≥5 chars), which breaks short-word searches
        // like "test" (4 chars, 2 trigrams).  Unquoted token queries have no
        // such minimum.  Split on any non-alphanumeric character (except `_`)
        // so that e.g. "plan.index" yields ["plan", "index"] rather than a
        // single token that FTS5 cannot parse.
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
    // Date/kind/filename filter active: need JOIN to lines and files.
    let from = date.from.unwrap_or(i64::MIN);
    let to = date.to.unwrap_or(i64::MAX);
    let kind_clause = kind_in_clause(date.kinds.len(), 5);
    let filename_clause = if date.filename_only { "AND l.line_number = 0" } else { "" };
    let sql = format!(
        "SELECT count(*) FROM (
             SELECT 1
             FROM lines_fts
             JOIN lines l ON l.id = lines_fts.rowid
             JOIN files f ON f.id = l.file_id
             WHERE lines_fts MATCH ?1
               AND f.mtime BETWEEN ?3 AND ?4
               {kind_clause}
               {filename_clause}
             LIMIT ?2
         )"
    );
    let mut dyn_params: Vec<Box<dyn rusqlite::ToSql>> = vec![
        Box::new(fts_query),
        Box::new(limit as i64),
        Box::new(from),
        Box::new(to),
    ];
    for k in &date.kinds {
        dyn_params.push(Box::new(k.clone()));
    }
    let count: i64 = conn.query_row(
        &sql,
        rusqlite::params_from_iter(dyn_params.iter().map(|p| p.as_ref())),
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// FTS5 trigram pre-filter. Returns up to `limit` candidate rows.
pub fn fts_candidates(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    query: &str,
    limit: usize,
    phrase: bool,
    date: DateFilter,
) -> Result<Vec<CandidateRow>> {
    let Some(fts_query) = build_fts_query(query, phrase) else {
        return Ok(vec![]);
    };

    struct RawRow {
        file_path: String,
        file_kind: String,
        line_number: usize,
        chunk_archive: Option<String>,
        chunk_name: Option<String>,
        line_offset: usize,
        file_id: i64,
        mtime: i64,
        size: Option<i64>,
    }

    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<RawRow> {
        Ok(RawRow {
            file_path:     row.get(0)?,
            file_kind:     row.get(1)?,
            line_number:   row.get::<_, i64>(2)? as usize,
            chunk_archive: row.get(3)?,
            chunk_name:    row.get(4)?,
            line_offset:   row.get::<_, i64>(5)? as usize,
            file_id:       row.get(6)?,
            mtime:         row.get(7)?,
            size:          row.get(8)?,
        })
    };

    let filename_clause = if date.filename_only { "AND l.line_number = 0" } else { "" };

    let raw: Vec<RawRow> = if date.is_active() || date.filename_only {
        let from = date.from.unwrap_or(i64::MIN);
        let to = date.to.unwrap_or(i64::MAX);
        let kind_clause = kind_in_clause(date.kinds.len(), 5);
        let sql = format!(
            "SELECT f.path, f.kind, l.line_number,
                    l.chunk_archive, l.chunk_name, l.line_offset_in_chunk, f.id,
                    f.mtime, f.size
             FROM lines_fts
             JOIN lines l ON l.id = lines_fts.rowid
             JOIN files f ON f.id = l.file_id
             WHERE lines_fts MATCH ?1
               AND f.mtime BETWEEN ?3 AND ?4
               {kind_clause}
               {filename_clause}
             LIMIT ?2"
        );
        let mut dyn_params: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(fts_query.clone()),
            Box::new(limit as i64),
            Box::new(from),
            Box::new(to),
        ];
        for k in &date.kinds {
            dyn_params.push(Box::new(k.clone()));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(dyn_params.iter().map(|p| p.as_ref())),
            map_row,
        )?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    } else {
        let mut stmt = conn.prepare(
            "SELECT f.path, f.kind, l.line_number,
                    l.chunk_archive, l.chunk_name, l.line_offset_in_chunk, f.id,
                    f.mtime, f.size
             FROM lines_fts
             JOIN lines l ON l.id = lines_fts.rowid
             JOIN files f ON f.id = l.file_id
             WHERE lines_fts MATCH ?1
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, limit as i64], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    // Read content from ZIP archives or inline storage, caching chunks to avoid redundant reads.
    let mut chunk_cache: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut results = Vec::with_capacity(raw.len());

    for row in raw {
        let content = read_chunk_lines(
            &mut chunk_cache, archive_mgr, conn,
            row.file_id,
            row.chunk_archive.as_deref(),
            row.chunk_name.as_deref(),
        )
            .get(row.line_offset)
            .cloned()
            .unwrap_or_default();

        // Split composite path into outer path + archive_path for search result compat.
        let (file_path, archive_path) = split_composite_path(&row.file_path);

        results.push(CandidateRow {
            file_path,
            file_kind:    row.file_kind,
            archive_path,
            line_number:  row.line_number,
            content,
            mtime:        row.mtime,
            size:         row.size,
            file_id:      row.file_id,
        });
    }

    Ok(results)
}

/// Return type for `document_candidates`: total qualifying files + per-file (representative, extras).
pub type DocumentCandidates = (usize, Vec<(CandidateRow, Vec<CandidateRow>)>);

/// Document-level fuzzy candidate search.
///
/// Unlike `fts_candidates` (which requires all query terms on the *same* line),
/// this finds files where each query term appears on *any* line, then surfaces
/// one result per file with extra_matches carrying the best line per remaining token.
///
/// Returns `(total, Vec<(representative, extra_matches)>)`.
/// `total` is the number of qualifying files before the limit is applied.
pub fn document_candidates(
    conn: &Connection,
    archive_mgr: &ArchiveManager,
    query: &str,
    limit: usize,
    date: DateFilter,
    case_sensitive: bool,
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
        let mut stmt = conn.prepare(
            "SELECT DISTINCT l.file_id
             FROM lines_fts
             JOIN lines l ON l.id = lines_fts.rowid
             WHERE lines_fts MATCH ?1
             LIMIT 100000",
        )?;
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

    // Apply date/kind filter: keep only files that match mtime range and/or kind allowlist.
    if date.is_active() && !qualifying_ids.is_empty() {
        let from = date.from.unwrap_or(i64::MIN);
        let to = date.to.unwrap_or(i64::MAX);
        let id_count = qualifying_ids.len();
        // Build an IN clause for the current qualifying set: ?3..?(id_count+2)
        let id_placeholders: String = (3..3 + id_count)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        // Kind IN clause starts after the id placeholders: ?(id_count+3)..
        let kind_start = id_count + 3;
        let kind_clause = if date.kinds.is_empty() {
            String::new()
        } else {
            let placeholders = (kind_start..kind_start + date.kinds.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("AND kind IN ({placeholders})")
        };
        let sql = format!(
            "SELECT id FROM files WHERE id IN ({id_placeholders}) AND mtime BETWEEN ?1 AND ?2 {kind_clause}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let id_params: Vec<Box<dyn rusqlite::ToSql>> = std::iter::once(Box::new(from) as Box<dyn rusqlite::ToSql>)
            .chain(std::iter::once(Box::new(to) as Box<dyn rusqlite::ToSql>))
            .chain(qualifying_ids.iter().map(|&id| Box::new(id) as Box<dyn rusqlite::ToSql>))
            .chain(date.kinds.iter().map(|k| Box::new(k.clone()) as Box<dyn rusqlite::ToSql>))
            .collect();
        let filtered: HashSet<i64> = stmt
            .query_map(rusqlite::params_from_iter(id_params.iter().map(|p| p.as_ref())), |row| row.get(0))?
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

    // Fetch up to `tokens.len()` lines per qualifying file so we can pick the best
    // line per token. We need enough rows to fill `limit` files × N tokens.
    let per_file_cap = tokens.len().max(1);
    let fetch_limit = (limit * 20 * per_file_cap).max(10_000) as i64;

    struct RawRow {
        file_path: String,
        file_kind: String,
        line_number: usize,
        chunk_archive: Option<String>,
        chunk_name: Option<String>,
        line_offset: usize,
        file_id: i64,
        mtime: i64,
        size: Option<i64>,
    }

    let mut stmt = conn.prepare(
        "SELECT f.path, f.kind, l.line_number,
                l.chunk_archive, l.chunk_name, l.line_offset_in_chunk, f.id,
                f.mtime, f.size
         FROM lines_fts
         JOIN lines l ON l.id = lines_fts.rowid
         JOIN files f ON f.id = l.file_id
         WHERE lines_fts MATCH ?1
         ORDER BY lines_fts.rank
         LIMIT ?2",
    )?;

    // Collect up to `per_file_cap` raw rows per qualifying file.
    let mut file_rows: HashMap<i64, Vec<RawRow>> = HashMap::new();
    let mut file_order: Vec<i64> = Vec::new(); // insertion order for stable output

    let mut rows = stmt.query(params![or_expr, fetch_limit])?;
    while let Some(row) = rows.next()? {
        let file_id: i64 = row.get(6)?;
        if !qualifying_ids.contains(&file_id) {
            continue;
        }
        let entry = file_rows.entry(file_id).or_insert_with(|| {
            file_order.push(file_id);
            Vec::new()
        });
        if entry.len() < per_file_cap {
            entry.push(RawRow {
                file_path:    row.get(0)?,
                file_kind:    row.get(1)?,
                line_number:  row.get::<_, i64>(2)? as usize,
                chunk_archive: row.get::<_, Option<String>>(3)?,
                chunk_name:   row.get::<_, Option<String>>(4)?,
                line_offset:  row.get::<_, i64>(5)? as usize,
                file_id,
                mtime:        row.get(7)?,
                size:         row.get(8)?,
            });
        }
        if file_order.len() >= limit && file_rows.get(&file_order[file_order.len()-1]).map_or(0, |v| v.len()) >= per_file_cap {
            break;
        }
    }

    // Read content from ZIP archives, reusing a chunk cache.
    let mut chunk_cache: HashMap<(String, String), Vec<String>> = HashMap::new();
    // For case-insensitive matching, compare lowercased tokens against lowercased content.
    // For case-sensitive matching, compare tokens as-is.
    let tokens_cmp: Vec<String> = if case_sensitive {
        tokens.clone()
    } else {
        tokens.iter().map(|t| t.to_lowercase()).collect()
    };

    let mut results = Vec::new();
    for file_id in file_order.into_iter().take(limit) {
        let rows = match file_rows.remove(&file_id) {
            Some(r) => r,
            None => continue,
        };

        // First row is the top FTS-ranked line → the representative.
        let rep_row = &rows[0];
        let rep_content = read_chunk_lines(
            &mut chunk_cache, archive_mgr, conn,
            rep_row.file_id,
            rep_row.chunk_archive.as_deref(),
            rep_row.chunk_name.as_deref(),
        )
            .get(rep_row.line_offset)
            .cloned()
            .unwrap_or_default();
        let rep_content_cmp = if case_sensitive { rep_content.clone() } else { rep_content.to_lowercase() };
        let (file_path, archive_path) = split_composite_path(&rep_row.file_path);

        let representative = CandidateRow {
            file_path: file_path.clone(),
            file_kind: rep_row.file_kind.clone(),
            archive_path: archive_path.clone(),
            line_number: rep_row.line_number,
            content: rep_content,
            mtime: rep_row.mtime,
            size: rep_row.size,
            file_id,
        };

        // For each token not already covered by the representative, find the first
        // subsequent row that covers it.
        let mut uncovered: Vec<&str> = tokens_cmp
            .iter()
            .filter(|t| !rep_content_cmp.contains(t.as_str()))
            .map(|t| t.as_str())
            .collect();

        let mut extras: Vec<CandidateRow> = Vec::new();
        for extra_row in &rows[1..] {
            if uncovered.is_empty() {
                break;
            }
            let content = read_chunk_lines(
                &mut chunk_cache, archive_mgr, conn,
                extra_row.file_id,
                extra_row.chunk_archive.as_deref(),
                extra_row.chunk_name.as_deref(),
            )
                .get(extra_row.line_offset)
                .cloned()
                .unwrap_or_default();
            let content_cmp = if case_sensitive { content.clone() } else { content.to_lowercase() };
            // Only include this row if it covers at least one new token.
            let newly_covered: Vec<usize> = uncovered
                .iter()
                .enumerate()
                .filter(|(_, t)| content_cmp.contains(*t))
                .map(|(i, _)| i)
                .collect();
            if !newly_covered.is_empty() {
                // Skip line_number=0 (metadata/path lines) — not useful as highlights.
                if extra_row.line_number > 0 {
                    let (ep, ea) = split_composite_path(&extra_row.file_path);
                    extras.push(CandidateRow {
                        file_path: ep,
                        file_kind: extra_row.file_kind.clone(),
                        archive_path: ea,
                        line_number: extra_row.line_number,
                        content,
                        mtime: extra_row.mtime,
                        size: extra_row.size,
                        file_id,
                    });
                }
                // Remove newly covered tokens (iterate in reverse to preserve indices).
                for i in newly_covered.into_iter().rev() {
                    uncovered.swap_remove(i);
                }
            }
        }

        results.push((representative, extras));
    }

    Ok((total, results))
}

/// Fetch alias paths grouped by their canonical file ID.
/// Returns a map of canonical_id → list of alias paths.
pub fn fetch_aliases_for_canonical_ids(
    conn: &Connection,
    canonical_ids: &[i64],
) -> Result<HashMap<i64, Vec<String>>> {
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    if canonical_ids.is_empty() {
        return Ok(map);
    }
    let mut stmt = conn.prepare(
        "SELECT canonical_file_id, path FROM files
         WHERE canonical_file_id = ?1
         ORDER BY path",
    )?;
    for &cid in canonical_ids {
        let paths: Vec<String> = stmt
            .query_map(params![cid], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        if !paths.is_empty() {
            map.insert(cid, paths);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::ArchiveManager;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(include_str!("../schema_v2.sql")).unwrap();
        conn.execute_batch("DROP TABLE IF EXISTS pending_chunk_removes;").unwrap();
        crate::db::register_scalar_functions(&conn).unwrap();
        conn
    }

    /// Insert a file with inline content and FTS entries. Returns the file_id.
    /// `lines` is `(line_number, content)` pairs in order; the position in the
    /// slice is used as `line_offset_in_chunk` for the inline path.
    fn insert_inline_file(conn: &Connection, path: &str, mtime: i64, kind: &str, lines: &[(usize, &str)]) -> i64 {
        conn.execute(
            "INSERT INTO files (path, mtime, kind) VALUES (?1, ?2, ?3)",
            rusqlite::params![path, mtime, kind],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        let content: String = lines.iter().map(|(_, c)| *c).collect::<Vec<_>>().join("\n");
        conn.execute(
            "INSERT INTO file_content (file_id, content) VALUES (?1, ?2)",
            rusqlite::params![file_id, content],
        ).unwrap();

        for (offset, (line_number, line_content)) in lines.iter().enumerate() {
            let line_id: i64 = conn.query_row(
                "INSERT INTO lines (file_id, line_number, line_offset_in_chunk)
                 VALUES (?1, ?2, ?3) RETURNING id",
                rusqlite::params![file_id, *line_number as i64, offset as i64],
                |r| r.get(0),
            ).unwrap();
            conn.execute(
                "INSERT INTO lines_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![line_id, line_content],
            ).unwrap();
        }

        file_id
    }

    fn dummy_mgr() -> (tempfile::TempDir, ArchiveManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ArchiveManager::new_for_reading(dir.path().to_path_buf());
        (dir, mgr)
    }

    // ── fts_candidates SQL tests ─────────────────────────────────────────────

    #[test]
    fn fts_candidates_finds_matching_content() {
        let conn = test_conn();
        let (_dir, mgr) = dummy_mgr();

        insert_inline_file(&conn, "docs/readme.txt", 1000, "text", &[
            (0, "[PATH] docs/readme.txt"),
            (1, "hello world information here"),
        ]);
        insert_inline_file(&conn, "docs/other.txt", 1000, "text", &[
            (0, "[PATH] docs/other.txt"),
            (1, "unrelated content"),
        ]);

        let results = fts_candidates(&conn, &mgr, "hello world", 100, false, DateFilter::default()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "docs/readme.txt");
        assert_eq!(results[0].line_number, 1);
        assert!(results[0].content.contains("hello"));
    }

    #[test]
    fn fts_candidates_no_match_returns_empty() {
        let conn = test_conn();
        let (_dir, mgr) = dummy_mgr();

        insert_inline_file(&conn, "file.txt", 1000, "text", &[
            (0, "[PATH] file.txt"),
            (1, "some content here"),
        ]);

        let results = fts_candidates(&conn, &mgr, "xyznonexistent", 100, false, DateFilter::default()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn fts_count_matches_candidate_count() {
        let conn = test_conn();
        let (_dir, mgr) = dummy_mgr();

        insert_inline_file(&conn, "a.txt", 1000, "text", &[
            (0, "[PATH] a.txt"),
            (1, "searchable term here"),
        ]);
        insert_inline_file(&conn, "b.txt", 1000, "text", &[
            (0, "[PATH] b.txt"),
            (1, "searchable term there"),
        ]);

        let count = fts_count(&conn, "searchable term", 100, false, DateFilter::default()).unwrap();
        let candidates = fts_candidates(&conn, &mgr, "searchable term", 100, false, DateFilter::default()).unwrap();
        assert_eq!(count, candidates.len());
        assert_eq!(count, 2);
    }

    #[test]
    fn fts_candidates_date_filter_restricts_by_mtime() {
        let conn = test_conn();
        let (_dir, mgr) = dummy_mgr();

        insert_inline_file(&conn, "old.txt", 100, "text", &[
            (0, "[PATH] old.txt"),
            (1, "matching content here"),
        ]);
        insert_inline_file(&conn, "new.txt", 9000, "text", &[
            (0, "[PATH] new.txt"),
            (1, "matching content here"),
        ]);

        let filter = DateFilter { from: Some(5000), to: Some(i64::MAX), ..Default::default() };
        let results = fts_candidates(&conn, &mgr, "matching content", 100, false, filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "new.txt");
    }

    #[test]
    fn fts_candidates_kind_filter() {
        let conn = test_conn();
        let (_dir, mgr) = dummy_mgr();

        insert_inline_file(&conn, "doc.pdf", 1000, "pdf", &[
            (0, "[PATH] doc.pdf"),
            (1, "common search term"),
        ]);
        insert_inline_file(&conn, "note.txt", 1000, "text", &[
            (0, "[PATH] note.txt"),
            (1, "common search term"),
        ]);

        let filter = DateFilter { kinds: vec!["pdf".to_string()], ..Default::default() };
        let results = fts_candidates(&conn, &mgr, "common search", 100, false, filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_kind, "pdf");
    }

    #[test]
    fn fts_candidates_filename_only_restricts_to_line_zero() {
        let conn = test_conn();
        let (_dir, mgr) = dummy_mgr();

        insert_inline_file(&conn, "docs/needle.txt", 1000, "text", &[
            (0, "[PATH] docs/needle.txt"),
            (1, "content that also has needle"),
        ]);

        let filter = DateFilter { filename_only: true, ..Default::default() };
        let results = fts_candidates(&conn, &mgr, "needle", 100, false, filter).unwrap();
        // filename_only=true restricts to line_number=0; the content line is excluded.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line_number, 0);
    }

    #[test]
    fn fts_candidates_respects_limit() {
        let conn = test_conn();
        let (_dir, mgr) = dummy_mgr();

        for i in 0..10i64 {
            insert_inline_file(&conn, &format!("file_{i}.txt"), 1000 + i, "text", &[
                (0, &format!("[PATH] file_{i}.txt")),
                (1, "common content term here"),
            ]);
        }

        let results = fts_candidates(&conn, &mgr, "common content", 3, false, DateFilter::default()).unwrap();
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
        // "plan.index" should yield two tokens joined with AND, not a bare "plan.index"
        // that causes an FTS5 syntax error.
        let q = build_fts_query("plan.index", false).unwrap();
        assert!(!q.contains('.'));
        assert!(q.contains("plan") && q.contains("index"));
    }

    #[test]
    fn fts_fuzzy_splits_on_special_chars() {
        // Non-alphanumeric chars other than _ are treated as separators.
        let q = build_fts_query("test^query", false).unwrap();
        assert!(!q.contains('^'));
        // Both sides are long enough to survive the >=3 filter
        assert!(q.contains("test") && q.contains("query"));
    }
}
