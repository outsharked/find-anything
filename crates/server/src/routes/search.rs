use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, State},
    http::{request::Parts, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use tokio::task::spawn_blocking;

use find_common::api::{ContextLine, FileKind, SearchMode, SearchResponse, SearchResult};

use crate::fuzzy::FuzzyScorer;
use crate::{archive::ArchiveManager, db, db::search::{CandidateRow, DocumentGroup}, db::DateFilter, AppState};

/// A scored search result paired with its `file_id` for alias lookup.
struct ScoredResult {
    result:  SearchResult,
    file_id: i64,
}

use super::{check_auth, source_db_path};

// ── GET /api/v1/search ────────────────────────────────────────────────────────

pub struct SearchParams {
    pub q: String,
    pub mode: SearchMode,
    /// Collected from repeated ?source=a&source=b params.
    pub source: Vec<String>,
    pub limit: usize,
    pub offset: usize,
    /// Optional unix timestamp bounds for mtime filtering.
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    /// Optional file kind allowlist (e.g. "pdf", "image"). Empty = any kind.
    pub kinds: Vec<String>,
    /// When true, fuzzy/exact/document/regex matching is case-sensitive. Default: false.
    pub case_sensitive: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for SearchParams {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let raw = parts.uri.query().unwrap_or("");
        let mut q = None;
        let mut mode: SearchMode = SearchMode::default();
        let mut source = Vec::new();
        let mut limit = None;
        let mut offset = None;
        let mut date_from = None;
        let mut date_to = None;
        let mut kinds = Vec::new();
        let mut case_sensitive = false;

        for (k, v) in form_urlencoded::parse(raw.as_bytes()) {
            match k.as_ref() {
                "q"              => q         = Some(v.into_owned()),
                "mode"           => mode = serde_json::from_value(serde_json::Value::String(v.into_owned())).unwrap_or_default(),
                "source"         => source.push(v.into_owned()),
                "kind"           => kinds.push(v.into_owned()),
                "limit"          => limit     = Some(v.parse::<usize>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid limit".to_string()))?),
                "offset"         => offset    = Some(v.parse::<usize>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid offset".to_string()))?),
                "date_from"      => date_from = Some(v.parse::<i64>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid date_from".to_string()))?),
                "date_to"        => date_to   = Some(v.parse::<i64>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid date_to".to_string()))?),
                "case_sensitive" => case_sensitive = matches!(v.as_ref(), "1" | "true"),
                _ => {}
            }
        }

        Ok(SearchParams {
            q:    q.ok_or_else(|| (StatusCode::BAD_REQUEST, "missing 'q'".to_string()))?,
            mode,
            source,
            limit:     limit.unwrap_or(50),
            offset:    offset.unwrap_or(0),
            date_from,
            date_to,
            kinds,
            case_sensitive,
        })
    }
}

/// Extract maximal sequences of non-special characters from a regex pattern
/// to use as FTS5 pre-filter terms. Special regex chars (`^$.*+?|()[]{}\`)
/// act as delimiters; escaped sequences are skipped entirely.
///
/// Examples:
///   `^fn\s+\w+`   → "fn"   (too short, filtered out by fts_candidates)
///   `class\s+Foo` → "class Foo"
///   `password`    → "password"
fn regex_to_fts_terms(pattern: &str) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = pattern.chars();

    while let Some(c) = chars.next() {
        if c == '\\' {
            // Escaped sequence — flush and skip the next char.
            if !current.is_empty() {
                terms.push(std::mem::take(&mut current));
            }
            chars.next();
        } else if "^$.*+?|()[]{}".contains(c) {
            // Regex special char — flush current literal sequence.
            if !current.is_empty() {
                terms.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms.join(" ")
}

/// Group line-level candidates by file, returning one result per file.
/// The first occurrence per file (highest-ranked by FTS) is the representative;
/// additional occurrences on different lines become `extra_matches`.
fn group_by_file(
    candidates: Vec<CandidateRow>,
    source_name: &str,
) -> Vec<ScoredResult> {
    use std::collections::HashMap;
    let mut file_order: Vec<i64> = Vec::new();
    // (representative SearchResult, extras Vec)
    let mut file_reps: HashMap<i64, (SearchResult, Vec<ContextLine>)> = HashMap::new();

    for c in candidates {
        let file_id = c.file_id;
        if let Some((_, extras)) = file_reps.get_mut(&file_id) {
            if c.line_number > 0 {
                extras.push(ContextLine { line_number: c.line_number, content: c.content });
            }
        } else {
            file_order.push(file_id);
            let result = make_result(source_name, &c, 0, vec![]);
            file_reps.insert(file_id, (result, vec![]));
        }
    }

    file_order.into_iter().filter_map(|file_id| {
        let (mut result, extras) = file_reps.remove(&file_id)?;
        result.extra_matches = extras;
        Some(ScoredResult { result, file_id })
    }).collect()
}

fn make_result(
    source: &str,
    c: &CandidateRow,
    score: u32,
    extra_matches: Vec<ContextLine>,
) -> SearchResult {
    let snippet = c.content.strip_prefix("[PATH] ").map(|s| s.to_string()).unwrap_or_else(|| c.content.clone());
    SearchResult {
        source: source.to_string(),
        path: c.file_path.clone(),
        archive_path: c.archive_path.clone(),
        line_number: c.line_number,
        snippet,
        score,
        kind: c.file_kind.clone(),
        mtime: c.mtime,
        size: c.size,
        context_lines: vec![],
        duplicate_paths: vec![],
        extra_matches,
    }
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    params: SearchParams,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) { return (s, Json(serde_json::Value::Null)).into_response(); }

    let sources_dir = state.data_dir.join("sources");
    let fts_limit = state.config.search.fts_candidate_limit;
    let query = params.q.clone();
    let mode = params.mode;
    let limit = params.limit.min(state.config.search.max_limit);

    // Build the list of (source_name, db_path) to query.
    let source_dbs: Vec<(String, std::path::PathBuf)> = if params.source.is_empty() {
        // All sources: scan the sources directory.
        match std::fs::read_dir(&sources_dir) {
            Err(_) => vec![],
            Ok(rd) => rd
                .filter_map(|e| {
                    let e = e.ok()?;
                    let name = e.file_name().into_string().ok()?;
                    let source_name = name.strip_suffix(".db")?.to_string();
                    Some((source_name, e.path()))
                })
                .collect(),
        }
    } else {
        params.source.iter().filter_map(|s| {
            source_db_path(&state, s).ok().map(|p| (s.clone(), p))
        }).collect()
    };

    let data_dir = state.data_dir.clone();
    let offset = params.offset;
    let date_filter = DateFilter { from: params.date_from, to: params.date_to, kinds: params.kinds.into_iter().map(|s| FileKind::from(s.as_str())).collect(), filename_only: false };
    let case_sensitive = params.case_sensitive;

    // Only score enough candidates to fill this page plus a buffer for fuzzy
    // filtering. This avoids reading thousands of ZIP chunks for common queries
    // where the total far exceeds what we show.
    let scoring_limit = (offset + limit + 200).min(fts_limit);

    // Query each source DB in parallel.
    let handles: Vec<_> = source_dbs
        .into_iter()
        .map(|(source_name, db_path)| {
            let query = query.clone();
            let mode = mode.clone();
            let data_dir = data_dir.clone();
            let date_filter = date_filter.clone();
            spawn_blocking(move || -> anyhow::Result<(usize, Vec<SearchResult>)> {
                if !db_path.exists() { return Ok((0, vec![])); }
                let conn = db::open(&db_path)?;
                let archive_mgr = ArchiveManager::new_for_reading(data_dir);

                // Document-family modes: one result per file.
                match mode {
                    SearchMode::Document => {
                        let (doc_total, candidates) = db::document_candidates(&conn, &archive_mgr, &query, scoring_limit, date_filter, case_sensitive)?;
                        let mut scorer = FuzzyScorer::new(&query, case_sensitive);
                        let result_pairs: Vec<ScoredResult> = candidates
                            .into_iter()
                            .map(|DocumentGroup { representative: rep, members: extras }| {
                                let file_id = rep.file_id;
                                let score = scorer.score(&rep.content).unwrap_or(0);
                                let extra_matches = extras.into_iter()
                                    .map(|e| ContextLine { line_number: e.line_number, content: e.content })
                                    .collect();
                                ScoredResult { result: make_result(&source_name, &rep, score, extra_matches), file_id }
                            })
                            .collect();
                        let file_ids: Vec<i64> = result_pairs.iter().map(|sr| sr.file_id).collect();
                        let dups_map = db::fetch_duplicates_for_file_ids(&conn, &file_ids)?;
                        let results: Vec<SearchResult> = result_pairs
                            .into_iter()
                            .map(|mut sr| {
                                if let Some(dups) = dups_map.get(&sr.file_id) { sr.result.duplicate_paths = dups.clone(); }
                                sr.result
                            })
                            .collect();
                        return Ok((doc_total, results));
                    }
                    SearchMode::DocExact => {
                        // Phrase FTS pre-filter → fts_candidates → group by file → exact post-filter.
                        let candidates = db::fts_candidates(&conn, &archive_mgr, &query, scoring_limit, true, date_filter)?;
                        let filtered: Vec<CandidateRow> = candidates.into_iter()
                            .filter(|c| if case_sensitive { c.content.contains(query.as_str()) } else { c.content.to_lowercase().contains(&query.to_lowercase()) })
                            .collect();
                        let source_total = filtered.len();
                        let result_pairs = group_by_file(filtered, &source_name);
                        let file_ids: Vec<i64> = result_pairs.iter().map(|sr| sr.file_id).collect();
                        let dups_map = db::fetch_duplicates_for_file_ids(&conn, &file_ids)?;
                        let results: Vec<SearchResult> = result_pairs
                            .into_iter()
                            .map(|mut sr| {
                                if let Some(dups) = dups_map.get(&sr.file_id) { sr.result.duplicate_paths = dups.clone(); }
                                sr.result
                            })
                            .collect();
                        return Ok((source_total, results));
                    }
                    SearchMode::DocRegex => {
                        // Literal fragments FTS pre-filter → fts_candidates → group by file → regex post-filter.
                        let fts_terms = regex_to_fts_terms(&query);
                        let re = regex::RegexBuilder::new(&query).case_insensitive(!case_sensitive).build()?;
                        let candidates = db::fts_candidates(&conn, &archive_mgr, &fts_terms, scoring_limit, false, date_filter)?;
                        let filtered: Vec<CandidateRow> = candidates.into_iter()
                            .filter(|c| re.is_match(&c.content))
                            .collect();
                        let source_total = filtered.len();
                        let result_pairs = group_by_file(filtered, &source_name);
                        let file_ids: Vec<i64> = result_pairs.iter().map(|sr| sr.file_id).collect();
                        let dups_map = db::fetch_duplicates_for_file_ids(&conn, &file_ids)?;
                        let results: Vec<SearchResult> = result_pairs
                            .into_iter()
                            .map(|mut sr| {
                                if let Some(dups) = dups_map.get(&sr.file_id) { sr.result.duplicate_paths = dups.clone(); }
                                sr.result
                            })
                            .collect();
                        return Ok((source_total, results));
                    }
                    _ => {}
                }

                // Line-family and file-family modes.
                // file-* modes restrict matches to line_number = 0 (filename rows).
                let filename_only = matches!(mode, SearchMode::FileFuzzy | SearchMode::FileExact | SearchMode::FileRegex);
                let date_filter = DateFilter { filename_only, ..date_filter };

                // For regex mode, extract literal character sequences from the pattern
                // for FTS5 pre-filtering, then apply the full regex as a post-filter.
                // For exact mode, treat the whole query as a phrase (literal substring).
                // For fuzzy mode, AND individual words.
                let (fts_phrase, fts_query) = match mode {
                    SearchMode::Fuzzy | SearchMode::FileFuzzy => (false, query.clone()),
                    SearchMode::Regex | SearchMode::FileRegex => (false, regex_to_fts_terms(&query)),
                    _ /* Exact | FileExact */ => (true, query.clone()),
                };

                // Fast count via FTS5 only — no ZIP reads, no JOINs.
                let source_total = db::fts_count(&conn, &fts_query, fts_limit, fts_phrase, date_filter.clone())?;

                // Score only as many candidates as needed for this page.
                let mut candidates = db::fts_candidates(&conn, &archive_mgr, &fts_query, scoring_limit, fts_phrase, date_filter)?;

                // For file-* modes, remove any line_number=0 rows that are not path lines
                // (e.g. EXIF, audio tags, PE metadata). Content is available after ZIP reads.
                if filename_only {
                    candidates.retain(|c| c.content.starts_with("[PATH] "));
                }

                // Build ScoredResult pairs for alias lookup.
                let result_pairs: Vec<ScoredResult> = match mode {
                    SearchMode::Exact | SearchMode::FileExact => {
                        // FTS5 trigram is case-insensitive pre-filter; for case-sensitive mode
                        // add a post-filter to discard candidates that don't literally contain the query.
                        candidates.into_iter()
                            .filter(|c| !case_sensitive || c.content.contains(query.as_str()))
                            .map(|c| ScoredResult { result: make_result(&source_name, &c, 0, vec![]), file_id: c.file_id })
                            .collect()
                    }
                    SearchMode::Regex | SearchMode::FileRegex => {
                        let re = regex::RegexBuilder::new(&query).case_insensitive(!case_sensitive).build()?;
                        candidates.into_iter()
                            .filter(|c| re.is_match(&c.content))
                            .map(|c| ScoredResult { result: make_result(&source_name, &c, 0, vec![]), file_id: c.file_id })
                            .collect()
                    }
                    _ /* Fuzzy | FileFuzzy */ => {
                        let query_terms: Vec<&str> = if case_sensitive {
                            query.split_whitespace().collect()
                        } else {
                            vec![]
                        };
                        let mut scorer = FuzzyScorer::new(&query, case_sensitive);
                        candidates.into_iter()
                            .filter_map(|c| {
                                // In case-sensitive mode, require every query term to appear
                                // as a literal substring. Nucleo's subsequence algorithm can
                                // match scattered lowercase letters inside capitalised words
                                // (e.g. "monhegan" matching "Monhegan" via 'm' in a prior word
                                // + 'onhegan' from the capital-M word), which is not the
                                // user-expected behaviour for case-sensitive search.
                                if !query_terms.is_empty()
                                    && !query_terms.iter().all(|t| c.content.contains(*t))
                                {
                                    return None;
                                }
                                scorer.score(&c.content)
                                    .map(|score| ScoredResult { result: make_result(&source_name, &c, score, vec![]), file_id: c.file_id })
                            })
                            .collect()
                    }
                };

                // Look up duplicates for all file IDs in the result set.
                let file_ids: Vec<i64> = result_pairs.iter().map(|sr| sr.file_id).collect();
                let dups_map = db::fetch_duplicates_for_file_ids(&conn, &file_ids)?;

                let results: Vec<SearchResult> = result_pairs
                    .into_iter()
                    .map(|mut sr| {
                        if let Some(dups) = dups_map.get(&sr.file_id) {
                            sr.result.duplicate_paths = dups.clone();
                        }
                        sr.result
                    })
                    .collect();

                Ok((source_total, results))
            })
        })
        .collect();

    let mut all_results: Vec<SearchResult> = Vec::new();
    for handle in handles {
        match handle.await.unwrap_or_else(|e| Err(anyhow::anyhow!(e))) {
            Ok((_source_total, mut r)) => {
                all_results.append(&mut r);
            }
            Err(e) => tracing::warn!("search source error: {e:#}"),
        }
    }

    all_results.sort_by(|a, b| b.score.cmp(&a.score));

    // Deduplicate by (source, path, archive_path, line_number), keeping the
    // highest-scoring occurrence (first after sort). Duplicates arise when FTS5
    // returns multiple rows for the same logical match (e.g. two members of the
    // same archive that share a line number after composite-path splitting).
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<_> = all_results
        .into_iter()
        .filter(|r| seen.insert((r.source.clone(), r.path.clone(), r.archive_path.clone(), r.line_number)))
        .collect();

    let unique_total = unique.len();
    let results: Vec<_> = unique.into_iter().skip(offset).take(limit).collect();

    Json(SearchResponse { results, total: unique_total }).into_response()
}
