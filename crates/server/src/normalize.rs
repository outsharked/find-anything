/// Server-side text normalization applied before content is written to ZIP archives.
///
/// Per-file normalization chain (first success wins for pretty-printing):
/// 1. Built-in pretty-printers (JSON, TOML)
/// 2. External stdin-mode formatter subprocess (if configured and exits 0)
/// 3. Word-wrap at max_line_length (always applied as the final step)
///
/// Batch normalization (`normalize_batch_indexed`) additionally runs
/// `batch`-mode formatters once per batch rather than once per file.
use find_common::api::{IndexLine, LINE_CONTENT_START};
use find_common::config::{FormatterConfig, FormatterMode, NormalizationSettings};

/// Normalize `lines` for the file named `name`.
///
/// Returns the lines unchanged if `cfg.max_line_length == 0`.
/// Line numbers are reassigned sequentially (1-based) after any content
/// transformation. The line-0 path entry is preserved and passed through
/// unchanged.
pub fn normalize_lines(
    lines: Vec<IndexLine>,
    name: &str,
    cfg: &NormalizationSettings,
) -> Vec<IndexLine> {
    if cfg.max_line_length == 0 {
        return lines;
    }

    // Separate path/metadata lines (< LINE_CONTENT_START) from content lines (>= LINE_CONTENT_START).
    let (zero_lines, mut content_lines): (Vec<IndexLine>, Vec<IndexLine>) =
        lines.into_iter().partition(|l| l.line_number < LINE_CONTENT_START);

    if content_lines.is_empty() {
        return zero_lines;
    }

    // Sort content lines by line_number before joining.
    content_lines.sort_by_key(|l| l.line_number);

    let ext = extension_of(name);

    let full_text: String = content_lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");

    // Step 1: built-in pretty-printers.
    let pretty = try_builtin_pretty(&full_text, &ext, name);

    // Step 2: external formatters (only if step 1 didn't apply).
    let formatted_text = if let Some(t) = pretty {
        Some(t)
    } else {
        try_external_formatters(&full_text, name, &ext, cfg)
    };

    // Use reformatted text if available, otherwise keep original.
    let working_text = formatted_text.unwrap_or(full_text);

    // Step 3: word-wrap every line that exceeds max_line_length.
    let wrapped_lines = apply_word_wrap(&working_text, cfg.max_line_length);

    // Rebuild IndexLine vec with fresh line numbers starting at LINE_CONTENT_START.
    let mut result = zero_lines;
    for (i, content) in wrapped_lines.into_iter().enumerate() {
        result.push(IndexLine {
            archive_path: None,
            line_number: i + LINE_CONTENT_START,
            content,
        });
    }
    result
}

/// Normalize all text-like files in a batch, calling each `batch`-mode
/// formatter once per batch rather than once per file.
///
/// Each element of `files` is `(original_index, path, lines)`. Lines are
/// updated in-place. `original_index` is preserved but not used internally —
/// the caller uses it to map results back to the full `files` vec.
///
/// Processing order:
/// 1. Each `Batch`-mode formatter runs once on all matching files.
/// 2. `Stdin`-mode formatters run per-file (via `normalize_lines`) on any
///    files not yet handled by a batch formatter.
/// 3. Word-wrap is applied to batch-handled files as the final step.
///    (`normalize_lines` already applies word-wrap for the per-file path.)
pub fn normalize_batch_indexed(
    files: &mut [(usize, String, Vec<IndexLine>)],
    cfg: &NormalizationSettings,
) {
    if cfg.max_line_length == 0 || files.is_empty() {
        return;
    }

    let mut handled = vec![false; files.len()];

    let batch_timeout = std::time::Duration::from_secs(cfg.batch_formatter_timeout_secs);
    let per_file_timeout = std::time::Duration::from_secs(cfg.per_file_formatter_timeout_secs);

    for fmt in &cfg.formatters {
        if fmt.mode != FormatterMode::Batch {
            continue;
        }
        apply_batch_formatter(files, &mut handled, fmt, batch_timeout, per_file_timeout);
    }

    // Per-file path for unhandled files (stdin formatters + built-in pretty-printers + word-wrap).
    for (i, (_, name, lines)) in files.iter_mut().enumerate() {
        if !handled[i] {
            *lines = normalize_lines(std::mem::take(lines), name, cfg);
        }
    }

    // Word-wrap for batch-handled files (formatter may have produced long lines).
    for (i, (_, _, lines)) in files.iter_mut().enumerate() {
        if handled[i] {
            *lines = word_wrap_lines(std::mem::take(lines), cfg.max_line_length);
        }
    }
}

// ── Built-in pretty-printers ─────────────────────────────────────────────────

