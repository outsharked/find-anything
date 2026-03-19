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
use crate::{db, db::search::{CandidateRow, DocumentGroup}, db::DateFilter, AppState};

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

    let content_store = Arc::clone(&state.content_store);
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
            let cs = Arc::clone(&content_store);
            let date_filter = date_filter.clone();
            spawn_blocking(move || -> anyhow::Result<(usize, Vec<SearchResult>)> {
                if !db_path.exists() { return Ok((0, vec![])); }
                let conn = db::open(&db_path)?;

                // Document-family modes: one result per file.
                match mode {
                    SearchMode::Document => {
                        let (doc_total, candidates) = db::document_candidates(&conn, &query, scoring_limit, date_filter)?;
                        let mut scorer = FuzzyScorer::new(&query, case_sensitive);
                        let result_pairs: Vec<ScoredResult> = candidates
                            .into_iter()
                            .map(|DocumentGroup { representative: rep, members: extras }| {
                                let file_id = rep.file_id;
                                let score = scorer.score(&rep.content).unwrap_or(1);
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
                        // Phrase FTS pre-filter → fts_candidates → group by file.
                        // FTS phrase match is sufficient; no content post-filter needed.
                        let candidates = db::fts_candidates(&conn, &query, scoring_limit, true, date_filter)?;
                        let source_total = candidates.len();
                        let result_pairs = group_by_file(candidates, &source_name);
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
                        // DocRegex: literal fragments FTS pre-filter at the file level,
                        // then apply the regex to the full joined document text so that
                        // patterns like `.*UART.*updates.*` can span multiple lines.
                        let fts_terms = regex_to_fts_terms(&query);
                        let re = regex::RegexBuilder::new(&query)
                            .case_insensitive(!case_sensitive)
                            .dot_matches_new_line(true)
                            .build()?;
                        // Use document_candidates so the FTS pre-filter intersects per-token
                        // file sets — a file qualifies if each literal term appears *somewhere*
                        // in it (not necessarily on the same line).
                        let (_, doc_groups) = db::document_candidates(&conn, &fts_terms, scoring_limit, date_filter)?;
                        let mut result_pairs: Vec<ScoredResult> = Vec::new();
                        for group in doc_groups {
                            let file_id = group.representative.file_id;
                            let doc_text = db::read_file_document(&conn, cs.as_ref(), file_id);
                            if re.is_match(&doc_text) {
                                let mut rep = group.representative;
                                // Find the line where the first match starts for the snippet.
                                if let Some(m) = re.find(&doc_text) {
                                    let line_idx = doc_text[..m.start()].chars().filter(|&c| c == '\n').count();
                                    let matched_line = doc_text.lines().nth(line_idx).unwrap_or("").to_string();
                                    rep.content = matched_line;
                                    // line_number is 0-based in the inline doc; add 1 for display
                                    // (keeps consistent with the 1-based FTS line numbering).
                                    rep.line_number = line_idx + 1;
                                }
                                result_pairs.push(ScoredResult { result: make_result(&source_name, &rep, 0, vec![]), file_id });
                            }
                        }
                        let source_total = result_pairs.len();
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

                // Score only as many candidates as needed for this page.
                let mut candidates = db::fts_candidates(&conn, &fts_query, scoring_limit, fts_phrase, date_filter)?;

                // For file-* modes, restrict to line_number == 0 (filename rows).
                // The FTS SQL already enforces this via SQL_FTS_FILENAME_ONLY; this is a safety check.
                if filename_only {
                    candidates.retain(|c| c.line_number == 0);
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
                        // Read content for regex post-filtering (ZIP reads needed for correctness).
                        let pairs: Vec<(i64, i64)> = candidates.iter().map(|c| (c.file_id, c.line_number as i64)).collect();
                        let content_map = db::read_content_batch(&conn, cs.as_ref(), &pairs);
                        candidates.into_iter()
                            .filter_map(|mut c| {
                                let content = content_map.get(&(c.file_id, c.line_number as i64)).cloned().unwrap_or_default();
                                // For filename-only regex: match against the file path.
                                let text = if filename_only { c.file_path.as_str() } else { content.as_str() };
                                if re.is_match(text) { c.content = content; Some(c) } else { None }
                            })
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
                                // After plan 080, content is not populated for non-regex modes.
                                // For FileFuzzy (filename search): score against the file path.
                                // For Fuzzy (content search): FTS already validated the match;
                                //   score against the path for ranking, or accept with score=1.
                                let score_text: &str = if !c.content.is_empty() {
                                    &c.content
                                } else if filename_only {
                                    // FileFuzzy: path is the right thing to score against.
                                    &c.file_path
                                } else {
                                    // Fuzzy content search: FTS validated match; score by path
                                    // for relative ranking (files whose path matches score higher).
                                    &c.file_path
                                };
                                // In case-sensitive mode, require every query term to appear
                                // as a literal substring.
                                if !query_terms.is_empty()
                                    && !query_terms.iter().all(|t| c.content.contains(*t) || c.file_path.contains(*t))
                                {
                                    return None;
                                }
                                let score = if filename_only || !c.content.is_empty() {
                                    // Use real fuzzy score when content is available or for filename search.
                                    scorer.score(score_text)?
                                } else {
                                    // Content search without content: FTS validated it, use path score
                                    // or default score=1 so all FTS matches are included.
                                    scorer.score(score_text).unwrap_or(1)
                                };
                                Some(ScoredResult { result: make_result(&source_name, &c, score, vec![]), file_id: c.file_id })
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

                Ok((results.len(), results))
            })
        })
        .collect();

    let mut all_results: Vec<SearchResult> = Vec::new();
    for handle in handles {
        match handle.await.unwrap_or_else(|e| Err(anyhow::anyhow!(e))) {
            Ok((_source_total, mut r)) => {
                all_results.append(&mut r);
            }
            Err(e) => tracing::error!("search source error: {e:#}"),
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
