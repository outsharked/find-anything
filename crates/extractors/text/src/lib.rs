use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use find_extract_types::{IndexLine, LINE_METADATA, LINE_CONTENT_START};
use find_extract_types::ExtractorConfig;
use gray_matter::{engine::YAML, Matter, Pod};

/// Extract text content from a file.
///
/// Supports:
/// - Plain text files
/// - Source code
/// - Markdown (with frontmatter extraction)
/// - Config files (JSON, YAML, TOML, etc.)
///
/// Content is truncated at `cfg.max_content_kb` bytes.
///
/// # Returns
/// Vector of IndexLine objects, one per non-empty line
pub fn extract(path: &Path, cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let content_limit = cfg.max_content_kb * 1024;

    // Check if this is a Markdown file that might have frontmatter
    let is_markdown = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false);

    if is_markdown {
        // Read up to content_limit bytes to parse frontmatter
        let file = std::fs::File::open(path)?;
        let mut buf = Vec::new();
        file.take(content_limit as u64).read_to_end(&mut buf)?;
        let content = String::from_utf8_lossy(&buf);
        return Ok(extract_markdown_with_frontmatter(&content));
    }

    // Non-Markdown: use efficient line-by-line reading, bounded by content limit
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file.take(content_limit as u64));

    Ok(reader
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            line.ok().map(|content| IndexLine {
                archive_path: None,
                line_number: i + LINE_CONTENT_START,
                content,
            })
        })
        .collect())
}

/// Check if a file path is likely a text file based on extension or by sniffing the file on disk.
pub fn accepts(path: &Path) -> bool {
    match ext_verdict(path) {
        Some(is_text) => is_text,
        None => {
            // Fallback: sniff first 8 KB from disk
            if let Ok(mut f) = std::fs::File::open(path) {
                let mut buf = vec![0u8; 8192];
                if let Ok(n) = f.read(&mut buf) {
                    buf.truncate(n);
                    return content_inspector::inspect(&buf).is_text();
                }
            }
            false
        }
    }
}

/// Check if an archive member (in-memory bytes) is likely a text file.
///
/// Uses the same extension whitelist as `accepts`, but falls back to sniffing
/// the provided bytes rather than reading from disk (which would fail for
/// archive members that have no on-disk path).
pub fn accepts_bytes(path: &Path, bytes: &[u8]) -> bool {
    match ext_verdict(path) {
        Some(is_text) => is_text,
        None => {
            let sniff = &bytes[..bytes.len().min(8192)];
            content_inspector::inspect(sniff).is_text()
        }
    }
}

/// Returns Some(true) for known text extensions, Some(false) for known binary
/// extensions, and None when the extension is unknown (caller should sniff).
fn ext_verdict(path: &Path) -> Option<bool> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    if is_text_ext(ext) {
        return Some(true);
    }
    if is_binary_ext(ext) {
        return Some(false);
    }
    None
}

/// Extract text content from in-memory bytes.
///
/// Used by `find-extract-dispatch` for archive members and other in-memory sources.
/// Does not include a filename line — the caller adds that.
pub fn extract_from_bytes(bytes: &[u8], name: &str, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let is_markdown = {
        let n = name.to_lowercase();
        n.ends_with(".md") || n.ends_with(".markdown")
    };
    // Use lossy conversion so Windows-1252 / Latin-1 encoded files (common in
    // legacy code archives) produce content with replacement chars rather than
    // silently returning empty lines.
    let content = String::from_utf8_lossy(bytes).into_owned();
    if is_markdown {
        Ok(extract_markdown_with_frontmatter(&content))
    } else {
        Ok(lines_from_str(&content, None))
    }
}

/// Convert a string to IndexLines (used by archive extractor for text entries).
pub fn lines_from_str(content: &str, archive_path: Option<String>) -> Vec<IndexLine> {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| IndexLine {
            archive_path: archive_path.clone(),
            line_number: i + LINE_CONTENT_START,
            content: line.to_string(),
        })
        .collect()
}

