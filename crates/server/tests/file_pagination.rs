mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::FileResponse;

/// Build a multi-line file with N numbered content lines and index it.
/// Content lines are "line 1", "line 2", ..., "line N".
async fn index_numbered_file(srv: &TestServer, source: &str, path: &str, n: usize) {
    let content = (1..=n)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let req = make_text_bulk(source, path, &content);
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;
}

async fn get_file(srv: &TestServer, source: &str, path: &str) -> FileResponse {
    srv.client
        .get(srv.url(&format!("/api/v1/file?source={source}&path={path}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn get_file_paged(
    srv: &TestServer,
    source: &str,
    path: &str,
    offset: usize,
    limit: usize,
) -> FileResponse {
    srv.client
        .get(srv.url(&format!(
            "/api/v1/file?source={source}&path={path}&offset={offset}&limit={limit}"
        )))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

// ── Backward compatibility ────────────────────────────────────────────────────

/// No offset/limit → returns all lines, total_lines == lines.len().
#[tokio::test]
async fn test_no_pagination_params_returns_all() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "big.txt", 10).await;

    let resp = get_file(&srv, "docs", "big.txt").await;

    assert_eq!(resp.lines.len(), 10);
    assert_eq!(resp.total_lines, 10);
    assert!(resp.line_offsets.is_empty(), "sequential page should have no line_offsets");
}

// ── total_lines reflects true count ──────────────────────────────────────────

/// total_lines must reflect the full file even when only a page is returned.
#[tokio::test]
async fn test_total_lines_reflects_full_count_not_page_length() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "large.txt", 20).await;

    let resp = get_file_paged(&srv, "docs", "large.txt", 0, 5).await;

    assert_eq!(resp.lines.len(), 5, "only the requested page should be returned");
    assert_eq!(resp.total_lines, 20, "total_lines must be the true total, not the page length");
}

// ── First page ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_first_page_returns_correct_lines() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "file.txt", 10).await;

    let resp = get_file_paged(&srv, "docs", "file.txt", 0, 4).await;

    assert_eq!(resp.lines, vec!["line 1", "line 2", "line 3", "line 4"]);
    assert_eq!(resp.total_lines, 10);
}

/// First page starting at line 1 → line_offsets should be empty (sequential).
#[tokio::test]
async fn test_first_page_has_no_line_offsets() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "file.txt", 10).await;

    let resp = get_file_paged(&srv, "docs", "file.txt", 0, 5).await;

    assert!(
        resp.line_offsets.is_empty(),
        "page starting at line 1 is sequential; line_offsets should be absent"
    );
}

// ── Subsequent pages ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_second_page_returns_correct_lines() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "file.txt", 10).await;

    let resp = get_file_paged(&srv, "docs", "file.txt", 4, 4).await;

    assert_eq!(resp.lines, vec!["line 5", "line 6", "line 7", "line 8"]);
    assert_eq!(resp.total_lines, 10);
}

/// Pages beyond the first are not 1-based, so line_offsets must be present.
#[tokio::test]
async fn test_non_first_page_has_line_offsets() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "file.txt", 10).await;

    let resp = get_file_paged(&srv, "docs", "file.txt", 5, 5).await;

    assert_eq!(
        resp.line_offsets,
        vec![6, 7, 8, 9, 10],
        "line_offsets must contain actual line numbers for non-first pages"
    );
}

/// Pages can be assembled into the full file by concatenating in order.
#[tokio::test]
async fn test_pages_concatenate_to_full_file() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "file.txt", 9).await;

    let full = get_file(&srv, "docs", "file.txt").await;
    let page1 = get_file_paged(&srv, "docs", "file.txt", 0, 3).await;
    let page2 = get_file_paged(&srv, "docs", "file.txt", 3, 3).await;
    let page3 = get_file_paged(&srv, "docs", "file.txt", 6, 3).await;

    let mut assembled: Vec<String> = vec![];
    assembled.extend(page1.lines);
    assembled.extend(page2.lines);
    assembled.extend(page3.lines);

    assert_eq!(assembled, full.lines, "pages must assemble into the full file");
}

// ── Last partial page ─────────────────────────────────────────────────────────

/// The last page may have fewer lines than limit.
#[tokio::test]
async fn test_last_partial_page() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "file.txt", 7).await;

    // Page size 4: page 0 = lines 1-4, page 1 = lines 5-7 (partial).
    let last = get_file_paged(&srv, "docs", "file.txt", 4, 4).await;

    assert_eq!(last.lines, vec!["line 5", "line 6", "line 7"]);
    assert_eq!(last.total_lines, 7);
}

// ── Out-of-bounds offset ──────────────────────────────────────────────────────

/// An offset beyond the total returns empty lines but correct total_lines.
#[tokio::test]
async fn test_offset_beyond_total_returns_empty_lines() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "small.txt", 5).await;

    let resp = get_file_paged(&srv, "docs", "small.txt", 100, 10).await;

    assert!(resp.lines.is_empty(), "offset beyond total should return no content lines");
    assert_eq!(resp.total_lines, 5, "total_lines must still reflect the true count");
}

// ── Metadata always returned ──────────────────────────────────────────────────

/// Metadata (line_number=0) must appear in every paged response, including
/// pages that are deep in the file (non-zero offset).
#[tokio::test]
async fn test_metadata_always_returned_for_any_page() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "notes.txt", 10).await;

    // Request a mid-file page that would not include line_number=0 in the
    // content query. The path entry must still appear in `metadata`.
    let resp = get_file_paged(&srv, "docs", "notes.txt", 5, 3).await;

    assert!(
        !resp.metadata.is_empty(),
        "metadata must always be returned, even for non-zero offset pages"
    );
    assert!(
        resp.metadata.iter().any(|m| m.contains("notes.txt")),
        "file path should appear in metadata: {:?}", resp.metadata
    );
}

/// Metadata is also present when offset is at the exact end of the file.
#[tokio::test]
async fn test_metadata_returned_when_content_lines_empty() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "notes.txt", 5).await;

    let resp = get_file_paged(&srv, "docs", "notes.txt", 5, 10).await;

    assert!(resp.lines.is_empty());
    assert!(
        !resp.metadata.is_empty(),
        "metadata must be present even when no content lines are returned"
    );
}

// ── Offset-only (no limit) ────────────────────────────────────────────────────

/// When limit is absent, offset is ignored and all lines are returned
/// (backward-compatible behaviour — older clients never send either param).
#[tokio::test]
async fn test_offset_without_limit_returns_all() {
    let srv = TestServer::spawn().await;
    index_numbered_file(&srv, "docs", "file.txt", 8).await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/file?source=docs&path=file.txt&offset=3"))
        .send()
        .await
        .unwrap()
        .json::<FileResponse>()
        .await
        .unwrap();

    // offset without limit is ignored — full file is returned.
    assert_eq!(resp.lines.len(), 8);
    assert_eq!(resp.total_lines, 8);
}
