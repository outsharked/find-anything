//! End-to-end coverage for issue #4: changes to `.index`/`.noindex` control
//! files must trigger a rescan of the affected directory, not be indexed as
//! regular file content. Exercises the real `find-scan` binary against a
//! live `TestServer`, mirroring how `find-watch` delegates rescans.

mod helpers;
use helpers::TestServer;

use find_common::api::FileRecord;
use std::path::{Path, PathBuf};

/// Resolve the `find-scan` binary the same way the server's own upload
/// delegation does (`crates/server/src/upload.rs::resolve_find_scan`): the
/// test binary lives in `target/debug/deps/`, but `find-scan` is built into
/// `target/debug/` (its parent).
fn find_scan_binary() -> PathBuf {
    let name = if cfg!(windows) { "find-scan.exe" } else { "find-scan" };
    let exe = std::env::current_exe().expect("current_exe");
    let deps_dir = exe.parent().expect("deps dir");
    let candidate = deps_dir.join(name);
    if candidate.exists() {
        return candidate;
    }
    deps_dir.parent().expect("target/debug dir").join(name)
}

/// Run `find-scan --config <config_path> [extra_arg]` and assert success.
async fn run_find_scan(config_path: &Path, extra_arg: Option<&Path>) {
    let mut cmd = tokio::process::Command::new(find_scan_binary());
    cmd.arg("--config").arg(config_path);
    if let Some(p) = extra_arg {
        cmd.arg(p);
    }
    let output = cmd.output().await.expect("spawn find-scan");
    assert!(
        output.status.success(),
        "find-scan failed (status={:?})\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Sorted list of indexed paths for `source`.
async fn list_paths(srv: &TestServer, source: &str) -> Vec<String> {
    let records: Vec<FileRecord> = srv
        .client
        .get(srv.url("/api/v1/files"))
        .query(&[("source", source)])
        .send()
        .await
        .expect("files request")
        .json()
        .await
        .expect("files json");
    let mut paths: Vec<String> = records.into_iter().map(|r| r.path).collect();
    paths.sort();
    paths
}

fn write_client_config(config_path: &Path, server_url: &str, source_path: &Path) {
    std::fs::write(
        config_path,
        format!(
            "[server]\nurl = \"{server_url}\"\ntoken = \"{}\"\n\n\
             [[sources]]\nname = \"test\"\npath = \"{}\"\n\n\
             [scan]\nexclude = []\n",
            helpers::TEST_TOKEN,
            source_path.display(),
        ),
    )
    .expect("write client.toml");
}

#[tokio::test]
async fn noindex_control_file_rescans_ancestor_and_removes_sibling_files() {
    let srv = TestServer::spawn().await;

    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("root.txt"), "root content").unwrap();
    std::fs::write(src.join("sub/a.txt"), "alpha content").unwrap();
    std::fs::write(src.join("sub/b.txt"), "beta content").unwrap();

    let config_path = tmp.path().join("client.toml");
    write_client_config(&config_path, &srv.base_url, &src);

    // Initial full scan indexes all three files.
    run_find_scan(&config_path, None).await;
    srv.wait_for_idle().await;
    assert_eq!(
        list_paths(&srv, "test").await,
        vec!["root.txt", "sub/a.txt", "sub/b.txt"]
    );

    // Add sub/.noindex and scan the control file path directly. This must
    // rescan sub's *ancestor* (src/, since the walker only evaluates
    // .noindex-presence for non-root walk entries) and delete sub's files —
    // not index the control file's own (empty) content.
    let noindex_path = src.join("sub/.noindex");
    std::fs::write(&noindex_path, "").unwrap();
    run_find_scan(&config_path, Some(&noindex_path)).await;
    srv.wait_for_idle().await;
    assert_eq!(
        list_paths(&srv, "test").await,
        vec!["root.txt"],
        "sub/a.txt and sub/b.txt must be removed once sub/.noindex excludes them, \
         and sub/.noindex itself must never be indexed as content"
    );

    // Remove .noindex and rescan the source root — the files must reappear.
    std::fs::remove_file(&noindex_path).unwrap();
    run_find_scan(&config_path, None).await;
    srv.wait_for_idle().await;
    assert_eq!(
        list_paths(&srv, "test").await,
        vec!["root.txt", "sub/a.txt", "sub/b.txt"]
    );
}

#[tokio::test]
async fn index_control_file_rescans_ancestor_and_applies_include_filter() {
    let srv = TestServer::spawn().await;

    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("root.txt"), "root content").unwrap();

    let config_path = tmp.path().join("client.toml");
    write_client_config(&config_path, &srv.base_url, &src);

    // Initial scan indexes just the root file — sub/ doesn't exist yet.
    run_find_scan(&config_path, None).await;
    srv.wait_for_idle().await;
    assert_eq!(list_paths(&srv, "test").await, vec!["root.txt"]);

    // Create sub/ with a.txt, b.txt, and a .index restricting sub/ to only
    // a.txt, then scan the control file path directly.
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("sub/a.txt"), "alpha content").unwrap();
    std::fs::write(src.join("sub/b.txt"), "beta content").unwrap();
    let index_path = src.join("sub/.index");
    std::fs::write(&index_path, "include = [\"a.txt\"]\n").unwrap();

    run_find_scan(&config_path, Some(&index_path)).await;
    srv.wait_for_idle().await;
    assert_eq!(
        list_paths(&srv, "test").await,
        vec!["root.txt", "sub/a.txt"],
        "sub/b.txt must stay excluded by sub/.index's include filter, \
         and sub/.index itself must never be indexed as content"
    );
}
