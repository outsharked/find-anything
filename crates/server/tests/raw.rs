mod helpers;
use helpers::TestServer;

use std::io::Write as _;

// ── helpers ────────────────────────────────────────────────────────────────────

/// Spawn a TestServer with a source named "files" whose root is `dir`.
async fn srv_with_source(dir: &std::path::Path) -> TestServer {
    let path_str = dir.to_str().unwrap().replace('\\', "/");
    let extra = format!("[sources.files]\npath = \"{path_str}\"\n");
    TestServer::spawn_with_extra_config(&extra).await
}

// ── auth ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn raw_requires_auth() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/raw?source=files&path=anything.txt"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401);
}

// ── path validation ────────────────────────────────────────────────────────────

#[tokio::test]
async fn raw_leading_slash_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=/etc/passwd"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400);
}

#[tokio::test]
async fn raw_path_traversal_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=..%2F..%2Fetc%2Fpasswd"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 400);
}

// ── source not configured ──────────────────────────────────────────────────────

#[tokio::test]
async fn raw_unknown_source_returns_404() {
    let srv = TestServer::spawn().await; // no source configured

    let status = srv
        .client
        .get(srv.url("/api/v1/raw?source=nosuchsource&path=file.txt"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}

// ── file not found ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn raw_missing_file_returns_404() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=doesnotexist.txt"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}

// ── successful file serve ──────────────────────────────────────────────────────

#[tokio::test]
async fn raw_serves_file_content() {
    let dir = tempfile::TempDir::new().unwrap();
    let file_path = dir.path().join("hello.txt");
    std::fs::write(&file_path, b"hello world content").unwrap();

    let srv = srv_with_source(dir.path()).await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=hello.txt"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), b"hello world content");
}

#[tokio::test]
async fn raw_content_type_from_extension() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("page.html"), b"<html></html>").unwrap();

    let srv = srv_with_source(dir.path()).await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=page.html"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("html"), "expected html content-type, got: {ct}");
}

#[tokio::test]
async fn raw_download_param_sets_attachment_disposition() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("report.txt"), b"data").unwrap();

    let srv = srv_with_source(dir.path()).await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=report.txt&download=1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let disp = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(disp.starts_with("attachment"), "expected attachment disposition, got: {disp}");
}

// ── byte-range requests ────────────────────────────────────────────────────────

#[tokio::test]
async fn raw_range_request_returns_206() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("data.bin"), b"abcdefghijklmnopqrstuvwxyz").unwrap();

    let srv = srv_with_source(dir.path()).await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=data.bin"))
        .header("Range", "bytes=0-4")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 206);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), b"abcde");
}

#[tokio::test]
async fn raw_range_out_of_bounds_returns_416() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("small.txt"), b"hi").unwrap();

    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=small.txt"))
        .header("Range", "bytes=999-9999")
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 416);
}

// ── archive member serving ─────────────────────────────────────────────────────

#[tokio::test]
async fn raw_serves_zip_member() {
    use std::io::Cursor;

    let dir = tempfile::TempDir::new().unwrap();

    // Build a small ZIP with one text member.
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("readme.txt", opts).unwrap();
        zip.write_all(b"zip member content").unwrap();
        zip.finish().unwrap();
    }
    std::fs::write(dir.path().join("archive.zip"), &buf).unwrap();

    let srv = srv_with_source(dir.path()).await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=archive.zip%3A%3Areadme.txt"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), b"zip member content");
}

#[tokio::test]
async fn raw_zip_member_not_found_returns_404() {
    use std::io::Cursor;

    let dir = tempfile::TempDir::new().unwrap();
    let mut buf = Vec::new();
    {
        let zip = zip::ZipWriter::new(Cursor::new(&mut buf));
        zip.finish().unwrap(); // empty ZIP
    }
    std::fs::write(dir.path().join("empty.zip"), &buf).unwrap();

    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/raw?source=files&path=empty.zip%3A%3Anomember.txt"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}
