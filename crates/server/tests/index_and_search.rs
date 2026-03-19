mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::{
    BulkRequest, DirEntry, FileKind, FileResponse, IndexFile, IndexLine, SearchResponse,
    SourceInfo, StatsResponse, TreeResponse, SCANNER_VERSION, LINE_PATH, LINE_METADATA,
    LINE_CONTENT_START,
};

#[tokio::test]
async fn test_bulk_index_then_search() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "readme.txt", "hello world this is a test");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=hello&source=docs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "expected at least one result");
    assert!(resp.results.iter().any(|r| r.path == "readme.txt"));
}

#[tokio::test]
async fn test_search_without_source_searches_all() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "note.txt", "zymurgy is the study of fermentation");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=zymurgy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.total >= 1, "expected zymurgy to be found without source filter");
}

#[tokio::test]
async fn test_context_retrieval() {
    let srv = TestServer::spawn().await;
    let content = (1..=20)
        .map(|n| format!("line {n} content"))
        .collect::<Vec<_>>()
        .join("\n");
    let req = make_text_bulk("docs", "multiline.txt", &content);
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/context?source=docs&path=multiline.txt&line=10&window=3"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 200);
}

#[tokio::test]
async fn test_file_retrieval() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "sample.txt", "line one\nline two\nline three\nline four\nline five");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: FileResponse = srv
        .client
        .get(srv.url("/api/v1/file?source=docs&path=sample.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.file_kind, FileKind::Text);
    // total_lines counts content lines only (line_number > 0); the filename at line_number=0 is excluded
    assert_eq!(resp.total_lines, 5);
}

#[tokio::test]
async fn test_stats_shows_indexed_file() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("my-source", "data.txt", "some content here");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // Use ?refresh=true so stats are rebuilt from DB rather than waiting for the
    // 30-second startup cache rebuild.
    let resp: StatsResponse = srv
        .client
        .get(srv.url("/api/v1/stats?refresh=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let source = resp.sources.iter().find(|s| s.name == "my-source")
        .expect("my-source not found in stats");
    assert_eq!(source.total_files, 1);
}

#[tokio::test]
async fn test_tree_shows_indexed_file() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "subdir/file.txt", "tree test content");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // Root listing should show "subdir" as a directory entry
    let root: TreeResponse = srv
        .client
        .get(srv.url("/api/v1/tree?source=docs&prefix="))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        root.entries.iter().any(|e| e.name == "subdir" && e.entry_type == "dir"),
        "expected subdir in root tree, got: {:?}", root.entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // Subdir listing should show the file
    let sub: TreeResponse = srv
        .client
        .get(srv.url("/api/v1/tree?source=docs&prefix=subdir/"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        sub.entries.iter().any(|e| e.name == "file.txt" && e.entry_type == "file"),
        "expected file.txt in subdir tree"
    );
}

#[tokio::test]
async fn test_recent_shows_indexed_file() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("docs", "recent.txt", "recently added content");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: serde_json::Value = srv
        .client
        .get(srv.url("/api/v1/recent"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let files = resp["files"].as_array().expect("files array");
    assert!(
        files.iter().any(|f| f["path"].as_str() == Some("recent.txt")),
        "expected recent.txt in recent files"
    );
}

#[tokio::test]
async fn test_sources_list_after_indexing() {
    let srv = TestServer::spawn().await;
    let req = make_text_bulk("my-unique-source", "file.txt", "content");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let sources: Vec<SourceInfo> = srv
        .client
        .get(srv.url("/api/v1/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(
        sources.iter().any(|s| s.name == "my-unique-source"),
        "expected my-unique-source in sources list"
    );
}

/// Build a BulkRequest that submits an archive member with a known size.
/// The outer archive file and one member are both included so the server can
/// navigate to the member via the tree endpoint.
fn make_archive_member_bulk(source: &str, archive_path: &str, member_name: &str, member_size: i64) -> BulkRequest {
    let composite = format!("{archive_path}::{member_name}");

    let outer = IndexFile {
        path: archive_path.to_string(),
        mtime: 1_700_000_000,
        size: Some(9999),
        kind: FileKind::Archive,
        lines: vec![
            IndexLine { archive_path: None, line_number: LINE_PATH, content: format!("[PATH] {archive_path}") },
            IndexLine { archive_path: None, line_number: LINE_METADATA, content: String::new() },
        ],
        extract_ms: None,
        content_hash: None,
        scanner_version: SCANNER_VERSION,
        is_new: true,
    };

    let member = IndexFile {
        path: composite.clone(),
        mtime: 1_700_000_000,
        size: Some(member_size),
        kind: FileKind::Text,
        lines: vec![
            IndexLine { archive_path: None, line_number: LINE_PATH, content: format!("[PATH] {composite}") },
            IndexLine { archive_path: None, line_number: LINE_METADATA, content: String::new() },
            IndexLine { archive_path: None, line_number: LINE_CONTENT_START, content: "hello from member".to_string() },
        ],
        extract_ms: None,
        content_hash: None,
        scanner_version: SCANNER_VERSION,
        is_new: true,
    };

    BulkRequest {
        source: source.to_string(),
        files: vec![outer, member],
        delete_paths: vec![],
        scan_timestamp: Some(1_700_000_000),
        indexing_failures: vec![],
        rename_paths: vec![],
    }
}

#[tokio::test]
async fn test_archive_member_size_stored_and_retrieved_via_file_endpoint() {
    let srv = TestServer::spawn().await;
    let req = make_archive_member_bulk("docs", "pkg.zip", "readme.txt", 4096);
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: FileResponse = srv
        .client
        .get(srv.url("/api/v1/file?source=docs&path=pkg.zip::readme.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.size, Some(4096), "archive member size should be stored and returned");
}

#[tokio::test]
async fn test_archive_member_size_stored_and_retrieved_via_tree_endpoint() {
    let srv = TestServer::spawn().await;
    let req = make_archive_member_bulk("docs", "bundle.zip", "data.txt", 8192);
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp: TreeResponse = srv
        .client
        .get(srv.url("/api/v1/tree?source=docs&prefix=bundle.zip::"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let entry: &DirEntry = resp.entries.iter().find(|e| e.name == "data.txt")
        .expect("data.txt not found in tree listing");
    assert_eq!(entry.size, Some(8192), "archive member size should be visible in tree listing");
}
