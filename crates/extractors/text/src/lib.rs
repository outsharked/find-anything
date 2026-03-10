use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use find_extract_types::IndexLine;
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
                line_number: i + 1,
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
            line_number: i + 1,
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
        | "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar"
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

    // If frontmatter exists, add it as line_number=0
    if let Some(data) = parsed.data {
        if let Some(frontmatter_lines) = extract_frontmatter_fields(&data) {
            lines.extend(frontmatter_lines);
        }
    }

    // Index the content (either full content if no frontmatter, or content after frontmatter).
    // Empty lines are stored with empty content so line numbers stay dense and context
    // retrieval (BETWEEN lo AND hi) reliably finds neighbours around any match.
    for (i, line) in parsed.content.lines().enumerate() {
        lines.push(IndexLine {
            archive_path: None,
            line_number: i + 1,
            content: line.trim().to_string(),
        });
    }

    lines
}

/// Convert frontmatter Pod to IndexLines at line_number=0.
fn extract_frontmatter_fields(data: &Pod) -> Option<Vec<IndexLine>> {
    if let Pod::Hash(mapping) = data {
        let mut lines = Vec::new();

        for (key, value) in mapping {
            if let Some(value_str) = pod_to_string(value) {
                let content = format!("[FRONTMATTER:{}] {}", key, value_str);
                lines.push(IndexLine {
                    archive_path: None,
                    line_number: 0,
                    content,
                });
            }
        }

        Some(lines)
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

        // Check frontmatter fields are at line 0
        let frontmatter_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.line_number == 0 && l.content.starts_with("[FRONTMATTER:"))
            .collect();

        assert_eq!(frontmatter_lines.len(), 5);

        // Check specific fields
        assert!(frontmatter_lines
            .iter()
            .any(|l| l.content == "[FRONTMATTER:title] Test Document"));
        assert!(frontmatter_lines
            .iter()
            .any(|l| l.content == "[FRONTMATTER:author] John Doe"));
        assert!(frontmatter_lines
            .iter()
            .any(|l| l.content == "[FRONTMATTER:tags] rust, indexing"));
        assert!(frontmatter_lines
            .iter()
            .any(|l| l.content == "[FRONTMATTER:count] 42"));
        assert!(frontmatter_lines
            .iter()
            .any(|l| l.content == "[FRONTMATTER:active] true"));

        // Check content is indexed
        let content_lines: Vec<_> = lines.iter().filter(|l| l.line_number > 0).collect();
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

        // Content should still be indexed
        assert!(lines.iter().any(|l| l.content == "# Regular Markdown"));
        assert!(lines
            .iter()
            .any(|l| l.content == "No frontmatter here."));
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

        // Content should be indexed
        assert!(lines.iter().any(|l| l.content == "# Content"));
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

        // Nested objects should be serialized
        let frontmatter_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.content.starts_with("[FRONTMATTER:"))
            .collect();

        assert_eq!(frontmatter_lines.len(), 1);
        assert!(frontmatter_lines[0]
            .content
            .starts_with("[FRONTMATTER:metadata]"));
        // Should contain nested data
        assert!(frontmatter_lines[0].content.contains("author"));
        assert!(frontmatter_lines[0].content.contains("John"));
    }
}
