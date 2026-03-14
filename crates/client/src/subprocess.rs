use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

fn missing_binaries_warned() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

use find_common::{api::IndexLine, config::ScanConfig};
use find_extract_archive::MemberBatch;

/// Outcome of a subprocess extraction attempt.
pub enum SubprocessOutcome {
    /// Extraction succeeded; contains the extracted lines.
    Ok(Vec<IndexLine>),
    /// Subprocess ran but failed; file should be indexed filename-only.
    Failed,
    /// Extractor binary was not found; file should not be indexed at all so it
    /// is retried once the binary is correctly deployed.
    BinaryMissing,
}

/// Extract content from any file via the appropriate subprocess.
///
/// For archive files, parses `Vec<MemberBatch>` from the binary and flattens to
/// `Vec<IndexLine>` so the caller receives a flat list identical to the pre-subprocess
/// result.  For all other formats, parses `Vec<IndexLine>` directly.
///
/// Returns `SubprocessOutcome::Failed` on subprocess failure or parse error
/// (the error is logged as a warning so the scan can continue with other files).
pub async fn extract_via_subprocess(
    abs_path: &Path,
    scan: &ScanConfig,
    extractor_dir: &Option<String>,
) -> SubprocessOutcome {
    let binary = extractor_binary_for(abs_path, extractor_dir);
    let max_content_kb = (scan.max_content_size_mb * 1024).to_string();
    let max_depth = scan.archives.max_depth.to_string();
    let max_line_length = scan.max_line_length.to_string();

    let ext = abs_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let is_archive = find_extract_archive::is_archive_ext(&ext);
    let is_pdf = ext == "pdf";

    let mut cmd = tokio::process::Command::new(&binary);
    cmd.arg(abs_path).arg(&max_content_kb);
    if is_archive {
        // find-extract-archive: <path> [max-content-kb] [max-depth] [max-line-length]
        cmd.arg(&max_depth).arg(&max_line_length);
    } else if is_pdf {
        // find-extract-pdf: <path> [max-content-kb] [max-line-length]
        cmd.arg(&max_line_length);
    }
    // Kill the child process if it is still running when the future is dropped
    // (i.e. when the timeout fires and the output future is cancelled).
    cmd.kill_on_drop(true);

    let timeout = tokio::time::Duration::from_secs(scan.subprocess_timeout_secs);
    let result = tokio::time::timeout(timeout, cmd.output()).await;

    match result {
        Err(_elapsed) => {
            warn!(
                "extractor {} timed out after {}s for {}",
                binary,
                scan.subprocess_timeout_secs,
                abs_path.display()
            );
            SubprocessOutcome::Failed
        }
        Ok(Ok(out)) => {
            relay_subprocess_logs(&out.stderr);
            if out.status.success() {
                let lines = if is_archive {
                    let batches: Vec<MemberBatch> =
                        serde_json::from_slice(&out.stdout).unwrap_or_default();
                    batches.into_iter().flat_map(|b| b.lines).collect()
                } else {
                    serde_json::from_slice::<Vec<IndexLine>>(&out.stdout).unwrap_or_default()
                };
                SubprocessOutcome::Ok(lines)
            } else {
                warn!(
                    "extractor {} exited {:?} for {}",
                    binary,
                    out.status.code(),
                    abs_path.display()
                );
                SubprocessOutcome::Failed
            }
        }
        Ok(Err(e)) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                // The binary itself is missing — this is a deployment error, not
                // a per-file extraction failure.  Log as ERROR once per binary so
                // it's clearly visible without flooding the log.
                let already_reported = missing_binaries_warned()
                    .lock()
                    .map(|mut set| !set.insert(binary.clone()))
                    .unwrap_or(false);
                if !already_reported {
                    error!(
                        "extractor binary not found: {binary} — check your installation \
                         (this error will be suppressed for subsequent files)"
                    );
                }
                SubprocessOutcome::BinaryMissing
            } else {
                warn!("failed to run extractor {binary}: {e:#}");
                SubprocessOutcome::Failed
            }
        }
    }
}

