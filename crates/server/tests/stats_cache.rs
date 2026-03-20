mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::{BulkRequest, FileKind};

/// ?refresh=true returns the correct file count after indexing.
#[tokio::test]
async fn refresh_returns_correct_file_count() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("src", "hello.txt", "hello world")).await;
    srv.wait_for_idle().await;

    let resp = srv.get_stats_refresh().await;
    let src = resp.sources.iter().find(|s| s.name == "src").expect("source not found");
    assert_eq!(src.total_files, 1);
    assert!(src.total_size > 0);
}

/// Indexing a second file increments total_files in the cache.
#[tokio::test]
async fn incremental_new_file_increments_total_files() {
    let srv = TestServer::spawn().await;

    // Index the first file and refresh the cache so the source entry exists.
    srv.post_bulk(&make_text_bulk("src", "a.txt", "first")).await;
    srv.wait_for_idle().await;
    let initial = srv.get_stats_refresh().await;
    let count_before = initial
        .sources
        .iter()
        .find(|s| s.name == "src")
        .map(|s| s.total_files)
        .unwrap_or(0);

    // Index a second file.
    srv.post_bulk(&make_text_bulk("src", "b.txt", "second")).await;
    srv.wait_for_idle().await;

    // The cache should reflect the incremental update without a refresh.
    let resp = srv.get_stats().await;
    let src = resp.sources.iter().find(|s| s.name == "src").expect("source not found");
    assert_eq!(src.total_files, count_before + 1);
}

/// Deleting a file decrements total_files in the cache.
#[tokio::test]
async fn incremental_delete_decrements_total_files() {
    let srv = TestServer::spawn().await;

    // Index a file and populate the cache.
    srv.post_bulk(&make_text_bulk("src", "file.txt", "content")).await;
    srv.wait_for_idle().await;
    srv.get_stats_refresh().await;

    // Delete the file.
    let del_req = BulkRequest {
        source: "src".to_string(),
        files: vec![],
        delete_paths: vec!["file.txt".to_string()],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
    };
    srv.post_bulk(&del_req).await;
    srv.wait_for_idle().await;

    // Cache should show 0 files without requiring a refresh.
    let resp = srv.get_stats().await;
    let src = resp.sources.iter().find(|s| s.name == "src").expect("source not found");
    assert_eq!(src.total_files, 0);
}

/// Indexing multiple text files updates by_kind incrementally.
#[tokio::test]
async fn incremental_by_kind_is_updated() {
    let srv = TestServer::spawn().await;

    // Index the first file and populate the cache.
    srv.post_bulk(&make_text_bulk("src", "readme.txt", "text content")).await;
    srv.wait_for_idle().await;
    srv.get_stats_refresh().await;

    // Index a second text file.
    srv.post_bulk(&make_text_bulk("src", "other.txt", "more text")).await;
    srv.wait_for_idle().await;

    let resp = srv.get_stats().await;
    let src = resp.sources.iter().find(|s| s.name == "src").expect("source not found");

    // There should be at least one kind entry with count >= 2.
    let text_kind = src.by_kind.values().find(|k| k.count >= 2);
    assert!(text_kind.is_some(), "expected at least 2 files in a kind, got: {:?}", src.by_kind);
}

// ── GET /api/v1/stats — source appears with correct counts ────────────────────

#[tokio::test]
async fn test_stats_endpoint_returns_source() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("myapp", "a.txt", "content")).await;
    srv.post_bulk(&make_text_bulk("myapp", "b.txt", "content")).await;
    srv.wait_for_idle().await;

    // The live stats cache is updated incrementally — no ?refresh needed.
    let resp = srv.get_stats().await;
    let src = resp.sources.iter().find(|s| s.name == "myapp");
    assert!(src.is_some(), "source 'myapp' should appear in stats");
    assert_eq!(src.unwrap().total_files, 2, "should report 2 indexed files");
}

#[tokio::test]
async fn test_stats_requires_auth() {
    let srv = TestServer::spawn().await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/stats"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "stats without auth should return 401");
}

#[tokio::test]
async fn test_stats_by_ext_populated_after_refresh() {
    let srv = TestServer::spawn().await;

    // Use paths whose extensions the scalar functions should extract.
    srv.post_bulk(&make_text_bulk("src", "script.js", "var x = 1;")).await;
    srv.post_bulk(&make_text_bulk("src", "module.py", "import os")).await;
    srv.wait_for_idle().await;

    let resp = srv.get_stats_refresh().await;
    let src = resp.sources.iter().find(|s| s.name == "src").expect("source not found");

    let exts: Vec<&str> = src.by_ext.iter().map(|e| e.ext.as_str()).collect();
    assert!(exts.contains(&"js"), "by_ext should contain 'js', got: {exts:?}");
    assert!(exts.contains(&"py"), "by_ext should contain 'py', got: {exts:?}");
}

#[tokio::test]
async fn test_stats_inbox_pending_reflects_paused_requests() {
    let srv = TestServer::spawn().await;

    // Pause so the bulk request stays in the inbox.
    srv.client.post(srv.url("/api/v1/admin/inbox/pause")).send().await.unwrap();
    srv.post_bulk(&make_text_bulk("src", "file.txt", "content")).await;

    let resp = srv.get_stats().await;
    assert!(resp.inbox_pending >= 1, "inbox_pending should be >= 1 while paused");
    assert!(resp.inbox_paused, "inbox_paused should be true");
}

// ── GET /api/v1/stats/stream (SSE) ────────────────────────────────────────────

#[tokio::test]
async fn test_stats_stream_returns_sse_headers() {
    let srv = TestServer::spawn().await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/stats/stream"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let ct = resp.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/event-stream"), "stream must use text/event-stream, got: {ct}");
}

#[tokio::test]
async fn test_stats_stream_requires_auth() {
    let srv = TestServer::spawn().await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/stats/stream"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "stats/stream without auth should return 401");
}

// ── by_kind breakdown ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_stats_by_kind_appears_in_response() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("src", "doc.txt", "text content")).await;
    srv.wait_for_idle().await;

    let resp = srv.get_stats_refresh().await;
    let src = resp.sources.iter().find(|s| s.name == "src").expect("source not found");

    assert!(
        src.by_kind.contains_key(&FileKind::Text),
        "by_kind should contain Text, got: {:?}", src.by_kind.keys().collect::<Vec<_>>()
    );
    let text_stats = &src.by_kind[&FileKind::Text];
    assert_eq!(text_stats.count, 1, "should report 1 text file");
}
