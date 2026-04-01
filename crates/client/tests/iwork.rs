/// Integration tests for iWork (.pages, .numbers, .key) document handling.
///
/// iWork text extraction is built into the archive extractor — no external
/// binary or configuration is needed.  `test.pages` is a minimal but genuine
/// iWork ZIP containing a known IWA document and a preview JPEG.
///
/// Covers:
///   I1. Native IWA text extraction: content is indexed and searchable
///       with default scan config (no extractor configuration required).
///   I2. [IWORK_PREVIEW] metadata line is stored in file metadata.
///   I3. `server_only` config uploads the file; the server runs find-scan
///       (which uses the built-in extractor) and content is indexed.
///   I4. /api/v1/view serves the preview JPEG via the iwork_parent_kind
///       fallback (composite path not in DB).
mod helpers;
use helpers::{fixtures_dir, TestEnv};

/// Build a minimal ZIP (using zip v2 API) containing `test.pages` at the given inner path.
fn make_zip_with_pages(inner_path: &str, pages_bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::{ZipWriter, SimpleFileOptions};
    let mut buf = Vec::new();
    let cursor = std::io::Cursor::new(&mut buf);
    let mut zip = ZipWriter::new(cursor);
    zip.start_file(inner_path, SimpleFileOptions::default()).unwrap();
    zip.write_all(pages_bytes).unwrap();
    zip.finish().unwrap();
    buf
}

// ── I1 — Native IWA text extraction is indexed and searchable ────────────────

#[tokio::test]
async fn i1_native_extractor_text_content_indexed() {
    let env = TestEnv::new().await;
    let fixtures = fixtures_dir();

    let pages_bytes = std::fs::read(format!("{fixtures}/test.pages")).expect("read test.pages");
    env.write_file_bytes("test.pages", &pages_bytes);

    env.run_scan().await;

    let results = env.search("iwork native unique test content").await;
    assert!(
        results.iter().any(|r| r.path == "test.pages"),
        "native IWA text not found in search results: {results:?}"
    );
}

// ── I2 — [IWORK_PREVIEW] metadata line is stored ─────────────────────────────

#[tokio::test]
async fn i2_iwork_preview_metadata_line_stored() {
    let env = TestEnv::new().await;
    let fixtures = fixtures_dir();

    let pages_bytes = std::fs::read(format!("{fixtures}/test.pages")).expect("read test.pages");
    env.write_file_bytes("test.pages", &pages_bytes);

    env.run_scan().await;

    // [IWORK_PREVIEW] is at LINE_METADATA=1 → stored in FileResponse.metadata, not .lines.
    let metadata = env.get_file_metadata("test.pages").await;
    assert!(
        metadata.iter().any(|l| l.contains("[IWORK_PREVIEW]")),
        "[IWORK_PREVIEW] not found in stored metadata: {metadata:?}"
    );
    assert!(
        metadata.iter().any(|l| l.contains("preview.jpg")),
        "preview.jpg missing from stored metadata: {metadata:?}"
    );
}

// ── I3 — server_only uploads file; server uses built-in extractor ─────────────

/// When the client marks `pages` as `server_only`, find-scan uploads the file
/// to the server.  The server spawns find-scan with the built-in archive
/// extractor, which natively extracts IWA text — no extractor binary needed.
///
/// NOTE: requires `find-scan` to be compiled (target/debug/find-scan).
#[tokio::test]
async fn i3_server_only_upload_indexes_content() {
    let env = TestEnv::new().await;
    let fixtures = fixtures_dir();

    let pages_bytes = std::fs::read(format!("{fixtures}/test.pages")).expect("read test.pages");
    env.write_file_bytes("test.pages", &pages_bytes);

    let scan = env.scan_config_with(|cfg| {
        cfg.extractors.insert(
            "pages".to_string(),
            find_common::config::ExtractorEntry::Builtin("server_only".to_string()),
        );
    });
    env.run_scan_with(scan).await;

    // The server spawns find-scan asynchronously outside the worker queue.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    env.server.wait_for_idle().await;

    let results = env.search("iwork native unique test content").await;
    assert!(
        results.iter().any(|r| r.path == "test.pages"),
        "server_only: content not indexed after server-side extraction.\n\
         Check that find-scan is built.\nResults: {results:?}"
    );
}

// ── I4 — /api/v1/view serves the preview JPEG via iwork_parent_kind ──────────

