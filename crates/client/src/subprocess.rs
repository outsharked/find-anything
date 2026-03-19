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

use find_common::{
    api::IndexLine,
    config::{ExternalExtractorConfig, ExtractorConfig, ExtractorEntry, ScanConfig},
};
use find_extract_archive::MemberBatch;
use find_extract_dispatch::{dispatch_from_bytes, dispatch_from_path};

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

/// Outcome of an external extractor run.
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub enum ExternalOutcome {
    /// Extraction succeeded; contains extracted lines (stdout mode).
    Ok(Vec<IndexLine>),
    /// Extraction succeeded; contains per-member batches with content hashes (tempdir mode).
    OkMembers(Vec<MemberBatch>),
    /// Extractor ran but failed; file should be indexed filename-only.
    Failed(String),
    /// Extractor binary was not found.
    BinaryMissing,
}

/// Which extractor to use for a given file.
#[derive(Debug)]
#[allow(dead_code)] // intentional: find-client compiles multiple binaries; not all use every export
pub enum ExtractorRoute {
    /// Call the extractor library directly in-process.
    Inline(InlineKind),
    /// Spawn the archive subprocess (streaming MPSC path).
    Archive,
    /// Spawn a non-archive extractor subprocess; contains the resolved binary path.
    Subprocess(String),
    /// Use a user-configured external tool.
    External(ExternalExtractorConfig),
}

/// Identifies which in-process extractor library to call.
#[derive(PartialEq, Debug)]
pub enum InlineKind {
    /// Text/code files — routed through find_extract_dispatch::dispatch_from_path.
    Text,
    Html,
    Media,
    Office,
}

/// Substitute {file}, {name}, {dir} placeholders in extractor args.
/// {dir} is only substituted if `dir` is Some.
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub fn substitute_args(args: &[String], file: &Path, dir: Option<&Path>) -> Vec<String> {
    let file_str = file.to_string_lossy();
    let name_str = file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    args.iter()
        .map(|a| {
            let mut a = a.replace("{file}", &file_str);
            a = a.replace("{name}", &name_str);
            if let Some(d) = dir {
                a = a.replace("{dir}", &d.to_string_lossy());
            }
            a
        })
        .collect()
}

