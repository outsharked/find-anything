mod helpers;
use helpers::TestEnv;

use find_common::api::{FileKind, SCANNER_VERSION};
use find_common::config::{ExtractorEntry, ExternalExtractorConfig, ExternalExtractorMode};

// ── S1 — Text file is indexed and searchable ─────────────────────────────────

#[tokio::test]
async fn s1_text_file_indexed_and_searchable() {
    let env = TestEnv::new().await;
    env.write_file("hello.txt", "unique_phrase_xyzzy this is a test file");
    env.run_scan().await;

    let results = env.search("unique_phrase_xyzzy").await;
    assert_eq!(results.len(), 1, "expected exactly 1 result");
    assert_eq!(results[0].path, "hello.txt");
}

// ── S2 — Multiple files, all indexed ─────────────────────────────────────────

#[tokio::test]
async fn s2_multiple_files_all_indexed() {
    let env = TestEnv::new().await;
    env.write_file("a.txt", "content_alpha_aaa");
    env.write_file("b.md", "content_beta_bbb");
    env.write_file("c.rs", "// content_gamma_ccc");
    env.run_scan().await;

    let files = env.list_files().await;
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"a.txt"), "a.txt missing: {paths:?}");
    assert!(paths.contains(&"b.md"), "b.md missing: {paths:?}");
    assert!(paths.contains(&"c.rs"), "c.rs missing: {paths:?}");

    assert!(!env.search("content_alpha_aaa").await.is_empty());
    assert!(!env.search("content_beta_bbb").await.is_empty());
    assert!(!env.search("content_gamma_ccc").await.is_empty());
}

// ── S3 — Unchanged file is not re-indexed ────────────────────────────────────

#[tokio::test]
async fn s3_unchanged_file_not_reindexed() {
    let env = TestEnv::new().await;
    env.write_file("stable.txt", "stable content here");
    env.run_scan().await;

    let before = env
        .list_files()
        .await
        .into_iter()
        .find(|f| f.path == "stable.txt")
        .expect("stable.txt not found after first scan");

    // Second scan — mtime unchanged, so file should be skipped.
    env.run_scan().await;

    let after = env
        .list_files()
        .await
        .into_iter()
        .find(|f| f.path == "stable.txt")
        .expect("stable.txt not found after second scan");

    assert_eq!(
        before.indexed_at, after.indexed_at,
        "indexed_at changed on second scan — file was re-indexed unnecessarily"
    );
}

// ── S4 — Modified file is re-indexed ─────────────────────────────────────────

#[tokio::test]
async fn s4_modified_file_is_reindexed() {
    let env = TestEnv::new().await;
    let path = env.write_file("changing.txt", "version one content here");
    env.run_scan().await;

    assert!(!env.search("version one content here").await.is_empty());

    // Bump mtime by setting it 2 seconds in the future.
    let new_mtime = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    let mtime = filetime::FileTime::from_system_time(new_mtime);
    filetime::set_file_mtime(&path, mtime).expect("set mtime");
    std::fs::write(&path, "version two content here").expect("overwrite file");

    env.run_scan().await;

    assert!(
        env.search("version two content here").await.len() >= 1,
        "version two not found"
    );
    assert!(
        env.search("version one content here").await.is_empty(),
        "version one still present after re-index"
    );
}

// ── S5 — Deleted file is removed from index ──────────────────────────────────

#[tokio::test]
async fn s5_deleted_file_removed_from_index() {
    let env = TestEnv::new().await;
    env.write_file("doomed.txt", "doomed_content_qqq");
    env.run_scan().await;

    assert!(!env.search("doomed_content_qqq").await.is_empty());

    env.remove_file("doomed.txt");
    env.run_scan().await;

    let files = env.list_files().await;
    assert!(
        !files.iter().any(|f| f.path == "doomed.txt"),
        "doomed.txt still in index after deletion"
    );
    assert!(
        env.search("doomed_content_qqq").await.is_empty(),
        "doomed_content_qqq still searchable after file deletion"
    );
}

// ── S6 — Exclude patterns respected ──────────────────────────────────────────

#[tokio::test]
async fn s6_exclude_patterns_respected() {
    let env = TestEnv::new().await;
    env.write_file("included.txt", "included_file_content");
    // .git/ is in the default exclude list.
    env.write_file(".git/config", "[core]\nrepositoryformatversion = 0");
    env.run_scan().await;

    let files = env.list_files().await;
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"included.txt"), "included.txt missing");
    assert!(
        !paths.iter().any(|p| p.starts_with(".git/")),
        ".git/ contents should be excluded: {paths:?}"
    );
}

