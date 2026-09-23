use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use rusqlite::OptionalExtension;

use find_common::api::{CreateLinkRequest, CreateLinkResponse, ResolveLinkResponse};

use crate::{db, AppState};

use super::{check_scope, composite_path, run_blocking, source_db_path};
use find_common::api::Scope;

const RATE_LIMIT_REQUESTS: u32 = 60;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// POST /api/v1/links — create a share link for a file.
/// Requires bearer auth. Looks up kind + mtime from the source DB, generates a
/// unique 6-char code, and inserts a row in links.db.
pub async fn post_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateLinkRequest>,
) -> impl IntoResponse {
    if let Err(s) = check_scope(&state, &headers, Scope::Read) {
        return (s, Json(serde_json::Value::Null)).into_response();
    }

    let db_path = match source_db_path(&state, &body.source) {
        Ok(p) => p,
        Err(s) => return (s, Json(serde_json::Value::Null)).into_response(),
    };

    let now = unix_now();
    let expires_at = match body.expires_in_secs {
        Some(0) => i64::MAX,                                  // never expires
        Some(secs) if secs > 0 => now + secs,
        _ => now + state.config.links.ttl_secs as i64,        // server default
    };
    let data_dir = state.data_dir.clone();
    let source = body.source.clone();
    let path = body.path.clone();
    let archive_path = body.archive_path.clone();

    run_blocking("post_link", move || {
        let full_path = composite_path(&path, archive_path.as_deref());
        let source_conn = db::open(&db_path)?;
        let (kind_str, mtime): (String, i64) = source_conn
            .query_row(
                "SELECT kind, mtime FROM files WHERE path = ?1",
                rusqlite::params![full_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|_| ("text".to_string(), 0));

        let links_conn = db::links::open_links_db(&data_dir)?;

        let code = loop {
            let candidate = gen_code();
            let exists: bool = links_conn
                .query_row(
                    "SELECT 1 FROM links WHERE code = ?1",
                    rusqlite::params![candidate],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !exists {
                break candidate;
            }
        };

        db::links::create_link(&links_conn, &db::links::CreateLinkArgs {
            code: &code,
            source: &source,
            path: &path,
            archive_path: archive_path.as_deref(),
            kind: &kind_str,
            mtime,
            created_at: now,
            expires_at,
        })?;

        let url = format!("/v/{code}");
        Ok((
            StatusCode::CREATED,
            Json(CreateLinkResponse { code, url, expires_at }),
        ))
    })
    .await
}

/// GET /api/v1/links/:code — resolve a share link.
/// No auth required. Rate-limited to 60 req/min per IP.
pub async fn get_link(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(code): Path<String>,
) -> Response {
    // Rate limit by IP.
    {
        let ip = addr.ip();
        let mut limiter = state
            .link_rate_limiter
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let now = std::time::Instant::now();
        let entry = limiter.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1).as_secs() >= RATE_LIMIT_WINDOW_SECS {
            *entry = (1, now);
        } else {
            entry.0 += 1;
            if entry.0 > RATE_LIMIT_REQUESTS {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            }
        }
    }

    let data_dir = state.data_dir.clone();

    run_blocking("get_link", move || {
        use db::links::{resolve_link, ResolveResult};
        let links_conn = db::links::open_links_db(&data_dir)?;

        Ok(match resolve_link(&links_conn, &code)? {
            ResolveResult::NotFound => StatusCode::NOT_FOUND.into_response(),
            ResolveResult::Expired => StatusCode::GONE.into_response(),
            ResolveResult::Found(row) => {
                let filename = link_basename(&row.path, row.archive_path.as_deref());
                Json(ResolveLinkResponse {
                    source: row.source,
                    path: row.path,
                    archive_path: row.archive_path,
                    kind: row.kind,
                    filename,
                    mtime: row.mtime,
                    expires_at: row.expires_at,
                })
                .into_response()
            }
        })
    })
    .await
}

fn gen_code() -> String {
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    bytes[..6]
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Return the display filename for a link: last path component of the inner
/// path if present, otherwise the outer path.
fn link_basename(path: &str, archive_path: Option<&str>) -> String {
    let p = archive_path.unwrap_or(path);
    p.split('/').next_back().unwrap_or(p).to_string()
}
