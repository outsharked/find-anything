use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use find_common::api::FileResponse;
use find_common::path::split_composite;

use crate::{archive::ArchiveManager, db, AppState};

use super::{check_auth, check_link_code_auth, composite_path, run_blocking, source_db_path};

// ── GET /api/v1/file?source=X&path=Y[&archive_path=Z][&link_code=C] ──────────
//
// `path` may be a composite path ("archive.zip::member.txt") or, for backward
// compatibility, a plain path with `archive_path` supplied separately.
// `link_code` is an alternative credential (no bearer auth required when set).

#[derive(Deserialize)]
pub struct FileParams {
    pub source: String,
    pub path: String,
    /// Legacy: combine with `path` into a composite path if provided.
    pub archive_path: Option<String>,
    /// Optional share link code as an alternative to bearer authentication.
    pub link_code: Option<String>,
    /// 0-based index of the first content line to return (pagination).
    pub offset: Option<usize>,
    /// Maximum number of content lines to return (pagination).
    pub limit: Option<usize>,
}

pub async fn get_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<FileParams>,
) -> impl IntoResponse {
    if params.link_code.is_none() {
        if let Err(s) = check_auth(&state, &headers) {
            return (s, Json(serde_json::Value::Null)).into_response();
        }
    }

    let db_path = match source_db_path(&state, &params.source) {
        Ok(p) => p,
        Err(s) => return (s, Json(serde_json::Value::Null)).into_response(),
    };

    // Build composite path from path + optional archive_path (backward compat).
    let full_path = composite_path(&params.path, params.archive_path.as_deref());
    let data_dir = state.data_dir.clone();
    let link_code = params.link_code.clone();
    let source = params.source.clone();
    let offset = params.offset.unwrap_or(0);
    let limit = params.limit;

    run_blocking("get_file", move || -> anyhow::Result<Response> {
        // Validate link code if provided (alternative to bearer auth).
        if let Some(code) = &link_code {
            if let Err(s) = check_link_code_auth(&data_dir, code, &source, &full_path) {
                return Ok((s, Json(serde_json::Value::Null)).into_response());
            }
        }

        let conn = db::open(&db_path)?;
        let archive_mgr = ArchiveManager::new_for_reading(data_dir);

        let (kind, mtime, size): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT kind, mtime, size FROM files WHERE path = ?1",
                rusqlite::params![full_path],
                |row| Ok((row.get(0)?, row.get(1).ok(), row.get(2).ok())),
            )
            .unwrap_or_else(|_| ("text".into(), None, None));

        let (all_lines, total_lines, content_unavailable) =
            db::get_file_lines_paged(&conn, &archive_mgr, &full_path, offset, limit)?;

        let metadata: Vec<String> = all_lines.iter()
            .filter(|l| l.line_number == 0)
            .map(|l| l.content.strip_prefix("[PATH] ").map(|s| s.to_string()).unwrap_or_else(|| l.content.clone()))
            .collect();

        let content_lines: Vec<_> = all_lines.into_iter()
            .filter(|l| l.line_number > 0)
            .collect();

        // Only emit line_offsets when lines aren't a contiguous 1-based sequence.
        let is_sequential = content_lines.iter().enumerate()
            .all(|(i, l)| l.line_number == i + 1);
        let line_offsets: Vec<usize> = if is_sequential {
            vec![]
        } else {
            content_lines.iter().map(|l| l.line_number).collect()
        };

        let lines: Vec<String> = content_lines.into_iter().map(|l| l.content).collect();

        // For archive members (path contains "::"), fall back to the outer archive's
        // error if no per-member error was recorded.
        let indexing_error = db::get_indexing_error(&conn, &full_path)?.or_else(|| {
            let (outer, _) = split_composite(&full_path)?;
            db::get_indexing_error(&conn, outer).ok().flatten()
        });
        Ok(Json(FileResponse {
            lines, line_offsets, metadata,
            file_kind: kind, total_lines, mtime, size,
            indexing_error, content_unavailable,
        }).into_response())
    }).await
}


// ── GET /api/v1/files?source=<name>[&q=<query>&limit=<n>] ────────────────────
//
// Without `q`: returns the full file list (used by find-scan for deletion detection).
// With `q`: returns up to `limit` (default 50) matching files for the Ctrl+P palette.

#[derive(Deserialize)]
pub struct FilesParams {
    pub source: String,
    /// Search query for palette mode. When present, returns up to `limit` matches.
    pub q: Option<String>,
    /// Maximum results for palette mode (default 50).
    pub limit: Option<usize>,
}

pub async fn list_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<FilesParams>,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) { return (s, Json(serde_json::Value::Null)).into_response(); }

    let db_path = match source_db_path(&state, &params.source) {
        Ok(p) => p,
        Err(s) => return (s, Json(serde_json::Value::Null)).into_response(),
    };

    let q = params.q.clone();
    let limit = params.limit.unwrap_or(50);

    run_blocking("list_files", move || {
        let conn = db::open(&db_path)?;
        match q {
            Some(q) => db::search_files(&conn, &q, limit).map(Json),
            None    => db::list_files(&conn).map(Json),
        }
    }).await
}