// ── S7 — ZIP members are indexed as composite paths ──────────────────────────

#[tokio::test]
async fn s7_zip_members_indexed_as_composite_paths() {
    let env = TestEnv::new().await;

    // Build a minimal ZIP in memory and write it to the source dir.
    let zip_bytes = {
        use std::io::Write;
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file("readme.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"archive_content_xyz this is inside the zip")
            .unwrap();
        zip.finish().unwrap().into_inner()
    };
    env.write_file_bytes("test.zip", &zip_bytes);
    env.run_scan().await;

    // Search results use path="test.zip" + archive_path="readme.txt" for backward compat.
    let results = env.search("archive_content_xyz").await;
    assert!(
        results.iter().any(|r| r.path == "test.zip" && r.archive_path.as_deref() == Some("readme.txt")),
        "expected test.zip::readme.txt in search results: {results:?}"
    );

    // The files table stores the composite path directly.
    let files = env.list_files().await;
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"test.zip::readme.txt"),
        "expected composite path test.zip::readme.txt in file list: {paths:?}"
    );
}

// ── S8 — External extractor (tempdir mode) ───────────────────────────────────

#[tokio::test]
async fn s8_external_extractor_tempdir_mode() {
    let env = TestEnv::new().await;
    let fixtures = helpers::fixtures_dir();

    // Copy test.nd1 fixture into the source directory.
    let fixture_nd1 = std::path::Path::new(&fixtures).join("test.nd1");
    let nd1_bytes = std::fs::read(&fixture_nd1).expect("read test.nd1");
    env.write_file_bytes("test.nd1", &nd1_bytes);

    let extractor_bin = std::path::Path::new(&fixtures)
        .join("find-extract-nd1")
        .to_string_lossy()
        .to_string();

    let scan = env.scan_config_with(|cfg| {
        cfg.extractors.insert(
            "nd1".to_string(),
            ExtractorEntry::External(ExternalExtractorConfig {
                mode: ExternalExtractorMode::TempDir,
                bin: extractor_bin,
                args: vec!["{file}".to_string(), "{dir}".to_string()],
            }),
        );
    });

    env.run_scan_with(scan).await;

    let files = env.list_files().await;
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

    // The fixture has 5 members: readme.txt, notes.txt, data.json, report.md, empty.txt.
    for member in &["readme.txt", "notes.txt", "data.json", "report.md", "empty.txt"] {
        let composite = format!("test.nd1::{member}");
        assert!(
            paths.contains(&composite.as_str()),
            "missing member {composite} in: {paths:?}"
        );
    }
}

// ── S9 — External extractor (stdout mode) ────────────────────────────────────

#[tokio::test]
async fn s9_external_extractor_stdout_mode() {
    let env = TestEnv::new().await;
    let fixtures = helpers::fixtures_dir();

    let fixture_nd1 = std::path::Path::new(&fixtures).join("test.nd1");
    let nd1_bytes = std::fs::read(&fixture_nd1).expect("read test.nd1");
    env.write_file_bytes("test.nd1", &nd1_bytes);

    let extractor_bin = std::path::Path::new(&fixtures)
        .join("find-extract-nd1-stdout")
        .to_string_lossy()
        .to_string();

    let scan = env.scan_config_with(|cfg| {
        cfg.extractors.insert(
            "nd1".to_string(),
            ExtractorEntry::External(ExternalExtractorConfig {
                mode: ExternalExtractorMode::Stdout,
                bin: extractor_bin,
                args: vec!["{file}".to_string()],
            }),
        );
    });

    env.run_scan_with(scan).await;

    let files = env.list_files().await;
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

    // In stdout mode, content is indexed under the outer path (no composite paths).
    assert!(
        paths.contains(&"test.nd1"),
        "test.nd1 not indexed: {paths:?}"
    );
    // No composite paths should exist for stdout mode.
    assert!(
        !paths.iter().any(|p| p.starts_with("test.nd1::")),
        "unexpected composite paths in stdout mode: {paths:?}"
    );
    // Content from the fixture should be searchable.
    let results = env.search("plain text note").await;
    assert!(
        results.iter().any(|r| r.path == "test.nd1"),
        "stdout content not searchable: {results:?}"
    );
}

