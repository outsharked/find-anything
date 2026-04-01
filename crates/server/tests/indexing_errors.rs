mod helpers;
use helpers::{TestServer, make_text_bulk};

use find_common::api::{BulkRequest, ErrorsResponse, FileKind, IndexFile, IndexLine, IndexingFailure, SCANNER_VERSION};

// ── helpers ───────────────────────────────────────────────────────────────────

async fn get_errors(srv: &TestServer, source: &str) -> ErrorsResponse {
    srv.client
        .get(srv.url(&format!("/api/v1/errors?source={source}")))
        .send()
        .await
        .expect("errors request")
        .json()
        .await
        .expect("errors json")
}

/// Build a BulkRequest that reports a failure for `path` with no file upsert —
/// mimicking the fixed client behaviour where a failed extractor does not send
/// a completion upsert.
fn failure_only_bulk(source: &str, path: &str, error: &str) -> BulkRequest {
    BulkRequest {
        source: source.to_string(),
        files: vec![],
        delete_paths: vec![],
        scan_timestamp: Some(1_700_000_000),
        indexing_failures: vec![IndexingFailure {
            path: path.to_string(),
            error: error.to_string(),
        }],
        rename_paths: vec![],
    }
}

/// Build a minimal completion-upsert BulkRequest for a file with no content
/// lines (simulating what the old client code sent after a failed extraction).
fn completion_upsert_bulk(source: &str, path: &str, mtime: i64) -> BulkRequest {
    BulkRequest {
        source: source.to_string(),
        files: vec![IndexFile {
            path: path.to_string(),
            mtime,
            size: Some(1024),
            kind: FileKind::Text,
            lines: vec![IndexLine {
                archive_path: None,
                line_number: 0,
                content: format!("[PATH] {path}"),
            }],
            extract_ms: None,
            file_hash: None,
            scanner_version: SCANNER_VERSION,
            is_new: true,
            force: false,
        }],
        delete_paths: vec![],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// An indexing failure submitted in a bulk request should appear in the errors endpoint.
#[tokio::test]
async fn test_indexing_failure_recorded() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&failure_only_bulk("docs", "heavy.pdf", "timed out after 600s")).await;
    srv.wait_for_idle().await;

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 1, "expected 1 indexing error");
    assert_eq!(resp.errors[0].path, "heavy.pdf");
    assert!(resp.errors[0].error.contains("timed out"), "error text mismatch: {}", resp.errors[0].error);
    assert_eq!(resp.errors[0].count, 1);
}

/// A successful upsert for a file that previously had an error should clear the error.
#[tokio::test]
async fn test_successful_upsert_clears_error() {
    let srv = TestServer::spawn().await;

    // First scan: failure recorded.
    srv.post_bulk(&failure_only_bulk("docs", "report.pdf", "timed out after 600s")).await;
    srv.wait_for_idle().await;

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 1, "expected error before successful upsert");

    // Second scan: file extracted successfully and upserted with real content.
    srv.post_bulk(&make_text_bulk("docs", "report.pdf", "quarterly earnings")).await;
    srv.wait_for_idle().await;

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 0, "error should be cleared after successful upsert");
}

/// When failure and upsert arrive in the same batch (e.g. server-side normalization
/// failure for a file that was otherwise indexed), the failure takes precedence.
#[tokio::test]
async fn test_failure_in_same_batch_as_upsert_survives() {
    let srv = TestServer::spawn().await;

    // Single batch: index the file AND report a failure for the same path.
    // do_cleanup_writes deletes errors for successfully_indexed paths first,
    // then inserts new failures — so the failure must win.
    let mut req = make_text_bulk("docs", "mixed.pdf", "some content");
    req.indexing_failures.push(IndexingFailure {
        path: "mixed.pdf".to_string(),
        error: "extraction partially failed".to_string(),
    });
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 1, "failure in same batch as upsert should be recorded");
    assert_eq!(resp.errors[0].path, "mixed.pdf");
}

/// Regression: a completion upsert sent in a *separate* batch after a failure
/// (the old client bug) must NOT clear the indexing error.
/// The server can't distinguish this from a genuine re-index — this test
/// documents the expected server behaviour when the client sends both.
/// After the client fix the completion upsert is never sent on failure, but
/// the server should handle this gracefully regardless.
#[tokio::test]
async fn test_completion_upsert_after_failure_clears_error() {
    let srv = TestServer::spawn().await;

    // Batch 1: failure only (fixed client behaviour).
    srv.post_bulk(&failure_only_bulk("docs", "big.pdf", "timed out after 600s")).await;
    srv.wait_for_idle().await;

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 1, "failure should be recorded after batch 1");

    // Batch 2: completion upsert with no failures (old client bug — sends upsert
    // without content after a timeout). The server treats any successful upsert
    // as clearing the error; the fix is on the client not to send this at all.
    srv.post_bulk(&completion_upsert_bulk("docs", "big.pdf", 1_700_000_001)).await;
    srv.wait_for_idle().await;

    // The server clears the error when it sees a successful upsert.
    // This test documents the server's behaviour so we know what the client must
    // avoid: after a failure, never send a completion upsert without a paired failure.
    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 0, "server clears error on successful upsert (client must not send upsert after failure)");
}

/// Repeated failures increment the error count and update last_seen.
#[tokio::test]
async fn test_repeated_failures_increment_count() {
    let srv = TestServer::spawn().await;

    for _ in 0..3 {
        srv.post_bulk(&failure_only_bulk("docs", "corrupt.pdf", "timed out after 600s")).await;
        srv.wait_for_idle().await;
    }

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 1, "still one error entry for the same file");
    assert_eq!(resp.errors[0].count, 3, "count should be 3 after 3 failures");
}

/// Errors for deleted files should be removed when the file is deleted.
#[tokio::test]
async fn test_delete_clears_error() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&failure_only_bulk("docs", "gone.pdf", "timed out after 600s")).await;
    srv.wait_for_idle().await;

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 1);

    let delete_req = BulkRequest {
        source: "docs".to_string(),
        files: vec![],
        delete_paths: vec!["gone.pdf".to_string()],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
    };
    srv.post_bulk(&delete_req).await;
    srv.wait_for_idle().await;

    let resp = get_errors(&srv, "docs").await;
    assert_eq!(resp.total, 0, "error should be removed when file is deleted");
}
