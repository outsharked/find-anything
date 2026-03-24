mod helpers;
use helpers::TestServer;

use find_common::api::{BulkRequest, FileKind, IndexFile, IndexLine, SCANNER_VERSION};

// Sample DICOM fixture (MR_small.dcm from pydicom, MIT licensed).
// Embedded so the test binary is self-contained.
const MR_SMALL: &[u8] = include_bytes!(
    "../../extractors/dicom/tests/fixtures/MR_small.dcm"
);

/// Spawn a TestServer with a temporary source directory.
async fn srv_with_source(dir: &std::path::Path) -> TestServer {
    let path_str = dir.to_str().unwrap().replace('\\', "/");
    let extra = format!("[sources.files]\npath = \"{path_str}\"\n");
    TestServer::spawn_with_extra_config(&extra).await
}

/// Index a DICOM file into the test server's DB.
async fn index_dicom(srv: &TestServer, path: &str) {
    let req = BulkRequest {
        source: "files".to_string(),
        files: vec![IndexFile {
            path: path.to_string(),
            mtime: 1_700_000_000,
            size: Some(MR_SMALL.len() as i64),
            kind: FileKind::Dicom,
            lines: vec![IndexLine {
                archive_path: None,
                line_number: 0,
                content: format!("[PATH] {path}"),
            }],
            extract_ms: None,
            file_hash: None,
            scanner_version: SCANNER_VERSION,
            is_new: true,
        }],
        delete_paths: vec![],
        scan_timestamp: Some(1_700_000_000),
        indexing_failures: vec![],
        rename_paths: vec![],
    };
    srv.post_bulk(&req).await;
    srv.wait_for_idle().await;
}

// ── Auth ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn view_requires_auth() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = reqwest::Client::new()
        .get(srv.url("/api/v1/view?source=files&path=scan.dcm"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 401);
}

// ── Path validation ───────────────────────────────────────────────────────────
// Malicious paths are not in the DB, so the view endpoint returns 404 rather
// than 400.  (Path traversal validation runs *after* the DB kind lookup.)

#[tokio::test]
async fn view_leading_slash_returns_404() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/view?source=files&path=/etc/passwd"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}

#[tokio::test]
async fn view_path_traversal_returns_404() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/view?source=files&path=..%2F..%2Fetc%2Fpasswd"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}

// ── Missing source / file ─────────────────────────────────────────────────────

#[tokio::test]
async fn view_unknown_source_returns_404() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/view?source=nosuchsource&path=scan.dcm"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}

#[tokio::test]
async fn view_missing_file_returns_404() {
    let dir = tempfile::TempDir::new().unwrap();
    let srv = srv_with_source(dir.path()).await;

    let status = srv
        .client
        .get(srv.url("/api/v1/view?source=files&path=doesnotexist.dcm"))
        .send()
        .await
        .unwrap()
        .status();

    assert_eq!(status.as_u16(), 404);
}

// ── PNG conversion ────────────────────────────────────────────────────────────

/// Returns the path to `find-preview-dicom`, trying sibling directories of the
/// current test binary. Returns `None` if it cannot be found.
fn find_preview_binary() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("find-preview-dicom");
            if candidate.exists() { return Some(candidate); }
            if let Some(parent) = dir.parent() {
                let candidate = parent.join("find-preview-dicom");
                if candidate.exists() { return Some(candidate); }
            }
        }
    }
    // Also check PATH entries.
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("find-preview-dicom");
            if candidate.exists() { return Some(candidate); }
        }
    }
    None
}

#[tokio::test]
async fn view_returns_png_for_valid_dicom() {
    if find_preview_binary().is_none() {
        eprintln!("SKIP view_returns_png_for_valid_dicom: find-preview-dicom not built");
        return;
    }

    let dir = tempfile::TempDir::new().unwrap();
    let dcm_path = dir.path().join("MR_small.dcm");
    std::fs::write(&dcm_path, MR_SMALL).unwrap();

    let srv = srv_with_source(dir.path()).await;
    index_dicom(&srv, "MR_small.dcm").await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/view?source=files&path=MR_small.dcm"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200, "expected 200 OK");

    let ct = resp.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "image/png", "expected PNG content-type, got: {ct}");

    let body = resp.bytes().await.unwrap();
    // PNG magic: \x89PNG\r\n\x1a\n
    assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"),
        "response body must start with PNG magic bytes");
    assert!(body.len() > 100, "PNG body suspiciously small: {} bytes", body.len());
}

#[tokio::test]
async fn view_extensionless_dicom_returns_png() {
    if find_preview_binary().is_none() {
        eprintln!("SKIP view_extensionless_dicom_returns_png: find-preview-dicom not built");
        return;
    }

    let dir = tempfile::TempDir::new().unwrap();
    let dcm_path = dir.path().join("MR_scan_no_ext");
    std::fs::write(&dcm_path, MR_SMALL).unwrap();

    let srv = srv_with_source(dir.path()).await;
    index_dicom(&srv, "MR_scan_no_ext").await;

    let resp = srv
        .client
        .get(srv.url("/api/v1/view?source=files&path=MR_scan_no_ext"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.bytes().await.unwrap();
    assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"), "expected PNG magic");
}
