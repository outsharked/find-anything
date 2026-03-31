/// Client-side chunked file upload with resume support.
///
/// Used as a fallback when the local extractor subprocess fails (e.g. OOM).
/// The file is sent to the server in 4 MB chunks using the upload API.
/// On connection error, the upload resumes from the last acknowledged offset.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::api::ApiClient;
use find_common::api::{UploadInitResponse, UploadScanHints};
use find_common::config::ScanConfig;

/// Build the scan hints to forward when uploading a file to the server.
#[allow(dead_code)] // used by find-scan and find-watch; not all binaries include both
pub fn hints_from_scan(scan: &ScanConfig) -> UploadScanHints {
    UploadScanHints {
        exclude: scan.exclude.clone(),
        exclude_extra: scan.exclude_extra.clone(),
        include: vec![],
        max_content_size_mb: Some(scan.max_content_size_mb),
    }
}

const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MB

/// Upload a file to the server for server-side extraction.
///
/// Sends the file in 4 MB chunks. On connection error, queries the server for
/// the current offset and resumes from there.
pub async fn upload_file(
    api: &ApiClient,
    abs_path: &Path,
    rel_path: &str,
    mtime: i64,
    source_name: &str,
    scan_hints: UploadScanHints,
) -> Result<()> {
    let meta = abs_path.metadata().context("stat file for upload")?;
    let total_size = meta.len();

    info!("server fallback upload: {rel_path} ({total_size} bytes)");

    // Initiate the upload.
    let init_resp: UploadInitResponse = api
        .upload_init(source_name, rel_path, mtime, total_size, scan_hints)
        .await
        .context("initiating upload")?;
    let upload_id = init_resp.upload_id;

    let mut file = std::fs::File::open(abs_path).context("opening file for upload")?;
    let mut offset: u64 = 0;

    while offset < total_size {
        // Seek to current offset.
        file.seek(SeekFrom::Start(offset))
            .context("seeking in upload file")?;

        let chunk_size = (total_size - offset).min(CHUNK_SIZE as u64) as usize;
        let mut buf = vec![0u8; chunk_size];
        file.read_exact(&mut buf).context("reading chunk for upload")?;

        let end = offset + chunk_size as u64 - 1;
        let content_range = format!("bytes {offset}-{end}/{total_size}");

        match api
            .upload_patch(&upload_id, &content_range, buf)
            .await
        {
            Ok(resp) => {
                offset = resp.received;
            }
            Err(e) => {
                warn!("upload chunk error for {rel_path}: {e:#}; checking offset for resume");
                // Query current offset for resume.
                match api.upload_status(&upload_id).await {
                    Ok(status) => {
                        offset = status.received;
                        info!("resuming upload at offset {offset} for {rel_path}");
                    }
                    Err(e2) => {
                        return Err(anyhow::anyhow!(
                            "upload failed and resume query also failed: {e:#}, {e2:#}"
                        ));
                    }
                }
            }
        }
    }

    info!("upload complete for {rel_path}");
    Ok(())
}
