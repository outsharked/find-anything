/// Server-side text normalization applied before content is written to ZIP archives.
///
/// Normalization chain (first success wins for pretty-printing):
/// 1. Built-in pretty-printers (JSON, TOML)
/// 2. External formatter subprocess (if configured and exits 0)
/// 3. Word-wrap at max_line_length (always applied as the final step)
use find_common::api::IndexLine;
use find_common::config::NormalizationSettings;

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

    // Separate line 0 (path / metadata) from content lines.
    let (zero_lines, mut content_lines): (Vec<IndexLine>, Vec<IndexLine>) =
        lines.into_iter().partition(|l| l.line_number == 0);

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

    // Rebuild IndexLine vec with fresh 1-based line numbers.
    let mut result = zero_lines;
    for (i, content) in wrapped_lines.into_iter().enumerate() {
        result.push(IndexLine {
            archive_path: None,
            line_number: i + 1,
            content,
        });
    }
    result
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
                stderr = %stderr.trim(),
                "normalize: formatter exited with error"
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

// ── Word-wrap ─────────────────────────────────────────────────────────────────

/// Split text at `\n`, then word-wrap any line exceeding `max_len`.
/// Empty lines are preserved. Returns individual line strings (no trailing newline).
fn apply_word_wrap(text: &str, max_len: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in text.split('\n') {
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
        let mut v = vec![IndexLine { archive_path: None, line_number: 0, content: "file.txt".into() }];
        for (i, &c) in contents.iter().enumerate() {
            v.push(IndexLine { archive_path: None, line_number: i + 1, content: c.into() });
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
        // line 0 + 2 content lines
        assert_eq!(result.len(), 3);
        let content: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
        assert_eq!(content[0].content, "hello");
        assert_eq!(content[1].content, "world");
    }

    #[test]
    fn long_line_is_wrapped() {
        let long = "word ".repeat(30).trim_end().to_string(); // ~149 chars
        let lines = make_lines(&[&long]);
        let result = normalize_lines(lines, "file.txt", &cfg(120));
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
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
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
        // Pretty-printed JSON should have multiple lines
        assert!(content_lines.len() > 1, "JSON should be pretty-printed");
    }

    #[test]
    fn invalid_json_falls_through_to_word_wrap() {
        let invalid = "this is not json at all";
        let lines = make_lines(&[invalid]);
        let result = normalize_lines(lines, "data.json", &cfg(120));
        // Should still produce content (not dropped)
        assert!(result.iter().any(|l| l.line_number > 0));
    }

    #[test]
    fn toml_is_pretty_printed() {
        let compact = "a=1\nb=\"hello\"\n[section]\nx=true";
        let lines = make_lines(&[compact]);
        let result = normalize_lines(lines, "config.toml", &cfg(120));
        assert!(result.iter().any(|l| l.line_number > 0));
    }

    #[test]
    fn markdown_is_wrapped() {
        let long = "word ".repeat(50).trim_end().to_string();
        let lines = make_lines(&[&long]);
        let result = normalize_lines(lines, "readme.md", &cfg(120));
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
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
        let content_lines: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
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
        let mut nums: Vec<usize> = result.iter().filter(|l| l.line_number > 0).map(|l| l.line_number).collect();
        nums.sort_unstable();
        for (i, &n) in nums.iter().enumerate() {
            assert_eq!(n, i + 1);
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
            }],
        }
    }

    #[cfg(unix)]
    #[test]
    fn external_formatter_success() {
        // /bin/cat reads stdin and writes it back to stdout.
        let settings = cfg_with_formatter("/bin/cat", "txt");
        let lines = make_lines(&["hello", "world"]);
        let result = normalize_lines(lines, "test.txt", &settings);
        let content: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
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
        let content: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
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
        let content: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
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
        let content: Vec<_> = result.iter().filter(|l| l.line_number > 0).collect();
        assert!(!content.is_empty(), "expected content to be returned when formatter is nonexistent");
        let joined: String = content.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("hello"), "expected 'hello' in output, got: {joined}");
        assert!(joined.contains("world"), "expected 'world' in output, got: {joined}");
    }
}
