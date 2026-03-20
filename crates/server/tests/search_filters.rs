mod helpers;
use helpers::{make_text_bulk, make_text_bulk_hashed, TestServer};

use find_common::api::{
    BulkRequest, FileKind, IndexFile, IndexLine, SearchResponse, SCANNER_VERSION,
    LINE_CONTENT_START, LINE_METADATA, LINE_PATH,
};

fn make_bulk_with_kind(source: &str, path: &str, content: &str, kind: FileKind) -> BulkRequest {
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
            mtime: 1_700_000_000,
            size: Some(content.len() as i64),
            kind,
            lines,
            extract_ms: None,
            content_hash: None,
            scanner_version: SCANNER_VERSION,
            is_new: true,
        }],
        delete_paths: vec![],
        scan_timestamp: Some(1_700_000_000),
        indexing_failures: vec![],
        rename_paths: vec![],
    }
}

fn make_bulk_with_kind_and_mtime(source: &str, path: &str, content: &str, kind: FileKind, mtime: i64) -> BulkRequest {
    let mut req = make_bulk_with_kind(source, path, content, kind);
    req.files[0].mtime = mtime;
    req.scan_timestamp = Some(mtime);
    req
}

// ── kind filter ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_kind_filter_text() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_bulk_with_kind("src", "note.txt", "common keyword content", FileKind::Text)).await;
    srv.post_bulk(&make_bulk_with_kind("src", "photo.jpg", "common keyword content", FileKind::Image)).await;
    srv.wait_for_idle().await;

    // kind=text should return only the text file.
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=keyword&source=src&kind=text"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.results.iter().all(|r| r.kind == FileKind::Text),
        "kind=text filter should exclude non-text results");
    assert!(resp.results.iter().any(|r| r.path == "note.txt"),
        "text file should appear in kind=text results");
    assert!(!resp.results.iter().any(|r| r.path == "photo.jpg"),
        "image file must be excluded by kind=text filter");
}

#[tokio::test]
async fn test_search_kind_filter_image() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_bulk_with_kind("src", "note.txt", "common keyword content", FileKind::Text)).await;
    srv.post_bulk(&make_bulk_with_kind("src", "photo.jpg", "common keyword content", FileKind::Image)).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=keyword&source=src&kind=image"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.results.iter().all(|r| r.kind == FileKind::Image),
        "kind=image filter should exclude non-image results");
    assert!(resp.results.iter().any(|r| r.path == "photo.jpg"),
        "image file should appear in kind=image results");
}

// ── date filter ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_date_from_filter() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_bulk_with_kind_and_mtime("src", "old.txt", "keyword content here", FileKind::Text, 1000)).await;
    srv.post_bulk(&make_bulk_with_kind_and_mtime("src", "new.txt", "keyword content here", FileKind::Text, 9000)).await;
    srv.wait_for_idle().await;

    // date_from=5000 should exclude old.txt (mtime=1000).
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=keyword&source=src&date_from=5000"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.results.iter().any(|r| r.path == "new.txt"), "new file should appear after date_from");
    assert!(!resp.results.iter().any(|r| r.path == "old.txt"), "old file should be excluded by date_from");
}

#[tokio::test]
async fn test_search_date_to_filter() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_bulk_with_kind_and_mtime("src", "old.txt", "keyword content here", FileKind::Text, 1000)).await;
    srv.post_bulk(&make_bulk_with_kind_and_mtime("src", "new.txt", "keyword content here", FileKind::Text, 9000)).await;
    srv.wait_for_idle().await;

    // date_to=5000 should exclude new.txt (mtime=9000).
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=keyword&source=src&date_to=5000"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.results.iter().any(|r| r.path == "old.txt"), "old file should appear before date_to");
    assert!(!resp.results.iter().any(|r| r.path == "new.txt"), "new file should be excluded by date_to");
}

// ── pagination ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_pagination_no_overlap() {
    let srv = TestServer::spawn().await;

    for i in 0..15 {
        srv.post_bulk(&make_text_bulk("src", &format!("file_{i:02}.txt"), "pagination keyword content")).await;
    }
    srv.wait_for_idle().await;

    let page1: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=pagination+keyword&source=src&limit=5&offset=0"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let page2: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=pagination+keyword&source=src&limit=5&offset=5"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(page1.results.len(), 5, "page 1 should return 5 results");
    assert_eq!(page2.results.len(), 5, "page 2 should return 5 results");

    let paths1: std::collections::HashSet<&str> = page1.results.iter().map(|r| r.path.as_str()).collect();
    let paths2: std::collections::HashSet<&str> = page2.results.iter().map(|r| r.path.as_str()).collect();
    assert!(paths1.is_disjoint(&paths2), "page 1 and page 2 must not overlap");
}

