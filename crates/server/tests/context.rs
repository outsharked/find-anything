mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::{ContextBatchItem, ContextBatchRequest, ContextBatchResponse, ContextResponse, LINE_CONTENT_START};

// ── GET /api/v1/context ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_context_returns_surrounding_lines() {
    let srv = TestServer::spawn().await;

    // Index a multi-line file. Lines are stored at LINE_CONTENT_START, LINE_CONTENT_START+1, …
    let content = "line alpha\nline bravo\nline charlie\nline delta\nline echo";
    srv.post_bulk(&make_text_bulk("docs", "multi.txt", content)).await;
    srv.wait_for_idle().await;

    // Request context around the middle line (LINE_CONTENT_START + 2 = "line charlie").
    let center = LINE_CONTENT_START + 2;
    let resp: ContextResponse = srv
        .client
        .get(srv.url(&format!("/api/v1/context?source=docs&path=multi.txt&line={center}&window=1")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(!resp.lines.is_empty(), "context must return at least one line");
    // With window=1, should include the line before and after center.
    assert!(resp.lines.len() >= 3, "window=1 should return at least 3 lines");
    assert!(resp.match_index.is_some(), "match_index should point to center line");
    // The center line must contain "charlie".
    let match_idx = resp.match_index.unwrap();
    assert!(resp.lines[match_idx].content.contains("charlie"),
        "center line should be 'line charlie', got {:?}", resp.lines[match_idx]);
}

#[tokio::test]
async fn test_context_window_is_respected() {
    let srv = TestServer::spawn().await;

    let content = (0..10).map(|i| format!("content line {i}")).collect::<Vec<_>>().join("\n");
    srv.post_bulk(&make_text_bulk("docs", "long.txt", &content)).await;
    srv.wait_for_idle().await;

    let center = LINE_CONTENT_START + 5; // middle of the file
    let window = 2;
    let resp: ContextResponse = srv
        .client
        .get(srv.url(&format!("/api/v1/context?source=docs&path=long.txt&line={center}&window={window}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // window=2 means center-2..=center+2 → at most 5 lines (clamped at file boundaries).
    assert!(resp.lines.len() <= 5, "window=2 should return at most 5 lines, got {}", resp.lines.len());
    assert_eq!(resp.lines.len(), 5, "with enough surrounding lines, should get exactly 5");
    assert!(resp.match_index.is_some());
    assert_eq!(resp.match_index.unwrap(), 2, "center should be index 2 in a 5-line window");
}

#[tokio::test]
async fn test_context_clamped_at_file_start() {
    let srv = TestServer::spawn().await;

    let content = "first line\nsecond line\nthird line";
    srv.post_bulk(&make_text_bulk("docs", "short.txt", content)).await;
    srv.wait_for_idle().await;

    // Ask for context at the very first content line with a large window.
    let center = LINE_CONTENT_START;
    let resp: ContextResponse = srv
        .client
        .get(srv.url(&format!("/api/v1/context?source=docs&path=short.txt&line={center}&window=5")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(!resp.lines.is_empty(), "should return lines even near start of file");
    assert!(resp.match_index.is_some(), "match_index should be set");
}

#[tokio::test]
async fn test_context_requires_auth() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("docs", "file.txt", "some content here")).await;
    srv.wait_for_idle().await;

    let status = reqwest::Client::new()
        .get(srv.url(&format!("/api/v1/context?source=docs&path=file.txt&line={LINE_CONTENT_START}&window=2")))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "context without auth should return 401");
}

// ── POST /api/v1/context-batch ────────────────────────────────────────────────

#[tokio::test]
async fn test_context_batch_multiple_items() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("src", "a.txt", "alpha content here")).await;
    srv.post_bulk(&make_text_bulk("src", "b.txt", "bravo content here")).await;
    srv.post_bulk(&make_text_bulk("src", "c.txt", "charlie content here")).await;
    srv.wait_for_idle().await;

    let req = ContextBatchRequest {
        requests: vec![
            ContextBatchItem { source: "src".into(), path: "a.txt".into(), archive_path: None, line: LINE_CONTENT_START, window: 2 },
            ContextBatchItem { source: "src".into(), path: "b.txt".into(), archive_path: None, line: LINE_CONTENT_START, window: 2 },
            ContextBatchItem { source: "src".into(), path: "c.txt".into(), archive_path: None, line: LINE_CONTENT_START, window: 2 },
        ],
    };

    let resp: ContextBatchResponse = srv
        .client
        .post(srv.url("/api/v1/context-batch"))
        .json(&req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.results.len(), 3, "batch should return one result per request");
    // Each result should contain its expected content.
    let has_alpha   = resp.results.iter().any(|r| r.path == "a.txt" && r.lines.iter().any(|l| l.content.contains("alpha")));
    let has_bravo   = resp.results.iter().any(|r| r.path == "b.txt" && r.lines.iter().any(|l| l.content.contains("bravo")));
    let has_charlie = resp.results.iter().any(|r| r.path == "c.txt" && r.lines.iter().any(|l| l.content.contains("charlie")));
    assert!(has_alpha,   "result for a.txt should contain 'alpha'");
    assert!(has_bravo,   "result for b.txt should contain 'bravo'");
    assert!(has_charlie, "result for c.txt should contain 'charlie'");
}

#[tokio::test]
async fn test_context_batch_unknown_source_returns_empty_lines() {
    let srv = TestServer::spawn().await;

    let req = ContextBatchRequest {
        requests: vec![
            ContextBatchItem {
                source: "no-such-source".into(),
                path: "ghost.txt".into(),
                archive_path: None,
                line: LINE_CONTENT_START,
                window: 2,
            },
        ],
    };

    let resp: ContextBatchResponse = srv
        .client
        .post(srv.url("/api/v1/context-batch"))
        .json(&req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Graceful degradation: returns one result with empty lines rather than erroring.
    assert_eq!(resp.results.len(), 1);
    assert!(resp.results[0].lines.is_empty(), "missing source should yield empty lines");
}

#[tokio::test]
async fn test_context_batch_requires_auth() {
    let srv = TestServer::spawn().await;

    let req = ContextBatchRequest { requests: vec![] };
    let status = reqwest::Client::new()
        .post(srv.url("/api/v1/context-batch"))
        .json(&req)
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "context-batch without auth should return 401");
}