// ── S10 — `--force` re-indexes all files ─────────────────────────────────────

#[tokio::test]
async fn s10_force_reindexes_all_files() {
    let env = TestEnv::new().await;
    env.write_file("force.txt", "force reindex test content");
    env.run_scan().await;

    let before = env
        .list_files()
        .await
        .into_iter()
        .find(|f| f.path == "force.txt")
        .expect("force.txt not found");

    // Small sleep to ensure indexed_at timestamp can advance.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // force_since = current time: re-indexes all files with indexed_at < now.
    let force_since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let api = env.api_client();
    let paths = vec![env.source_dir.path().to_string_lossy().to_string()];
    let source = find_client::scan::ScanSource {
        name: &env.source_name,
        paths: &paths,
        include: &[],
        subdir: None,
    };
    let opts = find_client::scan::ScanOptions {
        upgrade: false,
        quiet: true,
        dry_run: false,
        force_since: Some(force_since),
        mtime_override: None,
        force_index: false,
    };
    find_client::scan::run_scan(&api, &source, &env.scan_config(), &opts)
        .await
        .expect("force scan failed");
    env.server.wait_for_idle().await;

    let after = env
        .list_files()
        .await
        .into_iter()
        .find(|f| f.path == "force.txt")
        .expect("force.txt not found after force scan");

    assert!(
        after.indexed_at > before.indexed_at,
        "indexed_at did not advance after --force scan (before={:?}, after={:?})",
        before.indexed_at,
        after.indexed_at
    );
}

// ── S11 — `--upgrade` re-indexes outdated scanner versions ───────────────────

#[tokio::test]
async fn s11_upgrade_reindexes_old_scanner_version() {
    use find_common::api::{BulkRequest, IndexFile, IndexLine};

    let env = TestEnv::new().await;

    // Write the file so it exists on disk (needed for the scan to process it).
    env.write_file("upgrade.txt", "upgrade test content here");

    // Submit the file manually with scanner_version = 0 (always outdated).
    let old_bulk = BulkRequest {
        source: env.source_name.clone(),
        files: vec![IndexFile {
            path: "upgrade.txt".to_string(),
            mtime: 1_000_000,
            size: Some(27),
            kind: FileKind::Text,
            lines: vec![
                IndexLine { archive_path: None, line_number: 0, content: "upgrade.txt".to_string() },
                IndexLine { archive_path: None, line_number: 1, content: "upgrade test content here".to_string() },
            ],
            extract_ms: None,
            file_hash: None,
            scanner_version: 0, // intentionally old
            is_new: true,
            force: false,
        }],
        delete_paths: vec![],
        scan_timestamp: None,
        indexing_failures: vec![],
        rename_paths: vec![],
    };

    // Post the bulk request directly using reqwest to bypass the version check.
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    let json = serde_json::to_vec(&old_bulk).unwrap();
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&json).unwrap();
    let gz = enc.finish().unwrap();
    let status = env
        .server
        .client
        .post(env.server.url("/api/v1/bulk"))
        .header("Content-Encoding", "gzip")
        .header("Content-Type", "application/json")
        .body(gz)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status.as_u16(), 202);
    env.server.wait_for_idle().await;

    // Verify it has scanner_version = 0.
    let before = env
        .list_files()
        .await
        .into_iter()
        .find(|f| f.path == "upgrade.txt")
        .expect("upgrade.txt not found after manual submission");
    assert_eq!(before.scanner_version, 0, "expected scanner_version=0");

    // Run with --upgrade.
    let api = env.api_client();
    let paths = vec![env.source_dir.path().to_string_lossy().to_string()];
    let source = find_client::scan::ScanSource {
        name: &env.source_name,
        paths: &paths,
        include: &[],
        subdir: None,
    };
    let opts = find_client::scan::ScanOptions {
        upgrade: true,
        quiet: true,
        dry_run: false,
        force_since: None,
        mtime_override: None,
        force_index: false,
    };
    find_client::scan::run_scan(&api, &source, &env.scan_config(), &opts)
        .await
        .expect("upgrade scan failed");
    env.server.wait_for_idle().await;

    let after = env
        .list_files()
        .await
        .into_iter()
        .find(|f| f.path == "upgrade.txt")
        .expect("upgrade.txt not found after upgrade scan");
    assert_eq!(
        after.scanner_version, SCANNER_VERSION,
        "expected scanner_version={SCANNER_VERSION} after --upgrade"
    );
}