/// Run an external extractor in stdout mode.
/// The tool's stdout is captured and split into IndexLines.
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub async fn run_external_stdout(
    abs_path: &Path,
    ext_cfg: &ExternalExtractorConfig,
    scan: &ScanConfig,
) -> ExternalOutcome {
    if ext_cfg.args.iter().any(|a| a.contains("{dir}")) {
        warn!(
            "{{dir}} placeholder in stdout-mode extractor args for {} — this is a configuration error; placeholder will not be substituted",
            abs_path.display()
        );
    }

    let substituted = substitute_args(&ext_cfg.args, abs_path, None);
    let mut cmd = tokio::process::Command::new(&ext_cfg.bin);
    for arg in &substituted {
        cmd.arg(arg);
    }
    cmd.kill_on_drop(true);

    let timeout = tokio::time::Duration::from_secs(scan.subprocess_timeout_secs);
    let result = tokio::time::timeout(timeout, cmd.output()).await;

    match result {
        Err(_) => {
            warn!(
                "external extractor {} timed out after {}s for {}",
                ext_cfg.bin,
                scan.subprocess_timeout_secs,
                abs_path.display()
            );
            ExternalOutcome::Failed(format!("timed out after {}s", scan.subprocess_timeout_secs))
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            error!(
                "external extractor binary not found: {} — check your [scan.extractors] config",
                ext_cfg.bin
            );
            ExternalOutcome::BinaryMissing
        }
        Ok(Err(e)) => {
            warn!("failed to run external extractor {}: {:#}", ext_cfg.bin, e);
            ExternalOutcome::Failed(e.to_string())
        }
        Ok(Ok(out)) => {
            if !out.status.success() {
                warn!(
                    "external extractor {} exited {:?} for {}",
                    ext_cfg.bin,
                    out.status.code(),
                    abs_path.display()
                );
                return ExternalOutcome::Failed(format!("exited {:?}", out.status.code()));
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<IndexLine> = text
                .lines()
                .enumerate()
                .map(|(i, line)| IndexLine {
                    archive_path: None,
                    line_number: i + 1,
                    content: line.to_string(),
                })
                .collect();
            ExternalOutcome::Ok(lines)
        }
    }
}

/// Run an external extractor in tempdir mode.
/// The tool extracts into a temp directory; we walk and dispatch each extracted file.
/// Returns lines with `archive_path` set to the member's relative path, ready for
/// `build_member_index_files`.
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub async fn run_external_tempdir(
    abs_path: &Path,
    ext_cfg: &ExternalExtractorConfig,
    scan: &ScanConfig,
    ext_config: &ExtractorConfig,
) -> ExternalOutcome {
    let tmp_dir = match tempfile::TempDir::new() {
        Ok(d) => d,
        Err(e) => return ExternalOutcome::Failed(format!("failed to create temp dir: {e}")),
    };

    let substituted = substitute_args(&ext_cfg.args, abs_path, Some(tmp_dir.path()));
    let mut cmd = tokio::process::Command::new(&ext_cfg.bin);
    for arg in &substituted {
        cmd.arg(arg);
    }
    cmd.kill_on_drop(true);

    let timeout = tokio::time::Duration::from_secs(scan.subprocess_timeout_secs);
    let result = tokio::time::timeout(timeout, cmd.output()).await;

    match result {
        Err(_) => {
            warn!(
                "external extractor {} timed out after {}s for {}",
                ext_cfg.bin,
                scan.subprocess_timeout_secs,
                abs_path.display()
            );
            return ExternalOutcome::Failed(format!(
                "timed out after {}s",
                scan.subprocess_timeout_secs
            ));
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            error!(
                "external extractor binary not found: {} — check your [scan.extractors] config",
                ext_cfg.bin
            );
            return ExternalOutcome::BinaryMissing;
        }
        Ok(Err(e)) => {
            warn!("failed to run external extractor {}: {:#}", ext_cfg.bin, e);
            return ExternalOutcome::Failed(e.to_string());
        }
        Ok(Ok(out)) => {
            if !out.status.success() {
                warn!(
                    "external extractor {} exited {:?} for {}",
                    ext_cfg.bin,
                    out.status.code(),
                    abs_path.display()
                );
                return ExternalOutcome::Failed(format!("exited {:?}", out.status.code()));
            }
        }
    }

    // Walk extracted files, dispatch each for content extraction.
    // Each member becomes its own MemberBatch with a content hash computed from
    // the raw extracted bytes — enables deduplication of archive members across
    // different outer archives (e.g. identical MP3s in two different RAR files).
    let mut members: Vec<MemberBatch> = Vec::new();
    for entry in walkdir::WalkDir::new(tmp_dir.path())
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let member_full = entry.path();
        let member_rel = match member_full.strip_prefix(tmp_dir.path()) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let bytes = match std::fs::read(member_full) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "failed to read extracted member {}: {}",
                    member_full.display(),
                    e
                );
                continue;
            }
        };
        let member_name = member_full
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let member_ext = std::path::Path::new(&member_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // If this member is itself a recognized multi-file archive, recurse into
        // it using extract_streaming (the file is already on disk).  This mirrors
        // handle_nested_archive in the native archive path: inner member
        // archive_paths are prefixed with `member_rel::` so the server sees
        // composite paths like `outer.rar::inner.zip::hello.txt`.
        if find_extract_archive::is_archive_ext(&member_ext) && ext_config.max_depth > 0 {
            // Emit a filename-only batch for the archive member itself, exactly as
            // handle_nested_archive does before recursing.  This makes the inner
            // archive navigable as an archive in the tree.
            members.push(MemberBatch {
                lines: vec![IndexLine {
                    archive_path: Some(member_rel.clone()),
                    line_number: 0,
                    content: member_rel.clone(),
                }],
                content_hash: None,
                skip_reason: None,
                mtime: None,
                size: Some(bytes.len() as u64),
            });

            let inner_cfg = ExtractorConfig {
                max_depth: ext_config.max_depth.saturating_sub(1),
                ..ext_config.clone()
            };
            let outer = member_rel.clone();
            let mut ok = true;
            find_extract_archive::extract_streaming(member_full, &inner_cfg, &mut |batch| {
                let prefixed: Vec<IndexLine> = batch.lines.into_iter().map(|mut l| {
                    let inner = l.archive_path.as_deref().unwrap_or("");
                    l.archive_path = Some(if inner.is_empty() {
                        outer.clone()
                    } else {
                        format!("{}::{}", outer, inner)
                    });
                    l
                }).collect();
                members.push(MemberBatch {
                    lines: prefixed,
                    content_hash: batch.content_hash,
                    skip_reason: batch.skip_reason,
                    mtime: batch.mtime,
                    size: batch.size,
                });
            }).unwrap_or_else(|e| {
                warn!("failed to recurse into nested archive {}: {e:#}", member_rel);
                ok = false;
            });
            if ok {
                continue; // inner members emitted; skip dispatch_from_bytes below
            }
            // Fall through to filename-only on extraction failure.
        }

        let content_hash = if bytes.is_empty() {
            None
        } else {
            Some(blake3::hash(&bytes).to_hex().to_string())
        };

        let mut content_lines = dispatch_from_bytes(&bytes, &member_name, ext_config);
        // Set archive_path to member_rel on all returned lines.
        for line in &mut content_lines {
            line.archive_path = Some(member_rel.clone());
        }
        // Add filename marker line — build_member_index_files removes this and
        // replaces it with the composite path (outer::member).
        content_lines.push(IndexLine {
            archive_path: Some(member_rel.clone()),
            line_number: 0,
            content: format!("[PATH] {}", member_rel),
        });
        members.push(MemberBatch { lines: content_lines, content_hash, skip_reason: None, mtime: None, size: Some(bytes.len() as u64) });
    }

    ExternalOutcome::OkMembers(members)
}