pub fn is_text_ext(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "rs" | "ts" | "js" | "jsx" | "tsx" | "py" | "rb" | "go" | "java"
        | "c" | "cpp" | "h" | "hpp" | "cs" | "swift" | "kt" | "scala"
        | "r" | "m" | "pl" | "sh" | "bash" | "zsh" | "fish" | "ps1"
        | "lua" | "vim" | "el" | "clj" | "hs" | "ml" | "fs" | "ex"
        | "erl" | "dart" | "jl" | "nim" | "zig" | "s" | "asm"
        | "css" | "scss" | "sass" | "less" | "styl"
        | "html" | "htm" | "xml" | "svg" | "md" | "markdown" | "rst"
        | "tex" | "adoc" | "org"
        | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf"
        | "env" | "properties" | "plist" | "nix" | "hcl" | "tf"
        | "csv" | "tsv" | "sql" | "graphql" | "gql" | "proto"
        | "txt" | "log" | "diff" | "patch"
        | "lock"  // Cargo.lock, package-lock.json, etc.
        // Windows scripting / automation
        | "cmd" | "bat" | "vbs" | "vba" | "bas" | "cls" | "ahk" | "au3" | "reg"
        // Editor / IDE project files (text-based)
        | "workspace" | "code-workspace" | "sublime-project" | "sublime-workspace"
        | "editorconfig" | "gitignore" | "gitattributes" | "gitmodules"
        | "dockerignore" | "npmignore" | "eslintignore" | "prettierignore"
        // Misc text formats
        | "makefile" | "dockerfile" | "procfile" | "gemfile" | "rakefile"
        | "brewfile" | "csproj" | "vcxproj" | "sln" | "gradle"
        | "mod" | "sum"  // Go modules
        | "cabal"
    )
}

/// Returns true if `path` has a known-binary extension, meaning there is no
/// useful text to extract and the file need not be read beyond a small MIME-sniff
/// buffer.  This is the same logic as `is_binary_ext` but exposed for callers
/// (e.g. `find-extract-dispatch`) that want to short-circuit before reading.
pub fn is_binary_ext_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_binary_ext)
        .unwrap_or(false)
}

fn is_binary_ext(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "ico" | "webp" | "heic"
        | "mp3" | "mp4" | "avi" | "mov" | "mkv" | "flac" | "wav" | "ogg"
        | "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx"
        | "zip" | "tar" | "gz" | "bz2" | "xz" | "7z"
        | "exe" | "dll" | "so" | "dylib" | "sys" | "scr" | "efi"
        | "class" | "jar" | "pyc" | "pyd"
        | "o" | "a" | "lib" | "obj" | "wasm"
        | "deb" | "rpm" | "pkg" | "msi" | "snap" | "flatpak"
        // Disk / VM images
        | "bin" | "img" | "iso" | "dmg" | "vmdk" | "vhd" | "vhdx" | "vdi"
        | "qcow2" | "ova"
        | "db" | "sqlite" | "sqlite3" | "mdb"
        | "ttf" | "otf" | "woff" | "woff2"
        // dtSearch and other search-engine binary index formats
        | "ix" | "ixd" | "ixh"
    )
}

/// Extract content from Markdown file, parsing frontmatter if present.
fn extract_markdown_with_frontmatter(content: &str) -> Vec<IndexLine> {
    let mut lines = Vec::new();

    // Try to parse frontmatter
    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(content);

    // If frontmatter exists, concatenate all fields into the metadata slot (LINE_METADATA).
    if let Some(data) = parsed.data {
        if let Some(meta) = extract_frontmatter_metadata(&data) {
            lines.push(meta);
        }
    }

    // Index the content (either full content if no frontmatter, or content after frontmatter).
    // Empty lines are stored with empty content so line numbers stay dense and context
    // retrieval (BETWEEN lo AND hi) reliably finds neighbours around any match.
    for (i, line) in parsed.content.lines().enumerate() {
        lines.push(IndexLine {
            archive_path: None,
            line_number: i + LINE_CONTENT_START,
            content: line.trim().to_string(),
        });
    }

    lines
}

/// Convert frontmatter Pod to a single concatenated IndexLine at LINE_METADATA.
fn extract_frontmatter_metadata(data: &Pod) -> Option<IndexLine> {
    if let Pod::Hash(mapping) = data {
        let parts: Vec<String> = mapping
            .iter()
            .filter_map(|(key, value)| {
                pod_to_string(value).map(|v| format!("[FRONTMATTER:{}] {}", key, v))
            })
            .collect();

        if parts.is_empty() {
            return None;
        }

        Some(IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: parts.join(" "),
        })
    } else {
        None
    }
}