fn try_builtin_pretty(text: &str, ext: &str, name: &str) -> Option<String> {
    match ext {
        "json" | "jsonc" => {
            let v: serde_json::Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(file = %name, error = %e, "normalize: built-in JSON parse failed, falling through");
                    return None;
                }
            };
            let result = serde_json::to_string_pretty(&v).ok();
            if result.is_some() {
                tracing::debug!(file = %name, "normalize: built-in JSON pretty-print succeeded");
            }
            result
        }
        "toml" => {
            let v: toml::Value = match toml::from_str(text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(file = %name, error = %e, "normalize: built-in TOML parse failed, falling through");
                    return None;
                }
            };
            let result = toml::to_string_pretty(&v).ok();
            if result.is_some() {
                tracing::debug!(file = %name, "normalize: built-in TOML pretty-print succeeded");
            }
            result
        }
        _ => None,
    }
}

// ── External formatters ───────────────────────────────────────────────────────

fn try_external_formatters(
    text: &str,
    name: &str,
    ext: &str,
    cfg: &NormalizationSettings,
) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    for fmt in &cfg.formatters {
        if fmt.mode != FormatterMode::Stdin {
            continue; // batch formatters are handled by normalize_batch_indexed
        }
        if !fmt.extensions.iter().any(|e| e == ext) {
            continue;
        }

        let args: Vec<String> = fmt.args.iter()
            .map(|a| a.replace("{name}", name))
            .collect();

        tracing::debug!(
            formatter = %fmt.path,
            file = %name,
            "normalize: trying external formatter"
        );

        let mut child = match Command::new(&fmt.path)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    formatter = %fmt.path,
                    file = %name,
                    error = %e,
                    "normalize: failed to spawn formatter"
                );
                continue;
            }
        };

        // Write input and wait with a 5-second timeout via a thread.
        let text_bytes = text.as_bytes().to_vec();
        let stdin_result = child.stdin.take().map(|mut stdin| {
            stdin.write_all(&text_bytes)
        });

        if let Some(Err(e)) = stdin_result {
            tracing::warn!(
                formatter = %fmt.path,
                file = %name,
                error = %e,
                "normalize: failed to write to formatter stdin"
            );
            let _ = child.kill();
            continue;
        }

        let output = match wait_with_timeout(child, std::time::Duration::from_secs(5)) {
            Some(o) => o,
            None => {
                tracing::warn!(
                    formatter = %fmt.path,
                    file = %name,
                    "normalize: formatter timed out after 5s"
                );
                continue;
            }
        };

        if output.status.success() {
            let formatted = String::from_utf8_lossy(&output.stdout).into_owned();
            if !formatted.trim().is_empty() {
                tracing::debug!(
                    formatter = %fmt.path,
                    file = %name,
                    "normalize: formatter succeeded"
                );
                return Some(formatted);
            }
            tracing::warn!(
                formatter = %fmt.path,
                file = %name,
                "normalize: formatter exited 0 but produced empty output"
            );
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                formatter = %fmt.path,
                file = %name,
                exit_code = ?output.status.code(),
                "normalize: formatter exited with error"
            );
            tracing::debug!(
                formatter = %fmt.path,
                file = %name,
                stderr = %stderr.trim(),
                "normalize: formatter stderr"
            );
        }
    }
    None
}

/// Run `child.wait_with_output()` on a background thread with a timeout.
/// Returns `None` if the timeout expires (child is killed).
fn wait_with_timeout(
    child: std::process::Child,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    rx.recv_timeout(timeout).ok()?.ok()
}

/// Run `child.wait()` on a background thread with a timeout.
/// Returns `None` if the timeout expires; the child process is left to run
/// to completion on the background thread.
fn wait_status_with_timeout(
    mut child: std::process::Child,
    timeout: std::time::Duration,
) -> Option<std::process::ExitStatus> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    rx.recv_timeout(timeout).ok()?.ok()
}

// ── Batch formatter ───────────────────────────────────────────────────────────

/// Join the content lines of a file entry into a single string for formatting.
fn content_text(lines: &[IndexLine]) -> String {
    let mut sorted: Vec<&IndexLine> = lines.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
    sorted.sort_by_key(|l| l.line_number);
    sorted.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n")
}

/// Apply a formatted text result back to a file entry, rebuilding its lines
/// and marking it as handled. No-ops if `formatted_text` is blank.
fn apply_formatted_text(
    batch_idx: usize,
    formatted_text: &str,
    files: &mut [(usize, String, Vec<IndexLine>)],
    handled: &mut [bool],
    fmt: &FormatterConfig,
) {
    if formatted_text.trim().is_empty() {
        return;
    }
    let (_, file_name, lines) = &mut files[batch_idx];
    let mut result: Vec<IndexLine> = lines.iter().filter(|l| l.line_number < LINE_CONTENT_START).cloned().collect();
    for (j, content) in formatted_text.lines().enumerate() {
        result.push(IndexLine { archive_path: None, line_number: j + LINE_CONTENT_START, content: content.to_string() });
    }
    *lines = result;
    handled[batch_idx] = true;
    tracing::debug!(formatter = %fmt.path, file = %file_name, "normalize: batch formatter succeeded");
}

