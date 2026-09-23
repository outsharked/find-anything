use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::AppState;

use super::check_scope;
use find_common::api::Scope;

// ── POST /api/v1/bulk ─────────────────────────────────────────────────────────

/// Process-wide monotonic counter, embedded in every inbox filename so the
/// router can order requests by true arrival order (see `next_request_id`).
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build a unique, orderable inbox request id.
///
/// The timestamp prefix is for humans (`find-admin inbox`, log lines); it's
/// only second-granularity, so two requests arriving in the same second are
/// indistinguishable by it alone. The zero-padded sequence number is the
/// actual ordering guarantee: it's assigned in this call, in request-arrival
/// order, so sorting inbox filenames lexicographically (as the router does)
/// reproduces arrival order exactly — unlike sorting by filesystem mtime,
/// which can tie or even reorder for requests written within the same
/// mtime-granularity window (see the `worker::mod` router).
fn next_request_id() -> String {
    let seq = REQUEST_SEQ.fetch_add(1, AtomicOrdering::Relaxed);
    format!(
        "req_{}_{seq:020}_{}",
        chrono::Utc::now().format("%Y%m%d_%H%M%S"),
        uuid::Uuid::new_v4().simple()
    )
}

pub async fn bulk(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(s) = check_scope(&state, &headers, Scope::UpdateIndex) { return s.into_response(); }

    let is_gzip = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "gzip")
        .unwrap_or(false);

    if !is_gzip {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }

    let request_id = next_request_id();

    let inbox_path = state.data_dir.join("inbox").join(format!("{request_id}.gz"));

    match tokio::fs::write(&inbox_path, &body).await {
        Ok(()) => {
            tracing::debug!("Queued bulk request: {}", inbox_path.display());
            StatusCode::ACCEPTED.into_response()
        }
        Err(e) => {
            tracing::error!("Failed to write inbox request: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the mtime-ordering race (burst_coalescing.rs's
    /// `burst_with_interleaved_delete_applies_in_order`): consecutive calls
    /// must produce filenames that sort in call order, even when the
    /// second-granularity timestamp prefix is identical and the UUID suffix
    /// is random — the zero-padded sequence number must be what decides it.
    #[test]
    fn consecutive_ids_sort_in_call_order() {
        let ids: Vec<String> = (0..200).map(|_| next_request_id()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "request ids must already be in sorted (arrival) order");
    }

    /// The sequence number must be fixed-width so lexicographic comparison
    /// matches numeric comparison up to (and past) the point this test's
    /// sibling exercises — spot-check the width directly rather than relying
    /// only on the black-box ordering property above.
    #[test]
    fn sequence_segment_is_fixed_width() {
        let id = next_request_id();
        // id = "req_{YYYYMMDD}_{HHMMSS}_{seq}_{uuid}"
        let rest = id.strip_prefix("req_").unwrap();
        let segs: Vec<&str> = rest.splitn(4, '_').collect();
        let seq_str = segs[2];
        assert_eq!(seq_str.len(), 20, "sequence segment must be zero-padded to a fixed width: {id}");
        assert!(seq_str.chars().all(|c| c.is_ascii_digit()), "sequence segment must be all digits: {id}");
    }
}
