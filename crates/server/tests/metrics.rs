mod helpers;
use helpers::{make_text_bulk, TestServer};

use find_common::api::HotFilesResponse;

async fn index(srv: &TestServer, source: &str, path: &str, content: &str) {
    srv.post_bulk(&make_text_bulk(source, path, content)).await;
    srv.wait_for_idle().await;
}

#[tokio::test]
async fn prometheus_metrics_count_changes_by_source_and_op() {
    let srv = TestServer::spawn().await;

    index(&srv, "docs", "a.txt", "v1").await;
    index(&srv, "docs", "a.txt", "v2").await;
    index(&srv, "other", "b.txt", "x").await;

    let resp = srv.client.get(srv.url("/api/v1/metrics/prometheus")).send().await.unwrap();
    assert!(resp.status().is_success());
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/plain"));
    let body = resp.text().await.unwrap();

    assert!(body.contains(r#"find_anything_files_changed_total{source="docs",op="add"} 1"#), "{body}");
    assert!(body.contains(r#"find_anything_files_changed_total{source="docs",op="modify"} 1"#), "{body}");
    assert!(body.contains(r#"find_anything_files_changed_total{source="other",op="add"} 1"#), "{body}");
    assert!(body.contains(r#"find_anything_files{source="docs"} 1"#), "{body}");
    assert!(body.contains("find_anything_inbox_queue_depth 0"), "{body}");
}

#[tokio::test]
async fn prometheus_metrics_require_auth() {
    let srv = TestServer::spawn().await;
    let status = srv.anonymous_client()
        .get(srv.url("/api/v1/metrics/prometheus"))
        .send().await.unwrap().status();
    assert_eq!(status.as_u16(), 401);
}

#[tokio::test]
async fn hot_files_lists_repeatedly_changed_files() {
    let srv = TestServer::spawn().await;

    for i in 0..3 {
        index(&srv, "docs", "busy.log", &format!("rev {i}")).await;
    }
    index(&srv, "docs", "quiet.txt", "once").await;

    // Adds don't count; busy.log has 2 modifications.
    let r: HotFilesResponse = srv.client
        .get(srv.url("/api/v1/hot-files?min=2"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r.files.len(), 1, "{r:?}");
    assert_eq!((r.files[0].source.as_str(), r.files[0].path.as_str(), r.files[0].changes),
               ("docs", "busy.log", 2));

    // Default floor (5) hides it; other source filter excludes it.
    let r: HotFilesResponse = srv.client
        .get(srv.url("/api/v1/hot-files"))
        .send().await.unwrap().json().await.unwrap();
    assert!(r.files.is_empty());
    let r: HotFilesResponse = srv.client
        .get(srv.url("/api/v1/hot-files?min=1&source=nope"))
        .send().await.unwrap().json().await.unwrap();
    assert!(r.files.is_empty());

    let body = srv.client.get(srv.url("/api/v1/metrics/prometheus"))
        .send().await.unwrap().text().await.unwrap();
    assert!(body.contains(r#"find_anything_hot_files{source="docs"} 0"#), "{body}");
}

#[tokio::test]
async fn hot_files_rejects_bad_params() {
    let srv = TestServer::spawn().await;
    for q in ["window=bogus", "window=30d", "limit=5000"] {
        let status = srv.client.get(srv.url(&format!("/api/v1/hot-files?{q}")))
            .send().await.unwrap().status();
        assert_eq!(status.as_u16(), 400, "{q}");
    }
}