/// Run a single `Batch`-mode formatter on all matching, unhandled files in
/// the batch. Updates `lines` in-place and marks handled entries in `handled`.
///
/// If the batch times out, falls back to per-file mode so that only the
/// problematic file is skipped rather than the entire batch.
fn apply_batch_formatter(
    files: &mut [(usize, String, Vec<IndexLine>)],
    handled: &mut [bool],
    fmt: &FormatterConfig,
    batch_timeout: std::time::Duration,
    per_file_timeout: std::time::Duration,
) {
    // Collect (batch_index, extension) for matching unhandled files.
    let matching: Vec<(usize, String)> = files.iter().enumerate()
        .filter(|(i, (_, name, _))| {
            !handled[*i] && {
                let ext = extension_of(name);
                fmt.extensions.iter().any(|e| e == &ext)
            }
        })
        .map(|(i, (_, name, _))| (i, extension_of(name)))
        .collect();

    if matching.is_empty() {
        return;
    }

    let tmp = match tempfile::TempDir::new() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(formatter = %fmt.path, error = %e, "normalize: failed to create batch tempdir");
            return;
        }
    };

    // Write each matching file as {seq:05}.{ext} in the temp dir.
    let mut temp_entries: Vec<(usize, std::path::PathBuf)> = Vec::new();
    for (seq, (batch_idx, ext)) in matching.iter().enumerate() {
        let temp_path = tmp.path().join(format!("{seq:05}.{ext}"));
        let text = content_text(&files[*batch_idx].2);
        if let Err(e) = std::fs::write(&temp_path, &text) {
            tracing::warn!(formatter = %fmt.path, error = %e, "normalize: failed to write temp file {seq:05}.{ext}");
            continue;
        }
        temp_entries.push((*batch_idx, temp_path));
    }

    if temp_entries.is_empty() {
        return;
    }

    let dir_str = tmp.path().to_string_lossy();
    let args: Vec<String> = fmt.args.iter().map(|a| a.replace("{dir}", &dir_str)).collect();

    tracing::debug!(
        formatter = %fmt.path,
        files = temp_entries.len(),
        "normalize: running batch formatter"
    );

    let child = match std::process::Command::new(&fmt.path).args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Err(e) => {
            tracing::warn!(formatter = %fmt.path, error = %e, "normalize: failed to spawn batch formatter");
            return;
        }
        Ok(c) => c,
    };

    match wait_status_with_timeout(child, batch_timeout) {
        None => {
            tracing::warn!(
                formatter = %fmt.path,
                files = temp_entries.len(),
                timeout_secs = batch_timeout.as_secs(),
                "normalize: batch formatter timed out, retrying per-file"
            );
            apply_batch_formatter_per_file(&temp_entries, files, handled, fmt, per_file_timeout);
            return;
        }
        Some(s) if !s.success() => {
            // Non-zero exit is expected when the formatter encounters files it
            // cannot parse (e.g. malformed HTML, syntax errors). The formatter
            // typically still processes all other files successfully, so we
            // fall through and use whatever it managed to write.
            tracing::debug!(
                formatter = %fmt.path,
                exit_code = ?s.code(),
                "normalize: batch formatter exited with errors — reading per-file results"
            );
        }
        Some(_) => {}
    }

    // Read back each file and use whatever is there. If the formatter failed
    // on a file it leaves the original content on disk unchanged — reading
    // that back is identical to using the original, so no comparison needed.
    // The only guard is against an empty file (formatter wiped it).
    for (batch_idx, temp_path) in &temp_entries {
        match std::fs::read_to_string(temp_path) {
            Ok(text) => apply_formatted_text(*batch_idx, &text, files, handled, fmt),
            Err(e) => tracing::warn!(
                formatter = %fmt.path,
                file = %files[*batch_idx].1,
                error = %e,
                "normalize: failed to read back formatted temp file"
            ),
        }
    }
}

/// Per-file fallback used when the batch formatter times out.
///
/// Each file gets its own temp dir and a 10-second individual timeout. Files
/// that time out individually are skipped (kept with their original content);
/// all others are formatted normally.
fn apply_batch_formatter_per_file(
    temp_entries: &[(usize, std::path::PathBuf)],
    files: &mut [(usize, String, Vec<IndexLine>)],
    handled: &mut [bool],
    fmt: &FormatterConfig,
    per_file_timeout: std::time::Duration,
) {
    for (batch_idx, _) in temp_entries {
        let batch_idx = *batch_idx;
        let ext = extension_of(&files[batch_idx].1);
        let text = content_text(&files[batch_idx].2);

        let per_tmp = match tempfile::TempDir::new() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(formatter = %fmt.path, error = %e, "normalize: failed to create per-file tempdir");
                continue;
            }
        };

        let per_path = per_tmp.path().join(format!("file.{ext}"));
        if let Err(e) = std::fs::write(&per_path, &text) {
            tracing::warn!(formatter = %fmt.path, file = %files[batch_idx].1, error = %e, "normalize: failed to write per-file temp");
            continue;
        }

        let dir_str = per_tmp.path().to_string_lossy().into_owned();
        let args: Vec<String> = fmt.args.iter().map(|a| a.replace("{dir}", &dir_str)).collect();

        let child = match std::process::Command::new(&fmt.path)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Err(e) => {
                tracing::warn!(formatter = %fmt.path, file = %files[batch_idx].1, error = %e, "normalize: failed to spawn per-file formatter");
                continue;
            }
            Ok(c) => c,
        };

        if wait_status_with_timeout(child, per_file_timeout).is_none() {
            tracing::warn!(
                formatter = %fmt.path,
                file = %files[batch_idx].1,
                timeout_secs = per_file_timeout.as_secs(),
                "normalize: per-file formatter timed out, skipping"
            );
            continue;
        }

        if let Ok(formatted_text) = std::fs::read_to_string(&per_path) {
            apply_formatted_text(batch_idx, &formatted_text, files, handled, fmt);
        }
    }
}

