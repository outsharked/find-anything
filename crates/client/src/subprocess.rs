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
use find_extract_dispatch::dispatch_from_bytes;

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
    /// Extraction succeeded; contains extracted lines.
    Ok(Vec<IndexLine>),
    /// Extractor ran but failed; file should be indexed filename-only.
    Failed(String),
    /// Extractor binary was not found.
    BinaryMissing,
}

/// Result of `resolve_extractor`.
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub enum ExtractorChoice {
    /// Use built-in routing (existing code paths).
    Builtin,
    /// Use a user-configured external tool.
    External(ExternalExtractorConfig),
}

/// Resolve the extractor to use for a given file path.
/// Checks [scan.extractors] first; falls back to built-in routing.
#[allow(dead_code)] // used by find-scan; other binaries share this module
pub fn resolve_extractor(path: &Path, scan: &ScanConfig) -> ExtractorChoice {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if let Some(entry) = scan.extractors.get(&ext) {
        match entry {
            ExtractorEntry::Builtin(_) => {}
            ExtractorEntry::External(cfg) => return ExtractorChoice::External(cfg.clone()),
        }
    }
    ExtractorChoice::Builtin
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
    let mut all_lines: Vec<IndexLine> = Vec::new();
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
            content: member_rel,
        });
        all_lines.extend(content_lines);
    }

    ExternalOutcome::Ok(all_lines)
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
    fn resolve_extractor_builtin_sentinel_falls_through() {
        use find_common::config::{ExtractorEntry, ScanConfig};
        let mut scan = ScanConfig::default();
        scan.extractors.insert("zip".to_string(), ExtractorEntry::Builtin("builtin".to_string()));
        let path = std::path::Path::new("archive.zip");
        assert!(matches!(super::resolve_extractor(path, &scan), super::ExtractorChoice::Builtin));
    }

    #[test]
    fn resolve_extractor_unknown_extension_is_builtin() {
        use find_common::config::ScanConfig;
        let scan = ScanConfig::default();
        let path = std::path::Path::new("file.nd1");
        assert!(matches!(super::resolve_extractor(path, &scan), super::ExtractorChoice::Builtin));
    }

    #[test]
    fn resolve_extractor_external_entry_returned() {
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
        let path = std::path::Path::new("archive.nd1");
        assert!(matches!(
            super::resolve_extractor(path, &scan),
            super::ExtractorChoice::External(_)
        ));
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

        let lines = match outcome {
            super::ExternalOutcome::Ok(l) => l,
            _ => panic!("expected Ok"),
        };

        let member_paths: std::collections::HashSet<_> = lines.iter()
            .filter_map(|l| l.archive_path.as_deref())
            .collect();

        // All five members should be present (comment lines are not members).
        for name in &["readme.txt", "notes.txt", "data.json", "report.md", "empty.txt"] {
            assert!(member_paths.contains(name), "{name} not found; paths: {:?}", member_paths);
        }
        assert_eq!(member_paths.len(), 5, "unexpected extra members: {:?}", member_paths);

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
