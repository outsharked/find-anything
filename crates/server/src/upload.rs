/// Server-side file upload state management and find-scan delegation.
///
/// Files are received as chunked uploads (via PATCH requests) and assembled
/// into `.part` files in `data_dir/uploads/`. A companion `.meta` JSON file
/// records source, path, mtime, total size, and client scan hints.
///
/// When all bytes are received, the server places the file under a unique temp
/// directory, writes a minimal client.toml, and invokes `find-scan` to handle
/// extraction using its full pipeline (correct timeouts, TempDir mode, archive
/// members, file_hash). find-scan submits the result through the normal bulk
/// path; no special-casing is needed on the server.
///
/// A background cleanup task removes stale uploads (no activity for 2 hours).
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use find_common::api::UploadScanHints;
use find_common::config::{ExternalExtractorConfig, ExternalExtractorMode, ServerScanConfig};

/// Sidecar metadata file for an in-progress upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadMeta {
    pub source: String,
    pub rel_path: String,
    pub mtime: i64,
    pub total_size: u64,
    pub created_at: i64,
    /// Client scan hints forwarded from the upload init request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_hints: Option<UploadScanHints>,
}

pub fn uploads_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("uploads")
}

pub fn meta_path(uploads: &Path, id: &str) -> PathBuf {
    uploads.join(format!("{id}.meta"))
}

pub fn part_path(uploads: &Path, id: &str) -> PathBuf {
    uploads.join(format!("{id}.part"))
}

/// Read the upload meta for `id`. Returns None if not found.
pub fn read_meta(uploads: &Path, id: &str) -> Option<UploadMeta> {
    let path = meta_path(uploads, id);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write upload meta to disk.
pub fn write_meta(uploads: &Path, id: &str, meta: &UploadMeta) -> Result<()> {
    let path = meta_path(uploads, id);
    let json = serde_json::to_string(meta)?;
    std::fs::write(&path, json.as_bytes()).context("writing upload meta")?;
    Ok(())
}

/// Returns the current size of the `.part` file, or 0 if absent.
pub fn part_size(uploads: &Path, id: &str) -> u64 {
    part_path(uploads, id)
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Touch the `.meta` file so the cleanup task can track activity.
pub fn touch_meta(uploads: &Path, id: &str) {
    let path = meta_path(uploads, id);
    if let Ok(content) = std::fs::read(&path) {
        let _ = std::fs::write(&path, content);
    }
}

/// Delegate extraction of an uploaded file to `find-scan`.
///
/// Called after the final PATCH chunk is received. Creates a unique temp
/// directory, places the file at its relative path within it, writes a
/// minimal client.toml, and spawns find-scan. Cleans up temp files on exit.
pub async fn index_upload(
    id: String,
    meta: UploadMeta,
    data_dir: PathBuf,
    server_url: String,
    token: String,
    server_scan: ServerScanConfig,
) {
    let uploads = uploads_dir(&data_dir);
    let part = part_path(&uploads, &id);

    let result = run_index_upload(&id, &meta, &part, &server_url, &token, &server_scan).await;
    if let Err(e) = result {
        warn!("upload {id} extraction failed: {e:#}");
    }

    // Always clean up the .part and .meta files.
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_file(meta_path(&uploads, &id));
}

async fn run_index_upload(
    id: &str,
    meta: &UploadMeta,
    part: &Path,
    server_url: &str,
    token: &str,
    server_scan: &ServerScanConfig,
) -> Result<()> {
    let hints = meta.scan_hints.as_ref();
    let max_content_size_mb = hints
        .and_then(|h| h.max_content_size_mb)
        .unwrap_or(server_scan.max_content_size_mb);

    // ── 1. Create a unique temp root ─────────────────────────────────────────
    let temp_root = std::env::temp_dir().join(format!("find-upload-{id}"));
    std::fs::create_dir_all(&temp_root)
        .with_context(|| format!("creating temp dir {}", temp_root.display()))?;

    // Ensure cleanup runs even if we bail out early.
    let temp_toml = temp_root.with_extension("toml");
    struct Cleanup(PathBuf, PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
            let _ = std::fs::remove_file(&self.1);
        }
    }
    let _cleanup = Cleanup(temp_root.clone(), temp_toml.clone());

    // ── 2. Place file at <temp_root>/<rel_path> ───────────────────────────────
    let dest = temp_root.join(&meta.rel_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dirs for {}", dest.display()))?;
    }
    std::fs::rename(part, &dest)
        .or_else(|_| std::fs::copy(part, &dest).map(|_| ()))
        .with_context(|| format!("placing upload file at {}", dest.display()))?;

    // ── 3. Write temp client.toml ─────────────────────────────────────────────
    let exclude_toml = toml_string_array(hints.map(|h| h.exclude.as_slice()).unwrap_or(&[]));
    let exclude_extra_toml = toml_string_array(hints.map(|h| h.exclude_extra.as_slice()).unwrap_or(&[]));

    let include_toml = toml_string_array(hints.map(|h| h.include.as_slice()).unwrap_or(&[]));

    let extractors_toml = toml_extractors(&server_scan.extractors);
    let toml = format!(
        "[server]\nurl = \"{server_url}\"\ntoken = \"{token}\"\n\n\
         [[sources]]\nname = \"{}\"\npath = \"{}\"\ninclude = {include_toml}\n\n\
         [scan]\nsubprocess_timeout_secs = {}\nmax_content_size_mb = {}\n\
         exclude = {exclude_toml}\nexclude_extra = {exclude_extra_toml}{extractors_toml}",
        meta.source,
        temp_root.display(),
        server_scan.subprocess_timeout_secs,
        max_content_size_mb,
    );

    std::fs::write(&temp_toml, &toml)
        .with_context(|| format!("writing temp config {}", temp_toml.display()))?;

    // ── 4. Resolve find-scan binary ───────────────────────────────────────────
    let find_scan = resolve_find_scan();

    // ── 5. Spawn find-scan and await ──────────────────────────────────────────
    info!("upload {id}: running find-scan for {}", meta.rel_path);
    let status = tokio::process::Command::new(&find_scan)
        .arg("--config")
        .arg(&temp_toml)
        .arg(&dest)
        .status()
        .await
        .with_context(|| format!("spawning {find_scan}"))?;

    if !status.success() {
        warn!("upload {id}: find-scan exited {:?} for {}", status.code(), meta.rel_path);
    } else {
        info!("upload {id}: find-scan completed for {}", meta.rel_path);
    }

    Ok(())
    // _cleanup drops here, removing temp_root and temp_toml
}

/// Resolve the path to the `find-scan` binary.
///
/// Search order:
/// 1. Same directory as the current executable (production: binaries co-located).
/// 2. Parent of that directory (tests: binary is in `target/debug/deps/`, but
///    `find-scan` is built into `target/debug/`).
/// 3. Fall back to `"find-scan"` and let the OS PATH resolve it.
fn resolve_find_scan() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("find-scan");
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
            if let Some(parent) = dir.parent() {
                let candidate = parent.join("find-scan");
                if candidate.exists() {
                    return candidate.to_string_lossy().into_owned();
                }
            }
        }
    }
    "find-scan".to_string()
}