/// Apply word-wrap to the content lines in `lines`, preserving path/metadata entries.
fn word_wrap_lines(lines: Vec<IndexLine>, max_line_length: usize) -> Vec<IndexLine> {
    let (non_content_lines, mut content_lines): (Vec<IndexLine>, Vec<IndexLine>) =
        lines.into_iter().partition(|l| l.line_number < LINE_CONTENT_START);

    if content_lines.is_empty() {
        return non_content_lines;
    }

    content_lines.sort_by_key(|l| l.line_number);
    let full_text = content_lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
    let wrapped = apply_word_wrap(&full_text, max_line_length);

    let mut result = non_content_lines;
    for (i, content) in wrapped.into_iter().enumerate() {
        result.push(IndexLine { archive_path: None, line_number: i + LINE_CONTENT_START, content });
    }
    result
}

// ── Word-wrap ─────────────────────────────────────────────────────────────────

/// Split text at newlines, then word-wrap any line exceeding `max_len`.
/// Empty lines are preserved. Returns individual line strings (no trailing newline).
/// Uses `str::lines()` so a trailing newline in the input does not produce a
/// spurious empty last element.
fn apply_word_wrap(text: &str, max_len: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in text.lines() {
        if line.chars().count() <= max_len {
            result.push(line.to_string());
        } else {
            let wrapped = wrap_at_words(line, max_len);
            if wrapped.is_empty() {
                result.push(String::new());
            } else {
                result.extend(wrapped);
            }
        }
    }
    result
}