/// The view endpoint must serve the embedded JPEG when requested as
/// `test.pages::preview.jpg` even though that composite path has no DB entry.
/// The server uses the iwork_parent_kind fallback to detect this case.
#[tokio::test]
async fn i4_view_endpoint_serves_iwork_preview_jpeg() {
    let env = TestEnv::new().await;
    let fixtures = fixtures_dir();

    let pages_bytes = std::fs::read(format!("{fixtures}/test.pages")).expect("read test.pages");
    env.write_file_bytes("test.pages", &pages_bytes);

    env.run_scan().await;

    let metadata = env.get_file_metadata("test.pages").await;
    assert!(
        metadata.iter().any(|l| l.contains("[IWORK_PREVIEW]")),
        "prerequisite failed: [IWORK_PREVIEW] not in stored metadata: {metadata:?}"
    );

    let url = env.server.url(&format!(
        "/api/v1/view?source={}&path=test.pages%3A%3Apreview.jpg",
        env.source_name
    ));
    let resp = env.server.client.get(&url).send().await.unwrap();

    assert_eq!(resp.status().as_u16(), 200, "expected 200 from view endpoint");

    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("image/"), "expected image/* content-type, got: {ct}");

    let body = resp.bytes().await.unwrap();
    assert!(
        body.len() >= 3 && body[0] == 0xFF && body[1] == 0xD8 && body[2] == 0xFF,
        "expected JPEG magic bytes FF D8 FF (got {} bytes, first 4: {:?})",
        body.len(),
        &body[..body.len().min(4)]
    );
}

// ── I5 — Top-level .pages file must not be stored as kind=archive ─────────────

/// iWork files (.pages/.numbers/.key) are documents, not user-navigable archives.
/// Storing them as kind=archive causes the tree UI to show them as expandable nodes
/// instead of leaf nodes.  After scanning, the file_kind must be "document" (or any
/// non-archive kind); "archive" is wrong.
#[tokio::test]
async fn i5_top_level_pages_kind_is_not_archive() {
    let env = TestEnv::new().await;
    let fixtures = fixtures_dir();

    let pages_bytes = std::fs::read(format!("{fixtures}/test.pages")).expect("read test.pages");
    env.write_file_bytes("test.pages", &pages_bytes);
    env.run_scan().await;

    let resp = env.get_file_response("test.pages", None).await;
    assert_ne!(
        resp.file_kind.to_string(), "archive",
        "test.pages must not be stored as kind=archive — the tree UI would make it expandable"
    );
}

// ── I6 — .pages nested inside a ZIP should have [IWORK_PREVIEW] metadata ─────

/// When a .pages file is a member of a ZIP archive, the archive extractor should
/// treat it as a nested iWork document: extract the preview JPEG and IWA text.
/// The member entry must have [IWORK_PREVIEW] in its metadata (LINE_METADATA slot).
#[tokio::test]
async fn i6_pages_in_zip_has_preview_metadata() {
    let env = TestEnv::new().await;
    let fixtures = fixtures_dir();

    let pages_bytes = std::fs::read(format!("{fixtures}/test.pages")).expect("read test.pages");
    let zip_bytes = make_zip_with_pages("doc.pages", &pages_bytes);
    env.write_file_bytes("outer.zip", &zip_bytes);
    env.run_scan().await;

    // The .pages member is at archive_path "doc.pages" inside "outer.zip".
    let resp = env.get_file_response("outer.zip", Some("doc.pages")).await;
    assert!(
        resp.metadata.iter().any(|m| m.contains("[IWORK_PREVIEW]")),
        ".pages member inside ZIP must have [IWORK_PREVIEW] metadata; got: {:?}",
        resp.metadata
    );
}

// ── I7 — .pages nested inside a ZIP: IWA text must be searchable ─────────────

/// When a .pages file is a member of a ZIP archive, the IWA text content extracted
/// from the embedded document must be indexed and searchable.
#[tokio::test]
async fn i7_pages_in_zip_text_is_searchable() {
    let env = TestEnv::new().await;
    let fixtures = fixtures_dir();

    let pages_bytes = std::fs::read(format!("{fixtures}/test.pages")).expect("read test.pages");
    let zip_bytes = make_zip_with_pages("doc.pages", &pages_bytes);
    env.write_file_bytes("outer.zip", &zip_bytes);
    env.run_scan().await;

    let results = env.search("iwork native unique test content").await;
    // The result path may be "outer.zip" (the outer archive) or "outer.zip::doc.pages"
    // depending on how content is stored, but the text must be findable.
    assert!(
        !results.is_empty(),
        ".pages text inside ZIP must be searchable; got no results"
    );
}
