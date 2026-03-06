use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, State},
    http::{request::Parts, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use tokio::task::spawn_blocking;

use find_common::api::{ContextLine, SearchResponse, SearchResult};

use crate::fuzzy::FuzzyScorer;
use crate::{archive::ArchiveManager, db, db::search::CandidateRow, db::DateFilter, AppState};

use super::{check_auth, source_db_path};

// ── GET /api/v1/search ────────────────────────────────────────────────────────

pub struct SearchParams {
    pub q: String,
    pub mode: String,
    /// Collected from repeated ?source=a&source=b params.
    pub source: Vec<String>,
    pub limit: usize,
    pub offset: usize,
    /// Optional unix timestamp bounds for mtime filtering.
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
}

impl<S: Send + Sync> FromRequestParts<S> for SearchParams {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let raw = parts.uri.query().unwrap_or("");
        let mut q = None;
        let mut mode = None;
        let mut source = Vec::new();
        let mut limit = None;
        let mut offset = None;
        let mut date_from = None;
        let mut date_to = None;

        for (k, v) in form_urlencoded::parse(raw.as_bytes()) {
            match k.as_ref() {
                "q"         => q         = Some(v.into_owned()),
                "mode"      => mode      = Some(v.into_owned()),
                "source"    => source.push(v.into_owned()),
                "limit"     => limit     = Some(v.parse::<usize>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid limit".to_string()))?),
                "offset"    => offset    = Some(v.parse::<usize>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid offset".to_string()))?),
                "date_from" => date_from = Some(v.parse::<i64>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid date_from".to_string()))?),
                "date_to"   => date_to   = Some(v.parse::<i64>()
                    .map_err(|_| (StatusCode::BAD_REQUEST, "invalid date_to".to_string()))?),
                _ => {}
            }
        }

        Ok(SearchParams {
            q:         q.ok_or_else(|| (StatusCode::BAD_REQUEST, "missing 'q'".to_string()))?,
            mode:      mode.unwrap_or_else(|| "fuzzy".to_string()),
            source,
            limit:     limit.unwrap_or(50),
            offset:    offset.unwrap_or(0),
            date_from,
            date_to,
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

fn make_result(
    source: &str,
    c: &CandidateRow,
    score: u32,
    extra_matches: Vec<ContextLine>,
) -> SearchResult {
    SearchResult {
        source: source.to_string(),
        path: c.file_path.clone(),
        archive_path: c.archive_path.clone(),
        line_number: c.line_number,
        snippet: c.content.clone(),
        score,
        kind: c.file_kind.clone(),
        mtime: c.mtime,
        size: c.size,
        context_lines: vec![],
        aliases: vec![],
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
    let mode = params.mode.clone();
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
    let date_filter = DateFilter { from: params.date_from, to: params.date_to };

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
            spawn_blocking(move || -> anyhow::Result<(usize, Vec<SearchResult>)> {
                if !db_path.exists() { return Ok((0, vec![])); }
                let conn = db::open(&db_path)?;
                let archive_mgr = ArchiveManager::new(data_dir);

                // Document mode has its own query path (one result per file).
                if mode == "document" {
                    let (doc_total, candidates) = db::document_candidates(&conn, &archive_mgr, &query, scoring_limit, date_filter)?;
                    let mut scorer = FuzzyScorer::new(&query);
                    let result_pairs: Vec<(SearchResult, i64)> = candidates
                        .into_iter()
                        .map(|(rep, extras)| {
                            let file_id = rep.file_id;
                            let score = scorer.score(&rep.content).unwrap_or(0);
                            let extra_matches = extras.into_iter()
                                .map(|e| ContextLine { line_number: e.line_number, content: e.content })
                                .collect();
                            (make_result(&source_name, &rep, score, extra_matches), file_id)
                        })
                        .collect();
                    let canonical_ids: Vec<i64> = result_pairs.iter().map(|(_, id)| *id).collect();
                    let aliases_map = db::fetch_aliases_for_canonical_ids(&conn, &canonical_ids)?;
                    let results: Vec<SearchResult> = result_pairs
                        .into_iter()
                        .map(|(mut r, id)| {
                            if let Some(aliases) = aliases_map.get(&id) { r.aliases = aliases.clone(); }
                            r
                        })
                        .collect();
                    return Ok((doc_total, results));
                }

                // For regex mode, extract literal character sequences from the pattern
                // for FTS5 pre-filtering, then apply the full regex as a post-filter.
                // For exact mode, treat the whole query as a phrase (literal substring).
                // For fuzzy mode, AND individual words.
                let (fts_phrase, fts_query) = match mode.as_str() {
                    "fuzzy" => (false, query.clone()),
                    "regex" => (false, regex_to_fts_terms(&query)),
                    _ /* "exact" */ => (true, query.clone()),
                };

                // Fast count via FTS5 only — no ZIP reads, no JOINs.
                let source_total = db::fts_count(&conn, &fts_query, fts_limit, fts_phrase, date_filter)?;

                // Score only as many candidates as needed for this page.
                let candidates = db::fts_candidates(&conn, &archive_mgr, &fts_query, scoring_limit, fts_phrase, date_filter)?;

                // Build (SearchResult, file_id) pairs for alias lookup.
                let result_pairs: Vec<(SearchResult, i64)> = match mode.as_str() {
                    "exact" => {
                        // FTS5 trigram is already a substring match — candidates are the answer.
                        candidates.into_iter()
                            .map(|c| (make_result(&source_name, &c, 0, vec![]), c.file_id))
                            .collect()
                    }
                    "regex" => {
                        let re = regex::RegexBuilder::new(&query).case_insensitive(true).build()?;
                        candidates.into_iter()
                            .filter(|c| re.is_match(&c.content))
                            .map(|c| (make_result(&source_name, &c, 0, vec![]), c.file_id))
                            .collect()
                    }
                    _ /* "fuzzy" */ => {
                        let mut scorer = FuzzyScorer::new(&query);
                        candidates.into_iter()
                            .filter_map(|c| {
                                scorer.score(&c.content)
                                    .map(|score| (make_result(&source_name, &c, score, vec![]), c.file_id))
                            })
                            .collect()
                    }
                };

                // Look up aliases for all canonical file IDs in the result set.
                let canonical_ids: Vec<i64> = result_pairs.iter().map(|(_, id)| *id).collect();
                let aliases_map = db::fetch_aliases_for_canonical_ids(&conn, &canonical_ids)?;

                let results: Vec<SearchResult> = result_pairs
                    .into_iter()
                    .map(|(mut r, id)| {
                        if let Some(aliases) = aliases_map.get(&id) {
                            r.aliases = aliases.clone();
                        }
                        r
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
