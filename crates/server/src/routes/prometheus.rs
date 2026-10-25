/// Prometheus exposition (`GET /api/v1/metrics/prometheus`) and the hot-files
/// view (`GET /api/v1/hot-files`) that complements it.
///
/// File paths are deliberately never metric labels (unbounded cardinality);
/// they are only available from `hot-files`, which the
/// `find_anything_hot_files` gauge counts.
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use tokio::task::spawn_blocking;

use find_common::api::{HotFile, HotFilesResponse, Scope};

use crate::stats_cache::ChangeCounts;
use crate::{db, AppState};

use super::check_scope;

/// Window and floor behind the `find_anything_hot_files` gauge.
const GAUGE_WINDOW_SECS: i64 = 24 * 3600;
const GAUGE_MIN_CHANGES: i64 = 5;

/// Defaults and bounds for `GET /api/v1/hot-files`.
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 1000;
const MAX_WINDOW_SECS: i64 = db::stats::FILE_CHANGE_RETENTION_HOURS * 3600;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn source_dbs(state: &AppState) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(state.data_dir.join("sources")) else { return vec![] };
    let mut v: Vec<_> = rd
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().into_string().ok()?;
            Some((name.strip_suffix(".db")?.to_string(), e.path()))
        })
        .collect();
    v.sort();
    v
}

// ── Prometheus text ───────────────────────────────────────────────────────────

pub(crate) struct SourceSnapshot {
    pub name:          String,
    pub files:         usize,
    pub indexed_bytes: i64,
    pub db_bytes:      u64,
    pub last_scan:     Option<i64>,
    pub errors:        usize,
    pub hot_files:     usize,
    pub changes:       ChangeCounts,
}

pub(crate) struct Snapshot {
    pub sources:             Vec<SourceSnapshot>,
    pub inbox_queue_depth:   usize,
    pub failed_requests:     usize,
    pub content_store_bytes: u64,
}

fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

pub(crate) fn render(s: &Snapshot) -> String {
    let mut out = String::new();

    family(&mut out, "find_anything_files_changed_total", "counter",
        "Outer files added, modified or deleted since the server started.");
    for src in &s.sources {
        let n = escape_label(&src.name);
        for (op, v) in [("add", src.changes.added), ("modify", src.changes.modified), ("delete", src.changes.deleted)] {
            let _ = writeln!(out, "find_anything_files_changed_total{{source=\"{n}\",op=\"{op}\"}} {v}");
        }
    }

    let gauge = |out: &mut String, name: &str, help: &str, f: &dyn Fn(&SourceSnapshot) -> Option<String>| {
        family(out, name, "gauge", help);
        for src in &s.sources {
            if let Some(v) = f(src) {
                let _ = writeln!(out, "{name}{{source=\"{}\"}} {v}", escape_label(&src.name));
            }
        }
    };
    gauge(&mut out, "find_anything_files", "Files currently indexed.",
        &|x| Some(x.files.to_string()));
    gauge(&mut out, "find_anything_indexed_bytes", "Total size of the files currently indexed.",
        &|x| Some(x.indexed_bytes.to_string()));
    gauge(&mut out, "find_anything_db_size_bytes", "On-disk size of the source database.",
        &|x| Some(x.db_bytes.to_string()));
    gauge(&mut out, "find_anything_last_scan_timestamp_seconds", "Unix time of the last completed scan.",
        &|x| x.last_scan.map(|t| t.to_string()));
    gauge(&mut out, "find_anything_indexing_errors", "Files currently failing to index.",
        &|x| Some(x.errors.to_string()));
    gauge(&mut out, "find_anything_hot_files",
        "Files changed at least 5 times in the last 24h; see GET /api/v1/hot-files.",
        &|x| Some(x.hot_files.to_string()));

    for (name, help, v) in [
        ("find_anything_inbox_queue_depth", "Requests waiting to be indexed.", s.inbox_queue_depth as u64),
        ("find_anything_failed_requests", "Requests that failed and were set aside.", s.failed_requests as u64),
        ("find_anything_content_store_bytes", "On-disk size of the content store.", s.content_store_bytes),
    ] {
        family(&mut out, name, "gauge", help);
        let _ = writeln!(out, "{name} {v}");
    }
    out
}

// ── GET /api/v1/metrics/prometheus ────────────────────────────────────────────