/// Resolve the binary path for a named extractor binary.
/// Search order: configured extractor_dir → same dir as current exe → PATH.
fn resolve_binary(name: &str, extractor_dir: &Option<String>) -> String {
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

/// Resolve the path to the find-extract-archive binary.
pub fn resolve_binary_for_archive(extractor_dir: &Option<String>) -> String {
    resolve_binary("find-extract-archive", extractor_dir)
}

/// Resolve the extractor route for a given file path.
///
/// Resolution order:
/// 1. User-configured `scan.extractors` entry → `External` (unless overridden to builtin)
/// 2. Archive extensions → `Archive` (always subprocess regardless of inline_set)
/// 3. PDF → `Subprocess("find-extract-pdf")` (always subprocess)
/// 4. Extension matches an inline-eligible type and kind is in inline_set → `Inline(kind)`
/// 5. Extension matches an inline-eligible type but kind not in inline_set → `Subprocess(binary)`
/// 6. Everything else → `Subprocess("find-extract-dispatch")`
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub fn resolve_extractor(
    path: &Path,
    scan: &ScanConfig,
    extractor_dir: &Option<String>,
    inline_set: &[InlineKind],
) -> ExtractorRoute {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // 1. User-configured extractor override.
    if let Some(entry) = scan.extractors.get(&ext) {
        match entry {
            ExtractorEntry::Builtin(_) => {} // fall through to built-in routing
            ExtractorEntry::External(cfg) => return ExtractorRoute::External(cfg.clone()),
        }
    }

    // 2. Archive — always subprocess (streaming MPSC path is bespoke).
    if find_extract_archive::is_archive_ext(&ext) {
        return ExtractorRoute::Archive;
    }

    // 3. PDF — always subprocess (fork can panic on malformed data).
    if ext == "pdf" {
        return ExtractorRoute::Subprocess(resolve_binary("find-extract-pdf", extractor_dir));
    }

    // 4 & 5. Inline-eligible types — honour inline_set.
    let inline_kind: Option<InlineKind> = match ext.as_str() {
        "html" | "htm" | "xhtml" => Some(InlineKind::Html),
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "ico" | "webp" | "heic"
        | "tiff" | "tif" | "raw" | "cr2" | "nef" | "arw"
        | "mp3" | "flac" | "ogg" | "m4a" | "aac" | "wav" | "wma" | "opus"
        | "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" => Some(InlineKind::Media),
        "docx" | "xlsx" | "xls" | "xlsm" | "pptx" => Some(InlineKind::Office),
        _ => None,
    };

    if let Some(kind) = inline_kind {
        let binary = match &kind {
            InlineKind::Html   => "find-extract-html",
            InlineKind::Media  => "find-extract-media",
            InlineKind::Office => "find-extract-office",
            InlineKind::Text   => "find-extract-dispatch",
        };
        if inline_set.contains(&kind) {
            return ExtractorRoute::Inline(kind);
        } else {
            return ExtractorRoute::Subprocess(resolve_binary(binary, extractor_dir));
        }
    }

    // 5b. Specialist subprocess types — route to their dedicated binary.
    // epub must come before the dispatch fallthrough to preserve its dedicated extractor.
    if ext == "epub" {
        return ExtractorRoute::Subprocess(resolve_binary("find-extract-epub", extractor_dir));
    }

    // 6. Text/code and everything else — dispatch (inline if Text is in inline_set).
    if inline_set.contains(&InlineKind::Text) {
        ExtractorRoute::Inline(InlineKind::Text)
    } else {
        ExtractorRoute::Subprocess(resolve_binary("find-extract-dispatch", extractor_dir))
    }
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
    binary: &str,
) -> SubprocessOutcome {
    let binary = binary.to_string();
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
            relay_subprocess_logs(&out.stderr, &abs_path.to_string_lossy());
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
    binary: &str,
) -> (mpsc::Receiver<MemberBatch>, tokio::task::JoinHandle<bool>) {
    let binary = binary.to_string();
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
            relay_subprocess_logs(&stderr_bytes, &abs_path.to_string_lossy());
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
/// `file` is the path of the file being extracted — included in every log
/// line so errors can be traced back to the source file.
///
/// tracing-subscriber fmt (no time, no ANSI) formats lines as:
///   `{LEVEL} {target}: {message}`
/// We parse the level prefix and re-emit accordingly.
pub fn relay_subprocess_logs(stderr: &[u8], file: &str) {
    let text = String::from_utf8_lossy(stderr);
    for line in text.lines() {
        let Some((tag, msg)) = parse_relay_line(line, find_common::logging::is_ignored) else {
            continue;
        };
        match tag {
            "ERROR" => error!(target: "subprocess", file, "{msg}"),
            "WARN"  => warn!(target: "subprocess", file, "{msg}"),
            "INFO"  => info!(target: "subprocess", file, "{msg}"),
            _       => debug!(target: "subprocess", file, "{msg}"),
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

    #[test]
    fn substitute_args_replaces_all_placeholders() {
        let args = vec!["{file}".to_string(), "{dir}".to_string(), "{name}".to_string()];
        let file = std::path::Path::new("/tmp/my archive.zip");
        let dir = std::path::Path::new("/tmp/out");
        let result = super::substitute_args(&args, file, Some(dir));
        assert_eq!(result[0], "/tmp/my archive.zip");
        assert_eq!(result[1], "/tmp/out");
        assert_eq!(result[2], "my archive.zip");
    }

    #[test]
    fn substitute_args_no_dir_leaves_dir_placeholder() {
        let args = vec!["{file}".to_string(), "{dir}".to_string()];
        let file = std::path::Path::new("/tmp/x.zip");
        let result = super::substitute_args(&args, file, None);
        assert_eq!(result[0], "/tmp/x.zip");
        assert_eq!(result[1], "{dir}"); // not substituted
    }

    #[test]
    fn route_html_with_html_in_inline_set_returns_inline() {
        use find_common::config::ScanConfig;
        let scan = ScanConfig::default();
        let path = std::path::Path::new("page.html");
        let route = super::resolve_extractor(path, &scan, &None, &[super::InlineKind::Html]);
        assert!(matches!(route, super::ExtractorRoute::Inline(super::InlineKind::Html)));
    }

    #[test]
    fn route_html_without_html_in_inline_set_returns_subprocess() {
        use find_common::config::ScanConfig;
        let scan = ScanConfig::default();
        let path = std::path::Path::new("page.html");
        let route = super::resolve_extractor(path, &scan, &None, &[]);
        assert!(matches!(route, super::ExtractorRoute::Subprocess(_)));
    }

    #[test]
    fn route_pdf_always_subprocess() {
        use find_common::config::ScanConfig;
        let scan = ScanConfig::default();
        let path = std::path::Path::new("doc.pdf");
        let all = &[super::InlineKind::Text, super::InlineKind::Html,
                    super::InlineKind::Media, super::InlineKind::Office];
        let route = super::resolve_extractor(path, &scan, &None, all);
        assert!(matches!(route, super::ExtractorRoute::Subprocess(_)));
    }

    #[test]
    fn route_zip_always_archive() {
        use find_common::config::ScanConfig;
        let scan = ScanConfig::default();
        let path = std::path::Path::new("archive.zip");
        let route = super::resolve_extractor(path, &scan, &None, &[]);
        assert!(matches!(route, super::ExtractorRoute::Archive));
    }

    #[test]
    fn route_external_entry_still_returned() {
        use find_common::config::{ExternalExtractorConfig, ExternalExtractorMode, ExtractorEntry, ScanConfig};
        let mut scan = ScanConfig::default();
        scan.extractors.insert(
            "nd1".to_string(),
            ExtractorEntry::External(ExternalExtractorConfig {
                mode: ExternalExtractorMode::TempDir,
                bin: "/usr/bin/extract-nd1".to_string(),
                args: vec!["{file}".to_string(), "{dir}".to_string()],
            }),
        );
        let path = std::path::Path::new("file.nd1");
        let route = super::resolve_extractor(path, &scan, &None, &[]);
        assert!(matches!(route, super::ExtractorRoute::External(_)));
    }

    #[test]
    fn route_media_inline_set_respected() {
        use find_common::config::ScanConfig;
        let scan = ScanConfig::default();
        let path = std::path::Path::new("photo.jpg");
        let route_inline = super::resolve_extractor(path, &scan, &None, &[super::InlineKind::Media]);
        let route_sub    = super::resolve_extractor(path, &scan, &None, &[]);
        assert!(matches!(route_inline, super::ExtractorRoute::Inline(super::InlineKind::Media)));
        assert!(matches!(route_sub, super::ExtractorRoute::Subprocess(_)));
    }

    #[test]
    fn route_unknown_extension_is_dispatch_subprocess() {
        use find_common::config::ScanConfig;
        let scan = ScanConfig::default();
        let path = std::path::Path::new("file.xyz"); // unknown extension, not used elsewhere
        let route = super::resolve_extractor(path, &scan, &None, &[]);
        match &route {
            super::ExtractorRoute::Subprocess(bin) => {
                assert!(bin.contains("find-extract-dispatch"), "unexpected binary: {bin}");
            }
            _ => panic!("expected Subprocess, got different variant"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tempdir_members_indexed() {
        use std::path::PathBuf;
        use find_common::config::{
            extractor_config_from_scan, ExternalExtractorConfig, ExternalExtractorMode, ExtractorEntry,
            ScanConfig,
        };

        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let bin = fixtures_dir.join("find-extract-nd1").to_string_lossy().into_owned();
        let test_file = fixtures_dir.join("test.nd1");

        let mut scan = ScanConfig::default();
        scan.extractors.insert(
            "nd1".to_string(),
            ExtractorEntry::External(ExternalExtractorConfig {
                mode: ExternalExtractorMode::TempDir,
                bin: bin.clone(),
                args: vec!["{file}".to_string(), "{dir}".to_string()],
            }),
        );
        let ext_config = extractor_config_from_scan(&scan);

        let ext_cfg = ExternalExtractorConfig {
            mode: ExternalExtractorMode::TempDir,
            bin,
            args: vec!["{file}".to_string(), "{dir}".to_string()],
        };

        let outcome = super::run_external_tempdir(&test_file, &ext_cfg, &scan, &ext_config).await;

        let batches = match outcome {
            super::ExternalOutcome::OkMembers(b) => b,
            _ => panic!("expected OkMembers"),
        };
        let lines: Vec<_> = batches.iter().flat_map(|b| &b.lines).collect();

        let member_paths: std::collections::HashSet<_> = lines.iter()
            .filter_map(|l| l.archive_path.as_deref())
            .collect();

        // Five flat members + inner.zip and its two nested members should all appear.
        for name in &[
            "readme.txt", "notes.txt", "data.json", "report.md", "empty.txt",
            "inner.zip",
            "inner.zip::hello.txt",
            "inner.zip::subdir/world.txt",
        ] {
            assert!(member_paths.contains(name), "{name} not found; paths: {:?}", member_paths);
        }

        let data_content: String = lines.iter()
            .filter(|l| l.archive_path.as_deref() == Some("data.json"))
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(data_content.contains("hello world"), "missing 'hello world': {data_content}");

        let notes_content: String = lines.iter()
            .filter(|l| l.archive_path.as_deref() == Some("notes.txt"))
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(notes_content.contains("plain text note"), "missing 'plain text note': {notes_content}");
    }

    /// Verify that inner archives inside the tempdir are recursed into.
    /// test.nd1 contains inner.zip (two members); this test checks that those
    /// members appear as composite paths `inner.zip::hello.txt` etc.
    #[cfg(unix)]
    #[tokio::test]
    async fn tempdir_nested_archive_recursed() {
        use std::path::PathBuf;
        use find_common::config::{
            extractor_config_from_scan, ExternalExtractorConfig, ExternalExtractorMode,
            ExtractorEntry, ScanConfig,
        };

        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let ext_cfg = ExternalExtractorConfig {
            mode: ExternalExtractorMode::TempDir,
            bin: fixtures_dir.join("find-extract-nd1").to_string_lossy().into_owned(),
            args: vec!["{file}".to_string(), "{dir}".to_string()],
        };
        // Register the nd1 extractor in scan so that ext_config.external_dispatch
        // is populated — enabling consistent dispatch for inner.nd1 found inside
        // inner.zip, just as it would be at the top level.
        let mut scan = ScanConfig::default();
        scan.extractors.insert("nd1".to_string(), ExtractorEntry::External(ext_cfg.clone()));
        let ext_config = extractor_config_from_scan(&scan);

        let outcome = super::run_external_tempdir(
            &fixtures_dir.join("test.nd1"),
            &ext_cfg,
            &scan,
            &ext_config,
        ).await;

        let batches = match outcome {
            super::ExternalOutcome::OkMembers(b) => b,
            _ => panic!("expected OkMembers"),
        };
        let lines: Vec<_> = batches.iter().flat_map(|b| &b.lines).collect();

        // inner.zip itself should appear as a member.
        let has_inner_zip = lines.iter()
            .any(|l| l.archive_path.as_deref() == Some("inner.zip"));
        assert!(has_inner_zip, "inner.zip not found as a member");

        // Members of inner.zip should appear as composite paths.
        let has_hello = lines.iter()
            .any(|l| l.archive_path.as_deref() == Some("inner.zip::hello.txt"));
        let has_world = lines.iter()
            .any(|l| l.archive_path.as_deref() == Some("inner.zip::subdir/world.txt"));
        assert!(has_hello, "inner.zip::hello.txt not found");
        assert!(has_world, "inner.zip::subdir/world.txt not found");

        // Content of the inner members should be extractable.
        let hello_content: String = lines.iter()
            .filter(|l| l.archive_path.as_deref() == Some("inner.zip::hello.txt"))
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(hello_content.contains("hello from nested zip"), "unexpected content: {hello_content}");

        // Each member batch for inner zip members should have a content_hash set.
        let inner_batches_with_hash: Vec<_> = batches.iter()
            .filter(|b| b.lines.iter().any(|l| {
                l.archive_path.as_deref()
                    .map(|p| p.starts_with("inner.zip::") && p.ends_with("hello.txt"))
                    .unwrap_or(false)
            }))
            .filter(|b| b.content_hash.is_some())
            .collect();
        assert!(!inner_batches_with_hash.is_empty(), "inner.zip::hello.txt batch has no content_hash");

        // inner.nd1 inside inner.zip should be extracted via the external nd1 extractor,
        // yielding its member greet.txt as a composite path.
        let has_nd1_member = lines.iter().any(|l| {
            l.archive_path.as_deref()
                .map(|p| p.starts_with("inner.zip::inner.nd1::") && p.ends_with("greet.txt"))
                .unwrap_or(false)
        });
        assert!(has_nd1_member, "inner.zip::inner.nd1::greet.txt not found; paths: {:?}",
            lines.iter().filter_map(|l| l.archive_path.as_deref()).collect::<Vec<_>>());

        // max_depth = 0 should NOT recurse into inner.zip.
        let no_recurse_cfg = find_common::config::ExtractorConfig {
            max_depth: 0,
            ..ext_config.clone()
        };
        let outcome_shallow = super::run_external_tempdir(
            &fixtures_dir.join("test.nd1"),
            &ext_cfg,
            &scan,
            &no_recurse_cfg,
        ).await;
        let shallow_batches = match outcome_shallow {
            super::ExternalOutcome::OkMembers(b) => b,
            _ => panic!("expected OkMembers"),
        };
        let shallow_lines: Vec<_> = shallow_batches.iter().flat_map(|b| &b.lines).collect();
        let no_nested = shallow_lines.iter()
            .all(|l| !l.archive_path.as_deref().unwrap_or("").contains("inner.zip::"));
        assert!(no_nested, "depth=0 should not recurse into inner.zip");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdout_content_indexed_as_single_document() {
        use std::path::PathBuf;
        use find_common::config::{ExternalExtractorConfig, ExternalExtractorMode, ScanConfig};

        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let bin = fixtures_dir.join("find-extract-nd1-stdout").to_string_lossy().into_owned();
        let test_file = fixtures_dir.join("test.nd1");

        let scan = ScanConfig::default();
        let ext_cfg = ExternalExtractorConfig {
            mode: ExternalExtractorMode::Stdout,
            bin,
            args: vec!["{file}".to_string()],
        };

        let outcome = super::run_external_stdout(&test_file, &ext_cfg, &scan).await;

        let lines = match outcome {
            super::ExternalOutcome::Ok(l) => l,
            _ => panic!("expected Ok"),
        };

        assert!(lines.iter().all(|l| l.archive_path.is_none()), "unexpected archive_path in stdout mode");

        let all_content: String = lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join(" ");
        assert!(all_content.contains("hello world"), "missing 'hello world': {all_content}");
        assert!(all_content.contains("plain text note"), "missing 'plain text note': {all_content}");
        // Comment lines must not appear in output.
        assert!(!all_content.contains("nd1 —"), "comment line leaked into stdout output");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn extractor_nonzero_exit_returns_failed() {
        use std::path::PathBuf;
        use find_common::config::{ExternalExtractorConfig, ExternalExtractorMode, ScanConfig};

        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let test_file = fixtures_dir.join("test.nd1");

        // Write a script that exits 1 into a temp file
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"#!/usr/bin/env bash\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let scan = ScanConfig::default();
        let ext_cfg = ExternalExtractorConfig {
            mode: ExternalExtractorMode::Stdout,
            bin: tmp.path().to_string_lossy().into_owned(),
            args: vec!["{file}".to_string()],
        };

        let outcome = super::run_external_stdout(&test_file, &ext_cfg, &scan).await;

        assert!(
            matches!(outcome, super::ExternalOutcome::Failed(_)),
            "expected Failed"
        );
    }

    #[test]
    fn extract_inline_text_returns_lines() {
        use find_common::config::ScanConfig;
        let cfg = find_common::config::extractor_config_from_scan(&ScanConfig::default());
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let path = manifest.join("src/subprocess.rs"); // large file, always non-empty
        let lines = super::extract_inline(super::InlineKind::Text, &path, &cfg);
        assert!(!lines.is_empty(), "expected text lines from subprocess.rs");
    }

    #[test]
    fn extract_inline_html_returns_lines() {
        use find_common::config::ScanConfig;
        let cfg = find_common::config::extractor_config_from_scan(&ScanConfig::default());
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("tests/fixtures");
        let html_file = std::fs::read_dir(&fixture).ok()
            .and_then(|mut d| d.find(|e| {
                e.as_ref().ok()
                    .and_then(|e| e.path().extension().map(|x| x == "html"))
                    .unwrap_or(false)
            }))
            .and_then(|e| e.ok())
            .map(|e| e.path());
        if let Some(path) = html_file {
            let lines = super::extract_inline(super::InlineKind::Html, &path, &cfg);
            assert!(!lines.is_empty(), "expected html lines");
        }
        // No HTML fixture → pass silently.
    }
}

/// Call an extractor library in-process without spawning a subprocess.
///
/// On error, logs a warning and returns an empty vec (same semantics as a
/// subprocess `Failed` outcome: the file will be indexed by filename only).
///
/// `extract_inline` is synchronous. When called from an async context it
/// will block the Tokio executor thread; this is an accepted trade-off for
/// this change — `spawn_blocking` wrapping is out of scope.
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub fn extract_inline(kind: InlineKind, path: &Path, cfg: &ExtractorConfig) -> Vec<IndexLine> {
    let result = match kind {
        InlineKind::Text => dispatch_from_path(path, cfg),
        InlineKind::Html => find_extract_html::extract(path, cfg),
        InlineKind::Media => find_extract_media::extract(path, cfg),
        InlineKind::Office => find_extract_office::extract(path, cfg),
    };
    match result {
        Ok(lines) => lines,
        Err(e) => {
            warn!("inline extraction failed for {}: {e:#}", path.display());
            vec![]
        }
    }
}