#[allow(dead_code)] // used by find-scan; other binaries share this module
/// Start archive extraction in a subprocess and stream `MemberBatch` items
/// over a bounded channel as they are extracted.
///
/// The subprocess emits NDJSON (one `MemberBatch` JSON object per line) so
/// neither the subprocess nor the parent ever holds all extracted content in
/// memory at once.  The channel has capacity 8, which applies backpressure
/// through the pipe if the caller processes batches more slowly than the
/// subprocess produces them.
///
/// Returns `(receiver, join_handle)`.  The `JoinHandle` resolves to `true`
/// if the subprocess exited successfully, `false` otherwise.  Await it after
/// draining the receiver to check for extraction failure.
pub fn start_archive_subprocess(
    abs_path: PathBuf,
    scan: &ScanConfig,
    extractor_dir: &Option<String>,
) -> (mpsc::Receiver<MemberBatch>, tokio::task::JoinHandle<bool>) {
    let binary = extractor_binary_for(&abs_path, extractor_dir);
    let max_content_kb = (scan.max_content_size_mb * 1024).to_string();
    let max_depth = scan.archives.max_depth.to_string();
    let max_line_length = scan.max_line_length.to_string();

    let exclude_patterns_json = if scan.exclude.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&scan.exclude).unwrap_or_default()
    };

    let (tx, rx) = mpsc::channel(8);

    let handle = tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new(&binary);
        cmd.arg(&abs_path)
            .arg(&max_content_kb)
            .arg(&max_depth)
            .arg(&max_line_length);
        if !exclude_patterns_json.is_empty() {
            cmd.arg(&exclude_patterns_json);
        }
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!("failed to spawn {binary}: {e:#}");
                return false;
            }
        };

        // Take stderr before spawning the drain task so child remains available.
        let stderr = child.stderr.take();
        let stderr_handle = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut s) = stderr {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut buf).await;
            }
            buf
        });

        // Read stdout line by line and forward each parsed MemberBatch.
        let mut success = true;
        if let Some(stdout) = child.stdout.take() {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        match serde_json::from_str::<MemberBatch>(&line) {
                            Ok(batch) => {
                                if tx.send(batch).await.is_err() {
                                    // Receiver was dropped (caller aborted); kill subprocess.
                                    child.kill().await.ok();
                                    break;
                                }
                            }
                            Err(e) => warn!("archive subprocess parse error: {e:#}"),
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!("archive subprocess stdout read error: {e:#}");
                        break;
                    }
                }
            }
        }

        match child.wait().await {
            Ok(status) => {
                if !status.success() {
                    warn!(
                        "extractor {} exited {:?} for {}",
                        binary,
                        status.code(),
                        abs_path.display()
                    );
                    success = false;
                }
            }
            Err(e) => {
                warn!("archive subprocess wait error: {e:#}");
                success = false;
            }
        }

        if let Ok(stderr_bytes) = stderr_handle.await {
            relay_subprocess_logs(&stderr_bytes);
        }

        success
    });

    (rx, handle)
}

/// Parses a single tracing-subscriber fmt stderr line and decides whether to
/// relay it.  Returns `Some((level_tag, message))` if the line should be
/// emitted, or `None` if it is empty or suppressed by an ignore pattern.
///
/// Separated from the tracing macro calls so it can be unit-tested without a
/// live subscriber.
pub(crate) fn parse_relay_line(
    line: &str,
    is_ignored: impl Fn(&str) -> bool,
) -> Option<(&'static str, &str)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // Strip any leading non-alphanumeric chars (e.g. ANSI escape remnants).
    let rest = line.trim_start_matches(|c: char| !c.is_alphanumeric());
    for (prefix, tag) in [
        ("ERROR ", "ERROR"),
        ("WARN ",  "WARN"),
        ("INFO ",  "INFO"),
        ("DEBUG ", "DEBUG"),
        ("TRACE ", "TRACE"),
    ] {
        if let Some(msg) = rest.strip_prefix(prefix) {
            return if is_ignored(msg) { None } else { Some((tag, msg)) };
        }
    }
    // Unknown format — emit as WARN unless ignored.
    if is_ignored(line) { None } else { Some(("WARN", line)) }
}