/// Convert a Pod value to a searchable string.
fn pod_to_string(pod: &Pod) -> Option<String> {
    match pod {
        Pod::String(s) => Some(s.clone()),
        Pod::Integer(n) => Some(n.to_string()),
        Pod::Float(f) => Some(f.to_string()),
        Pod::Boolean(b) => Some(b.to_string()),
        Pod::Array(arr) => {
            let items: Vec<String> = arr.iter().filter_map(pod_to_string).collect();
            Some(items.join(", "))
        }
        Pod::Null => None,
        Pod::Hash(map) => {
            // For nested objects, serialize as key-value pairs
            let items: Vec<String> = map
                .iter()
                .filter_map(|(k, v)| pod_to_string(v).map(|val| format!("{}: {}", k, val)))
                .collect();
            Some(format!("{{{}}}", items.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frontmatter_extraction() {
        let content = r#"---
title: Test Document
author: John Doe
tags: [rust, indexing]
count: 42
active: true
---

# Heading

Content here
More content
"#;

        let lines = extract_markdown_with_frontmatter(content);

        // Check frontmatter is consolidated into the metadata slot (LINE_METADATA = 1).
        let meta_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.line_number == LINE_METADATA)
            .collect();

        assert_eq!(meta_lines.len(), 1);
        let meta = &meta_lines[0].content;

        // All fields should appear in the single concatenated metadata line.
        assert!(meta.contains("[FRONTMATTER:title] Test Document"), "meta={meta}");
        assert!(meta.contains("[FRONTMATTER:author] John Doe"), "meta={meta}");
        assert!(meta.contains("[FRONTMATTER:tags] rust, indexing"), "meta={meta}");
        assert!(meta.contains("[FRONTMATTER:count] 42"), "meta={meta}");
        assert!(meta.contains("[FRONTMATTER:active] true"), "meta={meta}");

        // Check content is indexed starting at LINE_CONTENT_START (2).
        let content_lines: Vec<_> = lines.iter().filter(|l| l.line_number >= LINE_CONTENT_START).collect();
        assert!(content_lines.len() > 0);
        assert!(content_lines
            .iter()
            .any(|l| l.content == "# Heading"));
        assert!(content_lines
            .iter()
            .any(|l| l.content == "Content here"));
    }

    #[test]
    fn test_no_frontmatter() {
        let content = r#"# Regular Markdown

No frontmatter here.
"#;

        let lines = extract_markdown_with_frontmatter(content);

        // Should have no frontmatter lines
        let frontmatter_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.content.starts_with("[FRONTMATTER:"))
            .collect();

        assert_eq!(frontmatter_lines.len(), 0);

        // Content should still be indexed (starting at LINE_CONTENT_START)
        assert!(lines.iter().any(|l| l.line_number >= LINE_CONTENT_START && l.content == "# Regular Markdown"));
        assert!(lines
            .iter()
            .any(|l| l.line_number >= LINE_CONTENT_START && l.content == "No frontmatter here."));
    }

    #[test]
    fn test_invalid_frontmatter() {
        let content = r#"---
invalid yaml: [unclosed
---

# Content
"#;

        // Should not panic, just treat as regular content
        let lines = extract_markdown_with_frontmatter(content);

        // Either parses without frontmatter, or includes everything as content
        // (exact behavior depends on gray_matter error handling)
        assert!(lines.len() > 0);
    }

    #[test]
    fn test_empty_frontmatter() {
        let content = r#"---
---

# Content
"#;

        let lines = extract_markdown_with_frontmatter(content);

        // Empty frontmatter should produce no frontmatter lines
        let frontmatter_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.content.starts_with("[FRONTMATTER:"))
            .collect();

        assert_eq!(frontmatter_lines.len(), 0);

        // Content should be indexed (starting at LINE_CONTENT_START)
        assert!(lines.iter().any(|l| l.line_number >= LINE_CONTENT_START && l.content == "# Content"));
    }

    #[test]
    fn test_nested_frontmatter() {
        let content = r#"---
metadata:
  author: John
  date: 2024-01-01
---

# Content
"#;

        let lines = extract_markdown_with_frontmatter(content);

        // Nested objects should be serialized into the single metadata line.
        let meta_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.line_number == LINE_METADATA)
            .collect();

        assert_eq!(meta_lines.len(), 1);
        assert!(meta_lines[0].content.starts_with("[FRONTMATTER:metadata]"));
        // Should contain nested data
        assert!(meta_lines[0].content.contains("author"));
        assert!(meta_lines[0].content.contains("John"));
    }

    // ── is_text_ext / is_binary_ext_path ─────────────────────────────────────

    #[test]
    fn text_extensions_are_accepted() {
        for ext in &["rs", "py", "js", "ts", "txt", "md", "json", "yaml", "toml", "sh", "sql"] {
            assert!(is_text_ext(ext), ".{ext} should be a text extension");
        }
    }

    #[test]
    fn text_ext_is_case_insensitive() {
        assert!(is_text_ext("RS"), ".RS should be treated as .rs");
        assert!(is_text_ext("TXT"), ".TXT should be treated as .txt");
    }

    #[test]
    fn binary_ext_path_returns_true_for_binary() {
        for ext in &["jpg", "png", "exe", "dll", "zip", "pdf", "mp3"] {
            let name = format!("file.{ext}");
            assert!(is_binary_ext_path(Path::new(&name)), ".{ext} should be binary");
        }
    }

    #[test]
    fn binary_ext_path_returns_false_for_text() {
        assert!(!is_binary_ext_path(Path::new("main.rs")));
        assert!(!is_binary_ext_path(Path::new("notes.txt")));
    }

    #[test]
    fn binary_ext_path_returns_false_for_no_extension() {
        assert!(!is_binary_ext_path(Path::new("Makefile")));
    }

    // ── accepts ───────────────────────────────────────────────────────────────

    #[test]
    fn accepts_known_text_extensions_without_reading_disk() {
        // These are all known-text extensions: accepts() returns Some(true) from ext_verdict.
        assert!(accepts(Path::new("main.rs")));
        assert!(accepts(Path::new("config.toml")));
        assert!(accepts(Path::new("README.md")));
        assert!(accepts(Path::new("data.json")));
    }

    #[test]
    fn accepts_rejects_known_binary_extensions() {
        assert!(!accepts(Path::new("photo.jpg")));
        assert!(!accepts(Path::new("program.exe")));
        assert!(!accepts(Path::new("library.zip")));
    }

    // ── accepts_bytes ─────────────────────────────────────────────────────────

    #[test]
    fn accepts_bytes_known_text_ext_returns_true_without_sniffing() {
        // .rs is a known text extension — bytes are irrelevant.
        assert!(accepts_bytes(Path::new("code.rs"), b"\x00\x01\x02\xFF"));
    }

    #[test]
    fn accepts_bytes_known_binary_ext_returns_false_without_sniffing() {
        assert!(!accepts_bytes(Path::new("image.jpg"), b"plaintext content"));
    }

    #[test]
    fn accepts_bytes_unknown_ext_sniffs_content() {
        // Unknown extension: falls back to content_inspector
        let text_bytes = b"This is plain ASCII text content with no special chars";
        let binary_bytes = b"\x00\x01\x02\x03\xFF\xFE\xFD\xFC\x00\x00\x00";
        assert!(accepts_bytes(Path::new("unknown_file"), text_bytes));
        assert!(!accepts_bytes(Path::new("unknown_file"), binary_bytes));
    }

    // ── extract_from_bytes ────────────────────────────────────────────────────

    #[test]
    fn extract_from_bytes_plain_text_returns_lines() {
        use find_extract_types::ExtractorConfig;
        let cfg = ExtractorConfig::default();
        let content = b"line one\nline two\nline three";
        let lines = extract_from_bytes(content, "file.txt", &cfg).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().any(|l| l.content == "line one"));
        assert!(lines.iter().any(|l| l.content == "line three"));
        // All lines should start at LINE_CONTENT_START or above.
        assert!(lines.iter().all(|l| l.line_number >= LINE_CONTENT_START));
    }

    #[test]
    fn extract_from_bytes_markdown_produces_frontmatter_metadata() {
        use find_extract_types::ExtractorConfig;
        let cfg = ExtractorConfig::default();
        let content = b"---\ntitle: Hello\n---\n# Body\n";
        let lines = extract_from_bytes(content, "doc.md", &cfg).unwrap();
        let has_meta = lines.iter().any(|l| l.line_number == LINE_METADATA && l.content.contains("Hello"));
        assert!(has_meta, "markdown with frontmatter should produce metadata line");
    }

    #[test]
    fn extract_from_bytes_empty_input_returns_empty() {
        use find_extract_types::ExtractorConfig;
        let cfg = ExtractorConfig::default();
        let lines = extract_from_bytes(b"", "empty.txt", &cfg).unwrap();
        assert!(lines.is_empty());
    }

    // ── lines_from_str ────────────────────────────────────────────────────────

    #[test]
    fn lines_from_str_assigns_sequential_line_numbers() {
        let lines = lines_from_str("alpha\nbeta\ngamma", None);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].content, "alpha");
        assert_eq!(lines[0].line_number, LINE_CONTENT_START);
        assert_eq!(lines[1].line_number, LINE_CONTENT_START + 1);
        assert_eq!(lines[2].line_number, LINE_CONTENT_START + 2);
    }

    #[test]
    fn lines_from_str_propagates_archive_path() {
        let ap = Some("archive.zip".to_string());
        let lines = lines_from_str("one\ntwo", ap.clone());
        assert!(lines.iter().all(|l| l.archive_path == ap));
    }

    #[test]
    fn lines_from_str_empty_string_returns_empty() {
        assert!(lines_from_str("", None).is_empty());
    }
}
