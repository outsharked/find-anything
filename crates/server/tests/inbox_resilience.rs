//! Tests for server behaviour when the data directory (or the inbox inside it)
//! becomes temporarily unavailable — simulating what happens when a Proxmox
//! bind-mount disappears during a backup or snapshot.

mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::SearchResponse;

/// When the inbox directory is removed, POST /api/v1/bulk returns 500.
/// When the inbox is restored, the server resumes accepting and indexing
/// requests without requiring a restart.
#[tokio::test]
async fn test_bulk_returns_500_when_inbox_missing_then_recovers() {
    let srv = TestServer::spawn().await;

    // Index a file successfully before breaking anything.
    let req_before = make_text_bulk("src", "before.txt", "content before outage");
    srv.post_bulk(&req_before).await;
    srv.wait_for_idle().await;

    // Remove the inbox directory to simulate a mount becoming unavailable.
    let inbox_dir = srv.data_dir_path().join("inbox");
    let inbox_backup = srv.data_dir_path().join("inbox.bak");
    std::fs::rename(&inbox_dir, &inbox_backup).expect("rename inbox to .bak");

    // Bulk POST should now fail with 500.
    let req_during = make_text_bulk("src", "during.txt", "content during outage");
    let status = srv.post_bulk_status(&req_during).await;
    assert_eq!(status.as_u16(), 500, "bulk should return 500 when inbox is missing");

    // Restore the inbox directory.
    std::fs::rename(&inbox_backup, &inbox_dir).expect("restore inbox");

    // The server should recover without a restart: a new bulk POST succeeds and
    // the file is indexed.
    let req_after = make_text_bulk("src", "after.txt", "content after recovery");
    srv.post_bulk(&req_after).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=content+after+recovery&source=src"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(resp.total >= 1, "file indexed after recovery should be searchable");
}