/// Re-emit subprocess stderr lines through our tracing subscriber so they
/// appear in the parent process output at the correct level and pass through
/// the same log-ignore filters as in-process events.
///
/// tracing-subscriber fmt (no time, no ANSI) formats lines as:
///   `{LEVEL} {target}: {message}`
/// We parse the level prefix and re-emit accordingly.
pub fn relay_subprocess_logs(stderr: &[u8]) {
    let text = String::from_utf8_lossy(stderr);
    for line in text.lines() {
        let Some((tag, msg)) = parse_relay_line(line, find_common::logging::is_ignored) else {
            continue;
        };
        match tag {
            "ERROR" => error!(target: "subprocess", "{msg}"),
            "WARN"  => warn!(target: "subprocess", "{msg}"),
            "INFO"  => info!(target: "subprocess", "{msg}"),
            _       => debug!(target: "subprocess", "{msg}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_relay_line;

    fn ignore_pdf(msg: &str) -> bool {
        msg.contains("pdf_extract: unknown glyph")
    }

    fn no_ignore(_: &str) -> bool { false }

    #[test]
    fn suppresses_warn_matching_ignore_pattern() {
        // Exact regression from a025efb/c029136: this line must be suppressed
        let line = "WARN pdf_extract: unknown glyph name 'box3' for font ArialMT";
        assert!(parse_relay_line(line, ignore_pdf).is_none());
    }

    #[test]
    fn passes_warn_not_matching_pattern() {
        let line = "WARN find_server::worker: slow step took 1200ms";
        assert!(parse_relay_line(line, ignore_pdf).is_some());
    }

    #[test]
    fn parses_error_prefix() {
        let (tag, msg) = parse_relay_line("ERROR some::crate: bad thing", no_ignore).unwrap();
        assert_eq!(tag, "ERROR");
        assert_eq!(msg, "some::crate: bad thing");
    }

    #[test]
    fn parses_warn_prefix() {
        let (tag, msg) = parse_relay_line("WARN target: message", no_ignore).unwrap();
        assert_eq!(tag, "WARN");
        assert_eq!(msg, "target: message");
    }

    #[test]
    fn parses_info_prefix() {
        let (tag, _) = parse_relay_line("INFO some::crate: hello", no_ignore).unwrap();
        assert_eq!(tag, "INFO");
    }

    #[test]
    fn empty_line_returns_none() {
        assert!(parse_relay_line("   ", no_ignore).is_none());
    }

    #[test]
    fn unknown_format_emitted_as_warn() {
        let (tag, msg) = parse_relay_line("bare message with no level", no_ignore).unwrap();
        assert_eq!(tag, "WARN");
        assert_eq!(msg, "bare message with no level");
    }

    #[test]
    fn unknown_format_suppressed_if_ignored() {
        let suppress_bare = |m: &str| m.contains("bare message");
        assert!(parse_relay_line("bare message with no level", suppress_bare).is_none());
    }
}

pub fn extractor_binary_for(path: &Path, extractor_dir: &Option<String>) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let name = match ext.as_str() {
        "zip" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "tbz2" | "txz" | "7z" => {
            "find-extract-archive"
        }
        "pdf" => "find-extract-pdf",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "ico" | "webp" | "heic"
        | "tiff" | "tif" | "raw" | "cr2" | "nef" | "arw"
        | "mp3" | "flac" | "ogg" | "m4a" | "aac" | "wav" | "wma" | "opus"
        | "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" => {
            "find-extract-media"
        }
        "html" | "htm" | "xhtml" => "find-extract-html",
        "docx" | "xlsx" | "xls" | "xlsm" | "pptx" => "find-extract-office",
        "epub" => "find-extract-epub",
        _ => "find-extract-dispatch",
    };

    // Resolution order:
    // 1. configured extractor_dir / name
    // 2. same dir as current executable / name
    // 3. name (rely on PATH)
    if let Some(dir) = extractor_dir {
        return format!("{}/{}", dir, name);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }

    name.to_string()
}
