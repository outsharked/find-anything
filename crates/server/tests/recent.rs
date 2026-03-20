mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::{BulkRequest, FileKind, IndexFile, IndexLine, RecentResponse, SCANNER_VERSION};

// Build a BulkRequest with a specific mtime (make_text_bulk always uses 1_700_000_000).
fn make_bulk_with_mtime(source: &str, path: &str, content: &str, mtime: i64) -> BulkRequest {
    use find_common::api::{LINE_CONTENT_START, LINE_METADATA, LINE_PATH};
    let mut lines = vec![
        IndexLine { archive_path: None, line_number: LINE_PATH,     content: format!("[PATH] {path}") },
        IndexLine { archive_path: None, line_number: LINE_METADATA, content: String::new() },
    ];
    for (i, line) in content.lines().enumerate() {
        lines.push(IndexLine { archive_path: None, line_number: i + LINE_CONTENT_START, content: line.to_string() });
    }
    BulkRequest {
        source: source.to_string(),
        files: vec![IndexFile {
            path: path.to_string(),
            mtime,
            size: Some(content.len() as i64),
            kind: FileKind::Text,
            lines,
            extract_ms: None,
            content_hash: None,
            scanner_version: SCANNER_VERSION,
            is_new: true,
        }],
        delete_paths: vec![],
        scan_timestamp: Some(mtime),
        indexing_failures: vec![],
        rename_paths: vec![],
    }
}

// ── GET /api/v1/recent ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_recent_returns_indexed_files() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("src", "a.txt", "content a")).await;
    srv.post_bulk(&make_text_bulk("src", "b.txt", "content b")).await;
    srv.post_bulk(&make_text_bulk("src", "c.txt", "content c")).await;
    srv.wait_for_idle().await;

    let resp: RecentResponse = srv
        .client
        .get(srv.url("/api/v1/recent"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let paths: Vec<&str> = resp.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"a.txt"), "a.txt should appear in recent");
    assert!(paths.contains(&"b.txt"), "b.txt should appear in recent");
    assert!(paths.contains(&"c.txt"), "c.txt should appear in recent");
}

#[tokio::test]
async fn test_recent_sorted_by_mtime() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_bulk_with_mtime("src", "old.txt",    "content", 1000)).await;
    srv.post_bulk(&make_bulk_with_mtime("src", "middle.txt", "content", 5000)).await;
    srv.post_bulk(&make_bulk_with_mtime("src", "new.txt",    "content", 9000)).await;
    srv.wait_for_idle().await;

    let resp: RecentResponse = srv
        .client
        .get(srv.url("/api/v1/recent?sort=mtime"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.files.len(), 3);
    // Should be sorted descending by mtime: new → middle → old.
    assert_eq!(resp.files[0].path, "new.txt",    "first should be newest");
    assert_eq!(resp.files[1].path, "middle.txt", "second should be middle");
    assert_eq!(resp.files[2].path, "old.txt",    "third should be oldest");
}

#[tokio::test]
async fn test_recent_limit_respected() {
    let srv = TestServer::spawn().await;

    for i in 0..10 {
        srv.post_bulk(&make_text_bulk("src", &format!("file_{i}.txt"), "content")).await;
    }
    srv.wait_for_idle().await;

    let resp: RecentResponse = srv
        .client
        .get(srv.url("/api/v1/recent?limit=3"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.files.len(), 3, "limit=3 should return exactly 3 results");
}

#[tokio::test]
async fn test_recent_limit_above_max_returns_400() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/recent?limit=9999"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400, "limit > MAX_RECENT_LIMIT should return 400");
}

#[tokio::test]
async fn test_recent_requires_auth() {
    let srv = TestServer::spawn().await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/recent"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "recent without auth should return 401");
}

#[tokio::test]
async fn test_recent_empty_when_nothing_indexed() {
    let srv = TestServer::spawn().await;

    let resp: RecentResponse = srv
        .client
        .get(srv.url("/api/v1/recent"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.files.is_empty(), "recent should be empty on fresh server");
}

// ── GET /api/v1/recent/stream (SSE) ──────────────────────────────────────────

#[tokio::test]
async fn test_recent_stream_returns_sse_headers() {
    let srv = TestServer::spawn().await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/recent/stream"))
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
async fn test_recent_stream_requires_auth() {
    let srv = TestServer::spawn().await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/recent/stream"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "recent/stream without auth should return 401");
}
