/// Server-side file upload state management and extraction.
///
/// Files are received as chunked uploads (via PATCH requests) and assembled
/// into `.part` files in `data_dir/uploads/`. A companion `.meta` JSON file
/// records source, path, mtime, and total size.
///
/// When all bytes are received, the server runs the appropriate extractor
/// subprocess and writes the resulting BulkRequest to the inbox directory so
/// the normal inbox worker can process it.
///
/// A background cleanup task removes stale uploads (no activity for 2 hours).
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use find_common::api::{BulkRequest, IndexFile, IndexLine};
use find_common::config::ExtractorConfig;
use find_common::subprocess::extract_lines_via_subprocess;

/// Sidecar metadata file for an in-progress upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadMeta {
    pub source: String,
    pub rel_path: String,
    pub mtime: i64,
    pub total_size: u64,
    pub created_at: i64,
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
    // Update mtime by writing the file again (cross-platform).
    if let Ok(content) = std::fs::read(&path) {
        let _ = std::fs::write(&path, content);
    }
}

/// Extract the uploaded file and write results to the inbox.
///
/// Called after the final PATCH chunk is received.
pub async fn index_upload(
    id: String,
    meta: UploadMeta,
    data_dir: PathBuf,
    extractor_dir: Option<String>,
    ext_cfg: ExtractorConfig,
) {
    let uploads = uploads_dir(&data_dir);
    let file_path = part_path(&uploads, &id);
    let inbox_dir = data_dir.join("inbox");

    info!(
        "server-side extraction: {} ({} → {})",
        id, file_path.display(), meta.rel_path
    );

    // Run the appropriate extractor subprocess.
    let mut lines =
        extract_lines_via_subprocess(&file_path, &ext_cfg, &extractor_dir).await;

    // Always include the filename at line 0 so the file is discoverable by name.
    if lines.iter().all(|l| l.line_number != 0) {
        lines.insert(
            0,
            IndexLine {
                archive_path: None,
                line_number: 0,
                content: meta.rel_path.clone(),
            },
        );
    }

    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let file_size = file_path.metadata().ok().map(|m| m.len() as i64);
    let kind = find_common::api::detect_kind_from_ext(
        std::path::Path::new(&meta.rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or(""),
    )
    .to_string();

    let req = BulkRequest {
        source: meta.source.clone(),
        files: vec![IndexFile {
            path: meta.rel_path.clone(),
            mtime: meta.mtime,
            size: file_size,
            kind,
            lines,
            extract_ms: None,
            content_hash: None,
            scanner_version: find_common::api::SCANNER_VERSION,
        }],
        delete_paths: vec![],
        scan_timestamp: Some(now),
        indexing_failures: vec![],
        rename_paths: vec![],
    };

    // Gzip-compress and write to inbox so the normal worker processes it.
    if let Err(e) = write_to_inbox(&req, &inbox_dir) {
        warn!("failed to write upload {id} to inbox: {e:#}");
    } else {
        info!("upload {id} written to inbox for {}", meta.rel_path);
    }

    // Clean up upload files.
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_file(meta_path(&uploads, &id));
}

/// Write a `BulkRequest` as a gzip-compressed JSON file in `inbox_dir`.
fn write_to_inbox(req: &BulkRequest, inbox_dir: &Path) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let path = inbox_dir.join(format!("{id}.gz"));

    let file = std::fs::File::create(&path).context("creating inbox file")?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    serde_json::to_writer(&mut encoder, req).context("serializing BulkRequest")?;
    encoder.finish().context("finishing gzip")?;
    Ok(())
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
                // Check if the meta file is stale.
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
                // Orphan .part without a .meta — remove it.
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
