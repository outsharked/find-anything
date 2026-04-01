/// Integration tests for iWork document handling.
///
/// Covers:
///   1. `force` flag on IndexFile bypasses the server-side stale mtime guard.
///   2. `/api/v1/view` serves a preview image extracted from inside a .pages ZIP.
mod helpers;
use helpers::TestServer;

use find_common::api::{
    BulkRequest, FileKind, IndexFile, IndexLine, SearchResponse, SCANNER_VERSION,
    LINE_PATH, LINE_CONTENT_START, LINE_METADATA,
};

// ── 1. Force flag bypasses the stale mtime guard ──────────────────────────────

/// Submitting with a lower mtime than what is already stored is normally rejected
/// (stale mtime guard). With `force: true`, the server must accept it anyway.
#[tokio::test]
async fn force_flag_bypasses_stale_mtime_guard() {
    let srv = TestServer::spawn().await;

    // Index with a far-future mtime so any subsequent normal submission looks stale.
    let initial = BulkRequest {
        source: "docs".to_string(),
        files: vec![IndexFile {
            path: "doc.txt".to_string(),
            mtime: 9_999_999_999,
            size: None,
            kind: FileKind::Text,
            lines: vec![
                IndexLine { archive_path: None, line_number: LINE_PATH,          content: "[PATH] doc.txt".to_string() },
                IndexLine { archive_path: None, line_number: LINE_CONTENT_START, content: "original_content_aaa".to_string() },
            ],
            file_hash: None,
            extract_ms: None,
            scanner_version: SCANNER_VERSION,
            is_new: true,
            force: false,
        }],
        delete_paths: vec![],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
    };
    srv.post_bulk(&initial).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv.client
        .get(srv.url("/api/v1/search?q=original_content_aaa&source=docs"))
        .send().await.unwrap().json().await.unwrap();
    assert!(resp.total >= 1, "original content should be indexed");

    // Submit with lower mtime and NO force — stale guard must reject it.
    let stale = BulkRequest {
        source: "docs".to_string(),
        files: vec![IndexFile {
            path: "doc.txt".to_string(),
            mtime: 1_000,
            size: None,
            kind: FileKind::Text,
            lines: vec![
                IndexLine { archive_path: None, line_number: LINE_PATH,          content: "[PATH] doc.txt".to_string() },
                IndexLine { archive_path: None, line_number: LINE_CONTENT_START, content: "stale_update_bbb".to_string() },
            ],
            file_hash: None,
            extract_ms: None,
            scanner_version: SCANNER_VERSION,
            is_new: false,
            force: false,
        }],
        delete_paths: vec![],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
    };
    srv.post_bulk(&stale).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv.client
        .get(srv.url("/api/v1/search?q=stale_update_bbb&source=docs"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(resp.total, 0, "stale submission (no force) must be rejected");

    // Same content with force: true — must be accepted.
    let forced = BulkRequest {
        source: "docs".to_string(),
        files: vec![IndexFile {
            path: "doc.txt".to_string(),
            mtime: 1_000,
            size: None,
            kind: FileKind::Text,
            lines: vec![
                IndexLine { archive_path: None, line_number: LINE_PATH,          content: "[PATH] doc.txt".to_string() },
                IndexLine { archive_path: None, line_number: LINE_CONTENT_START, content: "forced_update_ccc".to_string() },
            ],
            file_hash: None,
            extract_ms: None,
            scanner_version: SCANNER_VERSION,
            is_new: false,
            force: true,
        }],
        delete_paths: vec![],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
    };
    srv.post_bulk(&forced).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv.client
        .get(srv.url("/api/v1/search?q=forced_update_ccc&source=docs"))
        .send().await.unwrap().json().await.unwrap();
    assert!(resp.total >= 1, "forced submission must bypass stale guard and be indexed");
}

// ── 2. /api/v1/view serves the embedded preview from a .pages ZIP ─────────────

/// The view endpoint must serve preview.jpg from inside a .pages file when
/// requested as a composite path (doc.pages::preview.jpg).  The .pages file is
/// a ZIP; `get_archive_member_bytes` opens it and returns the JPEG bytes.
/// No separate DB entry is created for the preview child — the iwork_parent_kind
/// fallback in view.rs finds the parent document and routes the request.
#[tokio::test]
async fn iwork_preview_served_by_view_endpoint() {
    // Load test.pages (a ZIP containing preview.jpg) from the shared fixture.
    let fixture_zip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("extractors").join("archive")
        .join("tests").join("fixtures").join("iwork_preview.zip");
    let outer = std::fs::read(&fixture_zip).expect("read iwork_preview.zip");
    let pages_bytes = {
        use std::io::Read as _;
        let cursor = std::io::Cursor::new(&outer);
        let mut zip = zip::ZipArchive::new(cursor).unwrap();
        let mut entry = zip.by_name("test.pages").unwrap();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        buf
    };

    let source_dir = tempfile::TempDir::new().unwrap();
    std::fs::write(source_dir.path().join("test.pages"), &pages_bytes).unwrap();
    let source_path = source_dir.path().to_str().unwrap().replace('\\', "/");

    let extra = format!("[sources.files]\npath = \"{source_path}\"\n");
    let srv = TestServer::spawn_with_extra_config(&extra).await;

    // Index test.pages as a document. The [IWORK_PREVIEW] metadata line is what
    // the iWork extractor emits; we submit it directly to test the server side.
    let req = BulkRequest {
        source: "files".to_string(),
        files: vec![IndexFile {
            path: "test.pages".to_string(),
            mtime: 1_700_000_000,
            size: Some(pages_bytes.len() as i64),
            kind: FileKind::Document,
            lines: vec![
                IndexLine { archive_path: None, line_number: LINE_PATH,     content: "[PATH] test.pages".to_string() },
                IndexLine { archive_path: None, line_number: LINE_METADATA, content: "[IWORK_PREVIEW] preview.jpg".to_string() },
            ],
            file_hash: None,
            extract_ms: None,
            scanner_version: SCANNER_VERSION,
            is_new: true,
            force: false,
        }],
        delete_paths: vec![],
        scan_timestamp: Some(1_700_000_000),
        indexing_failures: vec![],
        rename_paths: vec![],
    };
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // Request the embedded preview image via composite path.
    let url = srv.url("/api/v1/view?source=files&path=test.pages%3A%3Apreview.jpg");
    let resp = srv.client.get(&url).send().await.unwrap();

    assert_eq!(resp.status().as_u16(), 200, "view endpoint must return 200 for iWork preview");

    let ct = resp.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("image/"), "expected image content-type, got: {ct}");

    let body = resp.bytes().await.unwrap();
    // JPEG magic bytes: FF D8 FF
    assert!(
        body.len() >= 3 && body[0] == 0xFF && body[1] == 0xD8 && body[2] == 0xFF,
        "expected JPEG magic bytes in response body (got {} bytes, first 4: {:?})",
        body.len(), &body[..body.len().min(4)]
    );
}