/// Split `s` at word boundaries into chunks of at most `max_len` characters.
/// Words longer than `max_len` are hard-split at the character boundary.
fn wrap_at_words(s: &str, max_len: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;

    for word in s.split_whitespace() {
        let word_chars: Vec<char> = word.chars().collect();
        let word_len = word_chars.len();

        if word_len > max_len {
            // Flush current line before hard-splitting.
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
                current_len = 0;
            }
            // Hard-split into max_len chunks; keep the last chunk in `current`
            // so subsequent words can be appended to it.
            let mut pos = 0;
            while pos < word_len {
                let end = (pos + max_len).min(word_len);
                let chunk: String = word_chars[pos..end].iter().collect();
                if end == word_len {
                    current_len = end - pos;
                    current = chunk;
                } else {
                    result.push(chunk);
                }
                pos = end;
            }
        } else if current_len == 0 {
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= max_len {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            result.push(std::mem::take(&mut current));
            current.push_str(word);
            current_len = word_len;
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extension_of(name: &str) -> String {
    // Use the last segment after '::' for archive members.
    let leaf = name.rsplit("::").next().unwrap_or(name);
    if let Some(pos) = leaf.rfind('.') {
        leaf[pos + 1..].to_lowercase()
    } else {
        String::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use find_common::config::NormalizationSettings;

    fn cfg(max_line_length: usize) -> NormalizationSettings {
        NormalizationSettings { max_line_length, ..Default::default() }
    }

    fn make_lines(contents: &[&str]) -> Vec<IndexLine> {
        let mut v = vec![
            IndexLine { archive_path: None, line_number: 0, content: "file.txt".into() },
            IndexLine { archive_path: None, line_number: 1, content: String::new() },
        ];
        for (i, &c) in contents.iter().enumerate() {
            v.push(IndexLine { archive_path: None, line_number: i + LINE_CONTENT_START, content: c.into() });
        }
        v
    }

    #[test]
    fn disabled_when_max_line_length_zero() {
        let lines = make_lines(&["hello world this is a very long line that should not be wrapped"]);
        let result = normalize_lines(lines.clone(), "file.txt", &cfg(0));
        assert_eq!(result.len(), lines.len());
    }

    #[test]
    fn short_lines_unchanged() {
        let lines = make_lines(&["hello", "world"]);
        let result = normalize_lines(lines, "file.txt", &cfg(120));
        // line 0 (path) + line 1 (metadata) + 2 content lines
        assert_eq!(result.len(), 4);
        let content: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert_eq!(content[0].content, "hello");
        assert_eq!(content[1].content, "world");
    }

    #[test]
    fn long_line_is_wrapped() {
        let long = "word ".repeat(30).trim_end().to_string(); // ~149 chars
        let lines = make_lines(&[&long]);
        let result = normalize_lines(lines, "file.txt", &cfg(120));
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(content_lines.len() > 1, "long line should be split");
        for cl in &content_lines {
            assert!(cl.content.chars().count() <= 120, "line too long: {}", cl.content);
        }
    }

    #[test]
    fn json_is_pretty_printed() {
        let minified = r#"{"a":1,"b":[1,2,3],"c":{"d":true}}"#;
        let lines = make_lines(&[minified]);
        let result = normalize_lines(lines, "data.json", &cfg(120));
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        // Pretty-printed JSON should have multiple lines
        assert!(content_lines.len() > 1, "JSON should be pretty-printed");
    }

    #[test]
    fn invalid_json_falls_through_to_word_wrap() {
        let invalid = "this is not json at all";
        let lines = make_lines(&[invalid]);
        let result = normalize_lines(lines, "data.json", &cfg(120));
        // Should still produce content (not dropped)
        assert!(result.iter().any(|l| l.line_number >= LINE_CONTENT_START));
    }

    #[test]
    fn toml_is_pretty_printed() {
        let compact = "a=1\nb=\"hello\"\n[section]\nx=true";
        let lines = make_lines(&[compact]);
        let result = normalize_lines(lines, "config.toml", &cfg(120));
        assert!(result.iter().any(|l| l.line_number >= LINE_CONTENT_START));
    }

    #[test]
    fn markdown_is_wrapped() {
        let long = "word ".repeat(50).trim_end().to_string();
        let lines = make_lines(&[&long]);
        let result = normalize_lines(lines, "readme.md", &cfg(120));
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(content_lines.len() > 1, "long markdown line should be wrapped");
        for cl in &content_lines {
            assert!(cl.content.chars().count() <= 120, "line too long: {}", cl.content);
        }
    }

    #[test]
    fn long_word_is_hard_split() {
        let long_word = "a".repeat(300);
        let lines = make_lines(&[&long_word]);
        let result = normalize_lines(lines, "file.txt", &cfg(120));
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(content_lines.len() > 1, "long word should be split");
        for cl in &content_lines {
            assert!(cl.content.chars().count() <= 120, "chunk too long: {}", cl.content);
        }
        // Reassembled content should equal the original word.
        let reassembled: String = content_lines.iter().map(|l| l.content.as_str()).collect();
        assert_eq!(reassembled, long_word);
    }

    #[test]
    fn line_numbers_are_reassigned_sequentially() {
        let long = "word ".repeat(30).trim_end().to_string();
        let lines = make_lines(&[&long, "short"]);
        let result = normalize_lines(lines, "file.txt", &cfg(120));
        let mut nums: Vec<usize> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).map(|l| l.line_number).collect();
        nums.sort_unstable();
        for (i, &n) in nums.iter().enumerate() {
            assert_eq!(n, i + LINE_CONTENT_START);
        }
    }

    #[test]
    fn extension_from_archive_member_path() {
        assert_eq!(extension_of("outer.zip::inner.json"), "json");
        assert_eq!(extension_of("file.txt"), "txt");
        assert_eq!(extension_of("noext"), "");
    }

    // ── External formatter tests ──────────────────────────────────────────────

    fn cfg_with_formatter(path: &str, ext: &str) -> NormalizationSettings {
        use find_common::config::FormatterConfig;
        NormalizationSettings {
            max_line_length: 120,
            formatters: vec![FormatterConfig {
                path: path.to_string(),
                args: vec![],
                extensions: vec![ext.to_string()],
                mode: find_common::config::FormatterMode::Stdin,
            }],
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn external_formatter_success() {
        // /bin/cat reads stdin and writes it back to stdout.
        let settings = cfg_with_formatter("/bin/cat", "txt");
        let lines = make_lines(&["hello", "world"]);
        let result = normalize_lines(lines, "test.txt", &settings);
        let content: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(!content.is_empty(), "expected non-empty output");
        let joined: String = content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hello"), "expected 'hello' in output, got: {joined}");
        assert!(joined.contains("world"), "expected 'world' in output, got: {joined}");
    }

    #[cfg(unix)]
    #[test]
    fn external_formatter_nonzero_exit_skipped() {
        // /bin/false always exits 1; the formatter should be skipped and the
        // original content returned via word-wrap fallback.
        let settings = cfg_with_formatter("/bin/false", "txt");
        let lines = make_lines(&["hello", "world"]);
        let result = normalize_lines(lines, "test.txt", &settings);
        let content: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(!content.is_empty(), "expected content to be returned as-is");
        let joined: String = content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hello"), "expected 'hello' in output, got: {joined}");
        assert!(joined.contains("world"), "expected 'world' in output, got: {joined}");
    }

    #[cfg(unix)]
    #[test]
    fn external_formatter_empty_output_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("noop.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let settings = cfg_with_formatter(script.to_str().unwrap(), "txt");
        let lines = make_lines(&["hello", "world"]);
        let result = normalize_lines(lines, "test.txt", &settings);
        let content: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(!content.is_empty(), "expected fallback content when formatter produces empty output");
        let joined: String = content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hello"), "expected 'hello' in output, got: {joined}");
        assert!(joined.contains("world"), "expected 'world' in output, got: {joined}");
    }

    #[cfg(unix)]
    #[test]
    fn external_formatter_nonexistent_skipped() {
        // A path that doesn't exist should fail to spawn and be skipped gracefully.
        let settings = cfg_with_formatter("/no/such/formatter", "txt");
        let lines = make_lines(&["hello", "world"]);
        let result = normalize_lines(lines, "test.txt", &settings);
        let content: Vec<_> = result.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(!content.is_empty(), "expected content to be returned when formatter is nonexistent");
        let joined: String = content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hello"), "expected 'hello' in output, got: {joined}");
        assert!(joined.contains("world"), "expected 'world' in output, got: {joined}");
    }

    // ── normalize_batch_indexed tests ─────────────────────────────────────────

    fn make_batch_entry(idx: usize, name: &str, contents: &[&str]) -> (usize, String, Vec<IndexLine>) {
        (idx, name.to_string(), make_lines(contents))
    }

    #[cfg(unix)]
    #[test]
    fn batch_formatter_called_once_for_all_matching_files() {
        use std::os::unix::fs::PermissionsExt;
        use find_common::config::FormatterConfig;

        // Script that appends "// formatted" to every .js file it finds in the dir.
        let tmp_script = tempfile::TempDir::new().unwrap();
        let script = tmp_script.path().join("fmt.sh");
        std::fs::write(&script, "#!/bin/sh\nfor f in \"$1\"/*.js; do echo '// formatted' >> \"$f\"; done\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cfg = NormalizationSettings {
            max_line_length: 120,
            formatters: vec![FormatterConfig {
                path: script.to_str().unwrap().to_string(),
                args: vec!["{dir}".to_string()],
                extensions: vec!["js".to_string()],
                mode: find_common::config::FormatterMode::Batch,
            }],
            ..Default::default()
        };

        let mut files = vec![
            make_batch_entry(0, "a.js", &["console.log('a')"]),
            make_batch_entry(1, "b.js", &["console.log('b')"]),
            make_batch_entry(2, "readme.txt", &["not js"]),
        ];

        normalize_batch_indexed(&mut files, &cfg);

        // JS files should contain "// formatted" appended by the batch formatter.
        let js_a_content: Vec<_> = files[0].2.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        let js_b_content: Vec<_> = files[1].2.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        let joined_a: String = js_a_content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        let joined_b: String = js_b_content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined_a.contains("// formatted"), "a.js should be formatted, got: {joined_a}");
        assert!(joined_b.contains("// formatted"), "b.js should be formatted, got: {joined_b}");

        // Non-matching file should pass through unchanged via per-file path.
        let txt_content: Vec<_> = files[2].2.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        let joined_txt: String = txt_content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined_txt.contains("not js"), "readme.txt should be unchanged, got: {joined_txt}");
    }

    #[cfg(unix)]
    #[test]
    fn batch_formatter_nonzero_falls_back_to_word_wrap() {
        use std::os::unix::fs::PermissionsExt;
        use find_common::config::FormatterConfig;

        // Formatter always fails.
        let tmp_script = tempfile::TempDir::new().unwrap();
        let script = tmp_script.path().join("fail.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let long_line = "word ".repeat(30).trim_end().to_string();
        let cfg = NormalizationSettings {
            max_line_length: 120,
            formatters: vec![FormatterConfig {
                path: script.to_str().unwrap().to_string(),
                args: vec!["{dir}".to_string()],
                extensions: vec!["js".to_string()],
                mode: find_common::config::FormatterMode::Batch,
            }],
            ..Default::default()
        };

        let mut files = vec![make_batch_entry(0, "a.js", &[&long_line])];
        normalize_batch_indexed(&mut files, &cfg);

        // Formatter failed → falls through to per-file normalize_lines → word-wrap applied.
        let content: Vec<_> = files[0].2.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(content.len() > 1, "long line should be word-wrapped after formatter failure");
        for l in &content {
            assert!(l.content.len() <= 120, "line too long after fallback: {}", l.content);
        }
    }

    #[cfg(unix)]
    #[test]
    fn batch_stdin_formatter_still_works_per_file() {
        // Verify that stdin-mode formatters in normalize_batch_indexed still
        // run per-file (via the normalize_lines fallback path).
        let cfg = NormalizationSettings {
            max_line_length: 120,
            formatters: vec![find_common::config::FormatterConfig {
                path: "/bin/cat".to_string(),
                args: vec![],
                extensions: vec!["txt".to_string()],
                mode: find_common::config::FormatterMode::Stdin,
            }],
            ..Default::default()
        };

        let mut files = vec![make_batch_entry(0, "file.txt", &["hello", "world"])];
        normalize_batch_indexed(&mut files, &cfg);

        let content: Vec<_> = files[0].2.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        let joined: String = content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hello"), "stdin formatter should have run, got: {joined}");
        assert!(joined.contains("world"), "stdin formatter should have run, got: {joined}");
    }

    // ── Fake-formatter integration tests ──────────────────────────────────────
    //
    // These tests use real shell scripts as fake formatters so the full
    // subprocess-invocation path is exercised without depending on biome or
    // prettier being installed.
    //
    // The fake formatter strips leading whitespace from each line — simple
    // enough to verify round-trip correctness unambiguously.

    #[cfg(unix)]
    fn make_script(name: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    /// Extract content lines (line_number > 0) as a Vec<&str>, in order.
    fn content_lines(lines: &[IndexLine]) -> Vec<&str> {
        let mut v: Vec<&IndexLine> = lines.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        v.sort_by_key(|l| l.line_number);
        v.iter().map(|l| l.content.as_str()).collect()
    }

    /// Build a NormalizationSettings with a single stdin-mode formatter and
    /// word-wrap effectively disabled (max_line_length large enough not to fire).
    #[cfg(unix)]
    fn stdin_cfg(script_path: &str, ext: &str) -> NormalizationSettings {
        use find_common::config::FormatterConfig;
        NormalizationSettings {
            max_line_length: 10_000,
            formatters: vec![FormatterConfig {
                path: script_path.to_string(),
                args: vec![],
                extensions: vec![ext.to_string()],
                mode: find_common::config::FormatterMode::Stdin,
            }],
            ..Default::default()
        }
    }

    /// Build a NormalizationSettings with a single batch-mode formatter.
    /// Uses short timeouts (2s) so tests exercise the fallback path quickly.
    #[cfg(unix)]
    fn batch_cfg(script_path: &str, ext: &str, args: Vec<&str>) -> NormalizationSettings {
        use find_common::config::FormatterConfig;
        NormalizationSettings {
            max_line_length: 10_000,
            batch_formatter_timeout_secs: 2,
            per_file_formatter_timeout_secs: 2,
            formatters: vec![FormatterConfig {
                path: script_path.to_string(),
                args: args.into_iter().map(str::to_string).collect(),
                extensions: vec![ext.to_string()],
                mode: find_common::config::FormatterMode::Batch,
            }],
        }
    }

    // Strips leading whitespace from stdin, writes to stdout.
    #[cfg(unix)]
    const STRIP_STDIN: &str = "sed 's/^[[:space:]]*//'";

    // Strips leading whitespace from every file in the directory passed as $1.
    #[cfg(unix)]
    const STRIP_BATCH: &str = r#"
for f in "$1"/*; do
    [ -f "$f" ] || continue
    sed 's/^[[:space:]]*//' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
done
"#;

    // Strips leading whitespace but exits 1 for files containing "FAIL_ME",
    // leaving those files unmodified.
    #[cfg(unix)]
    const STRIP_BATCH_PARTIAL_FAIL: &str = r#"
rc=0
for f in "$1"/*; do
    [ -f "$f" ] || continue
    if grep -q "FAIL_ME" "$f"; then
        echo "error: cannot process $f" >&2
        rc=1
    else
        sed 's/^[[:space:]]*//' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
    fi
done
exit $rc
"#;

    #[cfg(unix)]
    #[test]
    fn stdin_mode_strips_leading_whitespace() {
        let (_dir, script) = make_script("strip.sh", STRIP_STDIN);
        let cfg = stdin_cfg(script.to_str().unwrap(), "js");
        let lines = make_lines(&["  hello", "    world"]);
        let result = normalize_lines(lines, "file.js", &cfg);
        assert_eq!(content_lines(&result), vec!["hello", "world"]);
    }

    #[cfg(unix)]
    #[test]
    fn stdin_mode_zero_byte_file_falls_through() {
        // A file with no content lines produces no output from the formatter.
        // normalize_lines should return just the path line (line 0).
        let (_dir, script) = make_script("strip.sh", STRIP_STDIN);
        let cfg = stdin_cfg(script.to_str().unwrap(), "js");
        // Only a line-0 entry — no content lines.
        let lines = vec![IndexLine { archive_path: None, line_number: 0, content: "empty.js".into() }];
        let result = normalize_lines(lines, "empty.js", &cfg);
        let content = content_lines(&result);
        assert!(content.is_empty(), "expected no content lines for empty file, got: {content:?}");
    }

    #[cfg(unix)]
    #[test]
    fn batch_mode_strips_leading_whitespace_from_all_matching_files() {
        let (_dir, script) = make_script("strip.sh", STRIP_BATCH);
        let cfg = batch_cfg(script.to_str().unwrap(), "js", vec!["{dir}"]);

        let mut files = vec![
            make_batch_entry(0, "a.js", &["  hello", "  world"]),
            make_batch_entry(1, "b.js", &["    foo", "    bar"]),
            make_batch_entry(2, "readme.txt", &["  not matched"]),
        ];
        normalize_batch_indexed(&mut files, &cfg);

        assert_eq!(content_lines(&files[0].2), vec!["hello", "world"], "a.js not stripped");
        assert_eq!(content_lines(&files[1].2), vec!["foo", "bar"],     "b.js not stripped");
        // .txt has no matching formatter — falls through to normalize_lines (word-wrap only,
        // no stripping). Leading whitespace is preserved.
        assert_eq!(content_lines(&files[2].2), vec!["  not matched"], "readme.txt should be unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn batch_mode_partial_failure_still_formats_successful_files() {
        // The formatter exits 1 for files containing "FAIL_ME" and leaves them
        // on disk unmodified. Our code should still use the formatted content
        // from the other files.
        let (_dir, script) = make_script("strip_partial.sh", STRIP_BATCH_PARTIAL_FAIL);
        let cfg = batch_cfg(script.to_str().unwrap(), "js", vec!["{dir}"]);

        let mut files = vec![
            make_batch_entry(0, "good.js",  &["  hello"]),
            make_batch_entry(1, "bad.js",   &["  FAIL_ME"]),
            make_batch_entry(2, "good2.js", &["  world"]),
        ];
        normalize_batch_indexed(&mut files, &cfg);

        assert_eq!(content_lines(&files[0].2), vec!["hello"], "good.js should be stripped");
        // bad.js: formatter left it unchanged on disk (it skipped the file); we read back
        // the original content including leading whitespace.
        assert_eq!(content_lines(&files[1].2), vec!["  FAIL_ME"], "bad.js content should be original (unstripped)");
        assert_eq!(content_lines(&files[2].2), vec!["world"], "good2.js should be stripped");
    }

    #[cfg(unix)]
    #[test]
    fn batch_mode_zero_byte_input_file_is_skipped() {
        // A file with no content lines writes a 0-byte temp file. The formatter
        // runs but produces no output for it. The file is left unhandled and
        // falls through to the per-file path (also produces no content).
        let (_dir, script) = make_script("strip.sh", STRIP_BATCH);
        let cfg = batch_cfg(script.to_str().unwrap(), "js", vec!["{dir}"]);

        let empty = vec![IndexLine { archive_path: None, line_number: 0, content: "empty.js".into() }];
        let mut files = vec![
            (0, "empty.js".to_string(), empty),
            make_batch_entry(1, "normal.js", &["  hello"]),
        ];
        normalize_batch_indexed(&mut files, &cfg);

        assert!(content_lines(&files[0].2).is_empty(), "empty file should have no content lines");
        assert_eq!(content_lines(&files[1].2), vec!["hello"], "normal.js should be stripped");
    }

    /// Script body: hangs for 5s when the dir has more than one matching file,
    /// formats immediately (appends "// fallback") when there is exactly one.
    /// Used to exercise the per-file fallback path triggered by BATCH_FORMATTER_TIMEOUT.
    #[cfg(unix)]
    const HANG_MULTI_BATCH: &str = r#"
count=$(ls "$1"/*.js 2>/dev/null | wc -l | tr -d ' ')
if [ "$count" -gt 1 ]; then
    sleep 5
else
    for f in "$1"/*.js; do
        printf '\n// fallback' >> "$f"
    done
fi
"#;

    #[cfg(unix)]
    #[test]
    fn batch_timeout_falls_back_to_per_file() {
        // The script hangs when given multiple files → triggers the batch timeout
        // (2s in test builds).  The per-file fallback then re-runs the formatter
        // on each file individually, where it finds exactly one file and succeeds.
        let (_dir, script) = make_script("hang_multi.sh", HANG_MULTI_BATCH);
        let cfg = batch_cfg(script.to_str().unwrap(), "js", vec!["{dir}"]);

        let mut files = vec![
            make_batch_entry(0, "a.js", &["console.log('a')"]),
            make_batch_entry(1, "b.js", &["console.log('b')"]),
            make_batch_entry(2, "readme.txt", &["not js"]),
        ];
        normalize_batch_indexed(&mut files, &cfg);

        // Both JS files should have been formatted by the per-file fallback.
        let a = content_lines(&files[0].2).join("\n");
        let b = content_lines(&files[1].2).join("\n");
        assert!(a.contains("// fallback"), "a.js should be formatted by per-file fallback, got: {a}");
        assert!(b.contains("// fallback"), "b.js should be formatted by per-file fallback, got: {b}");

        // Non-JS file should be unaffected.
        let txt = content_lines(&files[2].2).join("\n");
        assert!(txt.contains("not js"), "readme.txt should be unchanged, got: {txt}");
    }

    #[cfg(unix)]
    #[test]
    fn batch_mode_already_formatted_file_is_used_as_is() {
        // A file that's already stripped has identical content before and after
        // the formatter. We use the in-place file regardless — no comparison.
        let (_dir, script) = make_script("strip.sh", STRIP_BATCH);
        let cfg = batch_cfg(script.to_str().unwrap(), "js", vec!["{dir}"]);

        let mut files = vec![
            make_batch_entry(0, "clean.js", &["already clean", "no leading whitespace"]),
        ];
        normalize_batch_indexed(&mut files, &cfg);

        assert_eq!(
            content_lines(&files[0].2),
            vec!["already clean", "no leading whitespace"],
            "already-formatted file should pass through unchanged"
        );
    }
}