#[tokio::test]
async fn test_search_pagination_total_is_accurate() {
    let srv = TestServer::spawn().await;

    for i in 0..8 {
        srv.post_bulk(&make_text_bulk("src", &format!("doc_{i}.txt"), "accurate keyword count")).await;
    }
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=accurate+keyword&source=src&limit=3"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.results.len(), 3, "should return only 3 results with limit=3");
    assert!(resp.total >= 8, "total should reflect all matching files, got {}", resp.total);
}

// ── duplicate paths ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_returns_duplicate_paths() {
    let srv = TestServer::spawn().await;

    // Two files with the same content_hash — they should appear as duplicates of each other.
    srv.post_bulk(&make_text_bulk_hashed("src", "original.txt", "duplicate content keyword here. ".repeat(5).trim())).await;
    srv.post_bulk(&make_text_bulk_hashed("src", "copy.txt",     "duplicate content keyword here. ".repeat(5).trim())).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=duplicate+content&source=src"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // At least one result should have a non-empty duplicate_paths.
    let has_dup = resp.results.iter().any(|r| !r.duplicate_paths.is_empty());
    assert!(has_dup, "at least one result should have non-empty duplicate_paths; results: {:?}",
        resp.results.iter().map(|r| (&r.path, &r.duplicate_paths)).collect::<Vec<_>>());
}

// ── case sensitivity ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_case_insensitive_by_default() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("src", "file.txt", "CamelCase identifier found here")).await;
    srv.wait_for_idle().await;

    // Searching lowercase should still find the file.
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=camelcase&source=src&mode=exact"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "case-insensitive exact search should match regardless of case");
}

#[tokio::test]
async fn test_search_case_sensitive_excludes_wrong_case() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("src", "file.txt", "CamelCase identifier found here")).await;
    srv.wait_for_idle().await;

    // case_sensitive=true with lowercase query should NOT match uppercase content.
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=camelcase&source=src&mode=exact&case_sensitive=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.total, 0, "case_sensitive=true should not match wrong case");
}

// ── auth + bad requests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_requires_auth() {
    let srv = TestServer::spawn().await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/search?q=anything"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "search without auth should return 401");
}

#[tokio::test]
async fn test_search_missing_q_returns_400() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/search"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400, "missing q param should return 400");
}

#[tokio::test]
async fn test_search_invalid_limit_returns_400() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/search?q=test&limit=notanumber"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400, "invalid limit should return 400");
}

// ── bad request params ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_invalid_offset_returns_400() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/search?q=test&offset=notanumber"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400, "invalid offset should return 400");
}

#[tokio::test]
async fn test_search_invalid_date_from_returns_400() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/search?q=test&date_from=notanumber"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400, "invalid date_from should return 400");
}

#[tokio::test]
async fn test_search_invalid_date_to_returns_400() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/search?q=test&date_to=bad"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400, "invalid date_to should return 400");
}

// ── multi-source search ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_search_across_multiple_sources() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("source-a", "a.txt", "multisource keyword content")).await;
    srv.post_bulk(&make_text_bulk("source-b", "b.txt", "multisource keyword content")).await;
    srv.wait_for_idle().await;

    // No source filter — should search all sources.
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=multisource+keyword"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sources: std::collections::HashSet<&str> = resp.results.iter().map(|r| r.source.as_str()).collect();
    assert!(sources.contains("source-a"), "results should include source-a");
    assert!(sources.contains("source-b"), "results should include source-b");
}

#[tokio::test]
async fn test_search_source_filter_restricts_results() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("alpha", "a.txt", "shared keyword content here")).await;
    srv.post_bulk(&make_text_bulk("beta",  "b.txt", "shared keyword content here")).await;
    srv.wait_for_idle().await;

    // Restrict to source=alpha only.
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=shared+keyword&source=alpha"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.results.iter().all(|r| r.source == "alpha"),
        "source filter should exclude results from other sources");
    assert!(!resp.results.is_empty(), "should return results from the specified source");
}
