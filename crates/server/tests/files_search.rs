mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::FileRecord;

/// Index a small set of files and return a started server.
async fn setup() -> TestServer {
    let srv = TestServer::spawn().await;

    // Index three files across two paths so we have something to query.
    for (path, content) in [
        ("src/main.rs", "fn main() {}"),
        ("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }"),
        ("docs/README.md", "# My Project"),
    ] {
        srv.post_bulk(&make_text_bulk("home", path, content)).await;
    }
    srv.wait_for_idle().await;
    srv
}

// ── GET /api/v1/files (no q) — unchanged behaviour ───────────────────────────

#[tokio::test]
async fn list_files_no_q_returns_all() {
    let srv = setup().await;
    let records: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", "home")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(records.len(), 3);
    // Full records include mtime (used by find-scan for deletion detection).
    assert!(records.iter().all(|r| r.mtime != 0));
}

// ── GET /api/v1/files?q= — palette search mode ───────────────────────────────

#[tokio::test]
async fn search_files_empty_q_returns_recent() {
    let srv = setup().await;
    let records: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", "home"), ("q", "")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Empty query returns something (recently indexed), not an error.
    assert!(!records.is_empty());
}

#[tokio::test]
async fn search_files_substring_match() {
    let srv = setup().await;
    let records: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", "home"), ("q", "src")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|r| r.path.contains("src")));
}

#[tokio::test]
async fn search_files_case_insensitive() {
    let srv = setup().await;
    let lower: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", "home"), ("q", "readme")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let upper: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", "home"), ("q", "README")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lower.len(), 1);
    assert_eq!(lower.len(), upper.len());
    assert_eq!(lower[0].path, upper[0].path);
}

#[tokio::test]
async fn search_files_no_match_returns_empty() {
    let srv = setup().await;
    let records: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", "home"), ("q", "zzznomatch")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(records.is_empty());
}

#[tokio::test]
async fn search_files_limit_respected() {
    let srv = TestServer::spawn().await;
    // Index 10 files all matching "file".
    for i in 0..10 {
        srv.post_bulk(&make_text_bulk("home", &format!("file{i}.txt"), "content"))
            .await;
    }
    srv.wait_for_idle().await;

    let records: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", "home"), ("q", "file"), ("limit", "3")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(records.len(), 3);
}
