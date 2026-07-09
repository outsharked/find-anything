//! Issue #59 / plan 093: bursts of single-file BulkRequests must be fully
//! indexed and searchable, with the inbox drained, under group-coalesced
//! processing.
mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::SearchResponse;

#[tokio::test]
async fn burst_of_single_file_requests_all_indexed_and_searchable() {
    let srv = TestServer::spawn().await;

    // Fire 40 single-file requests back-to-back (more than one group's worth)
    // without waiting in between — the watcher-burst pattern.
    for i in 0..40 {
        let req = make_text_bulk(
            "burst",
            &format!("dir/file{i:02}.txt"),
            &format!("needle{i:02} burst coalescing content"),
        );
        srv.post_bulk(&req).await;
    }

    srv.wait_for_idle().await;

    // Every file must be searchable.
    for i in 0..40 {
        let resp: SearchResponse = srv
            .client
            .get(srv.url(&format!("/api/v1/search?q=needle{i:02}&source=burst")))
            .send().await.unwrap()
            .json().await.unwrap();
        assert!(
            resp.results.iter().any(|r| r.path == format!("dir/file{i:02}.txt")),
            "file{i:02} not found after burst"
        );
    }

    // Inbox must be drained (no gz left behind, none quarantined).
    let inbox = srv.data_dir_path().join("inbox");
    let count = |d: &std::path::Path| std::fs::read_dir(d).map(|rd| rd
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gz"))
        .count()).unwrap_or(0);
    assert_eq!(count(&inbox), 0, "inbox not drained");
    assert_eq!(count(&inbox.join("failed")), 0, "requests were quarantined");
}

#[tokio::test]
async fn burst_with_interleaved_delete_applies_in_order() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("burst", "victim.txt", "shortlived content")).await;
    let del = find_common::api::BulkRequest {
        source: "burst".to_string(),
        files: vec![],
        delete_paths: vec!["victim.txt".to_string()],
        rename_paths: vec![],
        scan_timestamp: Some(2),
        indexing_failures: vec![],
    };
    srv.post_bulk(&del).await;
    srv.wait_for_idle().await;

    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=shortlived&source=burst"))
        .send().await.unwrap()
        .json().await.unwrap();
    assert!(
        !resp.results.iter().any(|r| r.path == "victim.txt"),
        "delete arriving after upsert must win"
    );
}