/// Render a `&[String]` as a TOML inline array: `["a", "b"]`.
fn toml_string_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("\"{s}\"")).collect();
    format!("[{}]", inner.join(", "))
}

/// Serialise `[scan.extractors]` entries for the temp client.toml forwarded to find-scan.
/// Only external extractors are included; `"server_only"` entries are intentionally omitted
/// (they are a client-side routing hint only and would cause infinite upload loops).
fn toml_extractors(extractors: &std::collections::HashMap<String, ExternalExtractorConfig>) -> String {
    if extractors.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n[scan.extractors]\n");
    let mut entries: Vec<_> = extractors.iter().collect();
    entries.sort_by_key(|(k, _)| k.as_str()); // deterministic output
    for (ext, cfg) in entries {
        let mode = match cfg.mode {
            ExternalExtractorMode::Stdout  => "stdout",
            ExternalExtractorMode::TempDir => "tempdir",
        };
        let bin = cfg.bin.replace('\\', "\\\\").replace('"', "\\\"");
        let args = toml_string_array(&cfg.args);
        s.push_str(&format!("{ext} = {{ mode = \"{mode}\", bin = \"{bin}\", args = {args} }}\n"));
    }
    s
}

/// Background task: remove stale upload files (no activity for 2 hours).
///
/// Runs every 10 minutes. Deletes `.part` + `.meta` pairs where the `.meta`
/// file is older than 2 hours, and any orphan `.part` files without a `.meta`.
pub async fn start_cleanup_task(data_dir: PathBuf) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10 * 60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        let uploads = uploads_dir(&data_dir);

        if !uploads.exists() {
            continue;
        }

        let entries = match std::fs::read_dir(&uploads) {
            Ok(e) => e,
            Err(e) => {
                warn!("upload cleanup: failed to read uploads dir: {e}");
                continue;
            }
        };

        let stale_threshold = std::time::Duration::from_secs(2 * 60 * 60);
        let now = std::time::SystemTime::now();

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

            if ext == "meta" {
                let is_stale = path
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|mtime| now.duration_since(mtime).ok())
                    .map(|age| age > stale_threshold)
                    .unwrap_or(false);

                if is_stale {
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    let part = uploads.join(format!("{stem}.part"));
                    info!("upload cleanup: removing stale upload {stem}");
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(&part);
                }
            } else if ext == "part" {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let meta = uploads.join(format!("{stem}.meta"));
                if !meta.exists() {
                    info!("upload cleanup: removing orphan {}", path.display());
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}