pub async fn get_prometheus_metrics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_scope(&state, &headers, Scope::Read) {
        return s.into_response();
    }

    let cached: Vec<crate::stats_cache::CachedSourceStats> = {
        let g = state.source_stats_cache.read().unwrap_or_else(|e| e.into_inner());
        g.sources.clone()
    };
    let changes = {
        let g = state.source_stats_cache.read().unwrap_or_else(|e| e.into_inner());
        g.changes.clone()
    };
    let data_dir = state.data_dir.clone();
    let content_store = Arc::clone(&state.content_store);

    let snapshot = spawn_blocking(move || {
        let count_gz = |dir: PathBuf| -> usize {
            std::fs::read_dir(dir)
                .map(|rd| rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|x| x == "gz"))
                    .count())
                .unwrap_or(0)
        };
        let now = now_secs();
        let sources = cached.into_iter().map(|c| {
            let db_path = data_dir.join("sources").join(format!("{}.db", c.name));
            let db_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
            let (last_scan, errors, hot_files) = match db::open(&db_path) {
                Ok(conn) => (
                    db::get_last_scan(&conn).unwrap_or(None),
                    db::get_indexing_error_count(&conn).unwrap_or(0),
                    db::count_hot_files(&conn, now, GAUGE_WINDOW_SECS, GAUGE_MIN_CHANGES).unwrap_or(0),
                ),
                Err(e) => {
                    tracing::warn!("prometheus metrics: cannot open {}: {e:#}", c.name);
                    (None, 0, 0)
                }
            };
            SourceSnapshot {
                changes: changes.get(&c.name).copied().unwrap_or_default(),
                name: c.name,
                files: c.total_files,
                indexed_bytes: c.total_size,
                db_bytes,
                last_scan,
                errors,
                hot_files,
            }
        }).collect();
        let inbox = data_dir.join("inbox");
        Snapshot {
            sources,
            inbox_queue_depth: count_gz(inbox.clone()),
            failed_requests: count_gz(inbox.join("failed")),
            content_store_bytes: content_store.storage_stats().map(|(_, b)| b).unwrap_or(0),
        }
    }).await;

    match snapshot {
        Ok(s) => (
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
            render(&s),
        ).into_response(),
        Err(e) => {
            tracing::warn!("prometheus metrics: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── GET /api/v1/hot-files ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct HotFilesQuery {
    /// Restrict to one source; default all.
    source: Option<String>,
    /// e.g. `90m`, `24h` (default), `7d`; a bare number is seconds.
    window: Option<String>,
    /// Only files with at least this many changes (default 5).
    min: Option<i64>,
    limit: Option<usize>,
}

/// Parse `90m` / `24h` / `7d` / `3600s` / `3600` into seconds.
fn parse_window(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        'd' => (&s[..s.len() - 1], 86400),
        's' => (&s[..s.len() - 1], 1),
        _   => (s, 1),
    };
    let n: i64 = num.parse().ok()?;
    (n > 0).then(|| n.checked_mul(mult)).flatten()
}

pub async fn get_hot_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<HotFilesQuery>,
) -> impl IntoResponse {
    if let Err(s) = check_scope(&state, &headers, Scope::Read) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }
    let bad = |msg: String| {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": msg }))).into_response()
    };

    let window_secs = match q.window.as_deref() {
        None => GAUGE_WINDOW_SECS,
        Some(w) => match parse_window(w) {
            Some(s) if s <= MAX_WINDOW_SECS => s,
            Some(_) => return bad(format!("window exceeds the {}h retention", MAX_WINDOW_SECS / 3600)),
            None => return bad(format!("invalid window {w:?}; use e.g. 90m, 24h, 7d")),
        },
    };
    let min_changes = q.min.unwrap_or(GAUGE_MIN_CHANGES).max(1);
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if limit > MAX_LIMIT {
        return bad(format!("limit exceeds maximum of {MAX_LIMIT}"));
    }

    let dbs: Vec<_> = source_dbs(&state)
        .into_iter()
        .filter(|(name, _)| q.source.as_ref().is_none_or(|s| s == name))
        .collect();
    let now = now_secs();

    let handles: Vec<_> = dbs.into_iter().map(|(source, path)| {
        spawn_blocking(move || -> anyhow::Result<Vec<HotFile>> {
            let conn = db::open(&path)?;
            Ok(db::hot_files(&conn, now, window_secs, min_changes, limit)?
                .into_iter()
                .map(|r| HotFile { source: source.clone(), path: r.path, changes: r.changes })
                .collect())
        })
    }).collect();

    let mut files: Vec<HotFile> = Vec::new();
    for h in handles {
        match h.await.unwrap_or_else(|e| Err(anyhow::anyhow!(e))) {
            Ok(v) => files.extend(v),
            Err(e) => tracing::warn!("hot-files error: {e:#}"),
        }
    }
    files.sort_by(|a, b| b.changes.cmp(&a.changes).then_with(|| a.path.cmp(&b.path)));
    files.truncate(limit);

    Json(HotFilesResponse { window_secs, min_changes, files }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_window_units() {
        assert_eq!(parse_window("90m"), Some(5400));
        assert_eq!(parse_window("24h"), Some(86400));
        assert_eq!(parse_window("7d"), Some(604800));
        assert_eq!(parse_window("3600s"), Some(3600));
        assert_eq!(parse_window("3600"), Some(3600));
        assert_eq!(parse_window("0h"), None);
        assert_eq!(parse_window("-5m"), None);
        assert_eq!(parse_window("abc"), None);
        assert_eq!(parse_window(""), None);
    }

    #[test]
    fn render_emits_counters_gauges_and_escapes_labels() {
        let snap = Snapshot {
            sources: vec![SourceSnapshot {
                name: "we\"ird\\src".into(),
                files: 3, indexed_bytes: 100, db_bytes: 4096,
                last_scan: Some(1700000000), errors: 2, hot_files: 1,
                changes: ChangeCounts { added: 5, modified: 6, deleted: 7 },
            }, SourceSnapshot {
                name: "never".into(),
                files: 0, indexed_bytes: 0, db_bytes: 0,
                last_scan: None, errors: 0, hot_files: 0,
                changes: ChangeCounts::default(),
            }],
            inbox_queue_depth: 4, failed_requests: 1, content_store_bytes: 999,
        };
        let out = render(&snap);
        assert!(out.contains("# TYPE find_anything_files_changed_total counter"));
        assert!(out.contains(r#"find_anything_files_changed_total{source="we\"ird\\src",op="modify"} 6"#), "{out}");
        assert!(out.contains(r#"find_anything_files{source="never"} 0"#));
        assert!(out.contains(r#"find_anything_last_scan_timestamp_seconds{source="we\"ird\\src"} 1700000000"#));
        // A source that never scanned has no last-scan sample rather than a fake 0.
        assert!(!out.contains(r#"last_scan_timestamp_seconds{source="never"}"#));
        assert!(out.contains("find_anything_inbox_queue_depth 4"));
        assert!(out.contains("find_anything_content_store_bytes 999"));
    }
}
