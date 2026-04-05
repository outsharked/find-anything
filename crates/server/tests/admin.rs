mod helpers;
use helpers::{make_text_bulk, make_text_bulk_hashed, write_fake_gz, TestServer};

use find_common::api::{
    CompactResponse, InboxDeleteResponse, InboxRetryResponse, InboxShowResponse,
    InboxStatusResponse, SearchResponse, SourceDeleteResponse, StatsResponse,
    UpdateApplyResponse,
};

// ── delete_source ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_source_removes_files_from_search() {
    let srv = TestServer::spawn().await;

    // Index a file with a unique term
    let req = make_text_bulk("to-delete", "doc.txt", "quixotically unique term");
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;

    // Confirm it is searchable
    let before: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=quixotically&source=to-delete"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(before.total >= 1, "expected file to be findable before source deletion");

    // Delete the source
    let status = srv
        .client
        .delete(srv.url("/api/v1/admin/source?source=to-delete"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 200, "delete_source should return 200");

    // The source should no longer appear in stats
    let stats: StatsResponse = srv
        .client
        .get(srv.url("/api/v1/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !stats.sources.iter().any(|s| s.name == "to-delete"),
        "deleted source must not appear in stats"
    );
}

#[tokio::test]
async fn test_delete_nonexistent_source_returns_404() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .delete(srv.url("/api/v1/admin/source?source=no-such-source"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404, "deleting a non-existent source should return 404");
}

#[tokio::test]
async fn test_delete_source_requires_auth() {
    let srv = TestServer::spawn().await;

    // Index a source first so the path exists
    srv.post_bulk(&make_text_bulk("protected", "file.txt", "content")).await;
    srv.wait_for_idle().await;

    let status = reqwest::Client::new()
        .delete(srv.url("/api/v1/admin/source?source=protected"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "delete_source without auth should return 401");
}

#[tokio::test]
async fn test_delete_source_other_sources_unaffected() {
    let srv = TestServer::spawn().await;

    srv.post_bulk(&make_text_bulk("keep", "keep.txt", "keep this content")).await;
    srv.post_bulk(&make_text_bulk("drop", "drop.txt", "drop this content")).await;
    srv.wait_for_idle().await;

    // Delete only the "drop" source
    srv.client
        .delete(srv.url("/api/v1/admin/source?source=drop"))
        .send()
        .await
        .unwrap();

    // "keep" source should still be searchable
    let resp: SearchResponse = srv
        .client
        .get(srv.url("/api/v1/search?q=keep+this+content&source=keep"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(resp.total >= 1, "deleting one source must not affect others");
}

// ── compact ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compact_dry_run_returns_200() {
    let srv = TestServer::spawn().await;

    // Index some content so there are archives to scan
    srv.post_bulk(&make_text_bulk("docs", "file.txt", "compaction test content")).await;
    srv.wait_for_idle().await;

    let status = srv
        .client
        .post(srv.url("/api/v1/admin/compact?dry_run=true"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 200, "compact dry_run should return 200");
}

#[tokio::test]
async fn test_compact_requires_auth() {
    let srv = TestServer::spawn().await;

    let status = reqwest::Client::new()
        .post(srv.url("/api/v1/admin/compact"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401, "compact without auth should return 401");
}

#[tokio::test]
async fn test_compact_on_empty_server_returns_200() {
    let srv = TestServer::spawn().await;

    // No data indexed — compact should still succeed gracefully
    let status = srv
        .client
        .post(srv.url("/api/v1/admin/compact"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 200, "compact on empty server should return 200");
}

// ── inbox pause / resume ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_pause_and_resume_inbox() {
    let srv = TestServer::spawn().await;

    let pause_status = srv
        .client
        .post(srv.url("/api/v1/admin/inbox/pause"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(pause_status.as_u16(), 200, "pause should return 200");

    let resume_status = srv
        .client
        .post(srv.url("/api/v1/admin/inbox/resume"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(resume_status.as_u16(), 200, "resume should return 200");
}

// ── inbox status ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_inbox_status_after_indexing() {
    let srv = TestServer::spawn().await;

    // Pause processing so the request stays in pending.
    srv.client.post(srv.url("/api/v1/admin/inbox/pause")).send().await.unwrap();
    srv.post_bulk(&make_text_bulk("src", "file.txt", "content")).await;

    let status: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();

    assert!(!status.pending.is_empty(), "should have at least one pending item after pause+bulk");
    assert!(status.paused, "inbox should report paused=true");
}

// ── inbox clear ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_inbox_clear_pending() {
    let srv = TestServer::spawn().await;

    srv.client.post(srv.url("/api/v1/admin/inbox/pause")).send().await.unwrap();
    srv.post_bulk(&make_text_bulk("src", "file.txt", "content")).await;

    let before: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();
    assert!(!before.pending.is_empty(), "should have pending items before clear");

    let del: InboxDeleteResponse = srv.client
        .delete(srv.url("/api/v1/admin/inbox?target=pending"))
        .send().await.unwrap().json().await.unwrap();
    assert!(del.deleted > 0, "should report deleted count");

    let after: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();
    assert!(after.pending.is_empty(), "pending should be empty after clear");
}

#[tokio::test]
async fn test_inbox_clear_all() {
    let srv = TestServer::spawn().await;

    // Seed a file into failed/ directly.
    let failed_dir = srv.data_dir_path().join("inbox/failed");
    std::fs::create_dir_all(&failed_dir).unwrap();
    write_fake_gz(&failed_dir.join("fake_failed.gz"));

    // Pause and submit a pending request.
    srv.client.post(srv.url("/api/v1/admin/inbox/pause")).send().await.unwrap();
    srv.post_bulk(&make_text_bulk("src", "file.txt", "content")).await;

    let del: InboxDeleteResponse = srv.client
        .delete(srv.url("/api/v1/admin/inbox?target=all"))
        .send().await.unwrap().json().await.unwrap();
    assert!(del.deleted >= 2, "should delete both pending and failed ({} deleted)", del.deleted);

    let after: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();
    assert!(after.pending.is_empty(), "pending should be empty after clear-all");
    assert!(after.failed.is_empty(), "failed should be empty after clear-all");
}

// ── inbox retry ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_inbox_retry_moves_failed_to_pending() {
    let srv = TestServer::spawn().await;

    // Pause the worker so it doesn't pick up and re-fail the fake file.
    srv.client.post(srv.url("/api/v1/admin/inbox/pause")).send().await.unwrap();

    // Write a .gz directly into the failed dir.
    let failed_dir = srv.data_dir_path().join("inbox/failed");
    std::fs::create_dir_all(&failed_dir).unwrap();
    write_fake_gz(&failed_dir.join("fake_failed.gz"));

    let before: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();
    assert!(!before.failed.is_empty(), "should have a failed item");

    let retry: InboxRetryResponse = srv.client
        .post(srv.url("/api/v1/admin/inbox/retry"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(retry.retried, 1, "should retry exactly one file");

    // With worker paused, the file stays in pending (not re-failed).
    let after: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();
    assert!(after.failed.is_empty(), "failed should be empty after retry");
    assert!(!after.pending.is_empty(), "item should be in pending after retry");
}

// ── inbox pause stops processing ──────────────────────────────────────────

#[tokio::test]
async fn test_inbox_pause_stops_processing() {
    let srv = TestServer::spawn().await;

    srv.client.post(srv.url("/api/v1/admin/inbox/pause")).send().await.unwrap();
    srv.post_bulk(&make_text_bulk("src", "file.txt", "paused content")).await;

    // Give the worker a moment — it must NOT process the request while paused.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let status: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();
    assert!(!status.pending.is_empty(), "worker must not drain inbox while paused");

    // Resume and verify it drains.
    srv.client.post(srv.url("/api/v1/admin/inbox/resume")).send().await.unwrap();
    srv.wait_for_idle().await;

    let after: InboxStatusResponse = srv.client
        .get(srv.url("/api/v1/admin/inbox"))
        .send().await.unwrap().json().await.unwrap();
    assert!(after.pending.is_empty(), "inbox should drain after resume");
}

// ── compact with real content ─────────────────────────────────────────────

#[tokio::test]
async fn test_compact_removes_orphaned_chunks() {
    let srv = TestServer::spawn().await;

    // Index a file with a file_hash so the archive worker writes chunks to the content store.
    let big_content = "archive content line for compaction test. ".repeat(10);
    srv.post_bulk(&make_text_bulk_hashed("compact-src", "file.txt", &big_content)).await;
    srv.wait_for_idle().await;

    // Remove the source DB directly (without going through delete_source which would
    // also call remove_chunks and clean up the ZIP). This leaves the chunks in the
    // ZIP orphaned — no DB references them any more.
    let db_path = srv.data_dir_path().join("sources/compact-src.db");
    assert!(db_path.exists(), "source DB should exist after indexing");
    std::fs::remove_file(&db_path).unwrap();

    // Now compact should find and remove the orphaned chunks.
    let resp: CompactResponse = srv.client
        .post(srv.url("/api/v1/admin/compact"))
        .send().await.unwrap().json().await.unwrap();

    assert!(resp.chunks_removed > 0, "compact should remove orphaned chunks (got {})", resp.chunks_removed);
    assert!(resp.bytes_freed > 0, "compact should report freed bytes");
    assert!(!resp.dry_run);
}

#[tokio::test]
async fn test_compact_deletes_fully_orphaned_archive() {
    let srv = TestServer::spawn().await;

    let big_content = "archive content line for full orphan compaction test. ".repeat(10);
    srv.post_bulk(&make_text_bulk_hashed("compact-src2", "file.txt", &big_content)).await;
    srv.wait_for_idle().await;

    // Remove the source DB directly so all chunks in its archive(s) are now orphaned.
    // (Using delete_source would also call remove_chunks, which pre-cleans the ZIPs.)
    let db_path = srv.data_dir_path().join("sources/compact-src2.db");
    assert!(db_path.exists(), "source DB should exist after indexing");
    std::fs::remove_file(&db_path).unwrap();

    let resp: CompactResponse = srv.client
        .post(srv.url("/api/v1/admin/compact"))
        .send().await.unwrap().json().await.unwrap();

    // When all entries in an archive are orphaned, the file is deleted entirely.
    // chunks_removed counts the entries that were in the now-deleted archive.
    assert!(resp.units_deleted >= 1,
        "compact should delete the fully-orphaned archive (deleted={})", resp.units_deleted);
    assert!(resp.chunks_removed > 0,
        "compact should count the removed chunks (got {})", resp.chunks_removed);
}

#[tokio::test]
async fn test_compact_dry_run_does_not_remove_chunks() {
    let srv = TestServer::spawn().await;

    let big_content = "archive content line for dry run compaction test. ".repeat(10);
    srv.post_bulk(&make_text_bulk_hashed("compact-src3", "file.txt", &big_content)).await;
    srv.wait_for_idle().await;

    // Remove the source DB directly so chunks in ZIP are orphaned but not yet cleaned up.
    let db_path = srv.data_dir_path().join("sources/compact-src3.db");
    assert!(db_path.exists(), "source DB should exist after indexing");
    std::fs::remove_file(&db_path).unwrap();

    // Dry run: counts should be non-zero but nothing is actually removed.
    let dry: CompactResponse = srv.client
        .post(srv.url("/api/v1/admin/compact?dry_run=true"))
        .send().await.unwrap().json().await.unwrap();

    assert!(dry.chunks_removed > 0, "dry run should report chunks to remove");
    assert!(dry.dry_run, "response should indicate dry_run=true");

    // Run compact for real — should still find chunks (dry-run didn't touch them).
    let real: CompactResponse = srv.client
        .post(srv.url("/api/v1/admin/compact"))
        .send().await.unwrap().json().await.unwrap();

    assert!(real.chunks_removed > 0,
        "real compact should still find chunks that dry-run left intact");
}

// ── delete source removes archive content ─────────────────────────────────

#[tokio::test]
async fn test_delete_source_removes_chunk_refs() {
    let srv = TestServer::spawn().await;

    // Index with a file_hash so chunks are archived to the content store.
    let big_content = "archive content line for delete source chunk test. ".repeat(10);
    srv.post_bulk(&make_text_bulk_hashed("del-src", "file.txt", &big_content)).await;
    srv.wait_for_idle().await;

    // Verify the source DB exists.
    let db_path = srv.data_dir_path().join("sources/del-src.db");
    assert!(db_path.exists(), "source DB should exist after indexing");

    // Delete the source. chunks_removed is now always 0 — orphaned blobs in
    // the content store are deferred to the next compaction pass rather than
    // removed eagerly by delete_source.
    let del_resp: SourceDeleteResponse = srv.client
        .delete(srv.url("/api/v1/admin/source?source=del-src"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(del_resp.files_deleted, 1, "should report one file deleted");
    assert_eq!(del_resp.chunks_removed, 0,
        "chunks_removed is 0 — orphaned content cleaned up at next compaction");

    // Source DB should be gone.
    assert!(!db_path.exists(), "source DB should be removed after delete_source");

    // After delete_source, compact should now reclaim the orphaned content.
    let resp: CompactResponse = srv.client
        .post(srv.url("/api/v1/admin/compact"))
        .send().await.unwrap().json().await.unwrap();
    assert!(resp.chunks_removed > 0 || resp.units_deleted > 0 || resp.units_rewritten > 0,
        "compact should reclaim orphaned content after delete_source");
}

// ── inbox_show ────────────────────────────────────────────────────────────────

/// Helper: pause the inbox, post a bulk request, return the filename of the
/// pending item from GET /api/v1/admin/inbox.
async fn pause_and_queue_one(srv: &TestServer, source: &str) -> String {
    srv.client
        .post(srv.url("/api/v1/admin/inbox/pause"))
        .send()
        .await
        .unwrap();
    srv.post_bulk(&make_text_bulk(source, "doc.txt", "show test content"))
        .await;

    let status: InboxStatusResponse = srv
        .client
        .get(srv.url("/api/v1/admin/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!status.pending.is_empty(), "expected a pending item");
    status.pending[0].filename.clone()
}

#[tokio::test]
async fn test_inbox_show_pending_item() {
    let srv = TestServer::spawn().await;
    let filename = pause_and_queue_one(&srv, "show-src").await;

    let resp: InboxShowResponse = srv
        .client
        .get(srv.url(&format!("/api/v1/admin/inbox/show?name={filename}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.source, "show-src");
    assert_eq!(resp.files.len(), 1);
    assert_eq!(resp.files[0].path, "doc.txt");
    assert!(matches!(
        resp.queue,
        find_common::api::WorkerQueueSlot::Pending
    ));
}

#[tokio::test]
async fn test_inbox_show_without_gz_extension() {
    // The route should accept the name with or without .gz suffix.
    let srv = TestServer::spawn().await;
    let filename = pause_and_queue_one(&srv, "show-src2").await;
    let name_no_ext = filename.trim_end_matches(".gz");

    let resp: InboxShowResponse = srv
        .client
        .get(srv.url(&format!("/api/v1/admin/inbox/show?name={name_no_ext}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.source, "show-src2");
}

#[tokio::test]
async fn test_inbox_show_failed_item() {
    let srv = TestServer::spawn().await;

    let failed_dir = srv.data_dir_path().join("inbox/failed");
    std::fs::create_dir_all(&failed_dir).unwrap();

    // Write a valid gzip-encoded BulkRequest into failed/.
    let req = make_text_bulk("failed-src", "failed.txt", "failed content");
    let json = serde_json::to_vec(&req).unwrap();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &json).unwrap();
    let gz = enc.finish().unwrap();
    std::fs::write(failed_dir.join("failed_item.gz"), &gz).unwrap();

    let resp: InboxShowResponse = srv
        .client
        .get(srv.url("/api/v1/admin/inbox/show?name=failed_item.gz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp.source, "failed-src");
    assert!(matches!(
        resp.queue,
        find_common::api::WorkerQueueSlot::Failed
    ));
}

#[tokio::test]
async fn test_inbox_show_nonexistent_returns_404() {
    let srv = TestServer::spawn().await;

    let status = srv
        .client
        .get(srv.url("/api/v1/admin/inbox/show?name=no-such-file.gz"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}

#[tokio::test]
async fn test_inbox_show_requires_auth() {
    let srv = TestServer::spawn().await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/admin/inbox/show?name=anything.gz"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401);
}

// ── inbox auth guards ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_inbox_status_requires_auth() {
    let srv = TestServer::spawn().await;
    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/admin/inbox"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
}

#[tokio::test]
async fn test_inbox_clear_requires_auth() {
    let srv = TestServer::spawn().await;
    let status = reqwest::Client::new()
        .delete(srv.url("/api/v1/admin/inbox?target=pending"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
}

#[tokio::test]
async fn test_inbox_retry_requires_auth() {
    let srv = TestServer::spawn().await;
    let status = reqwest::Client::new()
        .post(srv.url("/api/v1/admin/inbox/retry"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
}

#[tokio::test]
async fn test_inbox_pause_requires_auth() {
    let srv = TestServer::spawn().await;
    let status = reqwest::Client::new()
        .post(srv.url("/api/v1/admin/inbox/pause"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
}

#[tokio::test]
async fn test_inbox_resume_requires_auth() {
    let srv = TestServer::spawn().await;
    let status = reqwest::Client::new()
        .post(srv.url("/api/v1/admin/inbox/resume"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
}

// ── inbox_clear target=failed ─────────────────────────────────────────────────

#[tokio::test]
async fn test_inbox_clear_failed_only() {
    let srv = TestServer::spawn().await;

    // Seed failed dir.
    let failed_dir = srv.data_dir_path().join("inbox/failed");
    std::fs::create_dir_all(&failed_dir).unwrap();
    write_fake_gz(&failed_dir.join("fail1.gz"));
    write_fake_gz(&failed_dir.join("fail2.gz"));

    // Also queue a pending item (should be unaffected).
    srv.client
        .post(srv.url("/api/v1/admin/inbox/pause"))
        .send()
        .await
        .unwrap();
    srv.post_bulk(&make_text_bulk("src", "file.txt", "content")).await;

    let del: InboxDeleteResponse = srv
        .client
        .delete(srv.url("/api/v1/admin/inbox?target=failed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(del.deleted, 2, "should delete exactly 2 failed items");

    let after: InboxStatusResponse = srv
        .client
        .get(srv.url("/api/v1/admin/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(after.failed.is_empty(), "failed queue should be empty");
    assert!(!after.pending.is_empty(), "pending should be unaffected");
}

// ── update_check ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_update_check_requires_auth() {
    let srv = TestServer::spawn().await;
    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/admin/update/check"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
}

// ── update_apply (no-systemd fast path) ──────────────────────────────────────

#[tokio::test]
async fn test_update_apply_without_systemd_returns_400() {
    // Force under_systemd = false regardless of whether INVOCATION_ID is set
    // in the CI environment (GitHub Actions runners may run under systemd).
    let srv = TestServer::spawn_with_extra_config("force_systemd = false").await;

    let resp: UpdateApplyResponse = srv
        .client
        .post(srv.url("/api/v1/admin/update/apply"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(!resp.ok);
    assert!(
        resp.message.contains("systemd"),
        "message should mention systemd requirement: {}",
        resp.message
    );
}

#[tokio::test]
async fn test_update_apply_requires_auth() {
    let srv = TestServer::spawn().await;
    let status = reqwest::Client::new()
        .post(srv.url("/api/v1/admin/update/apply"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 401);
}
