use std::path::Path;

use find_extract_types::IndexLine;
use find_extract_types::ExtractorConfig;
use scraper::{ElementRef, Html, Selector};

/// Accept .html, .htm, .xhtml files.
pub fn accepts(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "html" | "htm" | "xhtml"
    )
}

/// Extract text content from HTML bytes.
///
/// Used by `find-extract-dispatch` for archive members and other in-memory sources.
pub fn extract_from_bytes(bytes: &[u8], _name: &str, _cfg: &ExtractorConfig) -> Vec<IndexLine> {
    let src = String::from_utf8_lossy(bytes);
    extract_from_str(&src)
}

/// Extract text content from an HTML file.
///
/// Metadata lines (line_number = 0):
///   - `[HTML:title]` from `<title>`
///   - `[HTML:description]` from `<meta name="description" content="…">`
///
/// Content lines (line_number ≥ 1): visible text from block-level elements
/// (h1–h6, p, li, td, th, pre, blockquote, figcaption), skipping elements
/// inside nav/header/footer/script/style.
pub fn extract(path: &Path, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let bytes = std::fs::read(path)?;
    let src = String::from_utf8_lossy(&bytes);
    Ok(extract_from_str(&src))
}

const EXCLUDED_TAGS: &[&str] = &["nav", "header", "footer", "script", "style"];

fn extract_from_str(src: &str) -> Vec<IndexLine> {
    let document = Html::parse_document(src);
    let mut lines = Vec::new();

    // ── Metadata: <title> ────────────────────────────────────────────────────
    let title_sel = Selector::parse("title").unwrap();
    if let Some(el) = document.select(&title_sel).next() {
        let text = el.text().collect::<Vec<_>>().join(" ");
        let text = text.trim();
        if !text.is_empty() {
            lines.push(IndexLine {
                archive_path: None,
                line_number: 0,
                content: format!("[HTML:title] {}", text),
            });
        }
    }

    // ── Metadata: <meta name="description"> ──────────────────────────────────
    let desc_sel = Selector::parse("meta[name='description']").unwrap();
    if let Some(el) = document.select(&desc_sel).next() {
        if let Some(content) = el.value().attr("content") {
            let text = content.trim();
            if !text.is_empty() {
                lines.push(IndexLine {
                    archive_path: None,
                    line_number: 0,
                    content: format!("[HTML:description] {}", text),
                });
            }
        }
    }

    // ── Content: block-level elements ─────────────────────────────────────────
    let content_sel =
        Selector::parse("h1, h2, h3, h4, h5, h6, p, li, td, th, pre, blockquote, figcaption")
            .unwrap();

    let mut line_number = 0usize;

    for el in document.select(&content_sel) {
        // Skip elements inside excluded containers
        if in_excluded_container(el) {
            continue;
        }

        // Collect all text nodes, collapse whitespace
        let text: String = el.text().collect::<Vec<_>>().join(" ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

        if text.is_empty() {
            continue;
        }

        line_number += 1;
        lines.push(IndexLine {
            archive_path: None,
            line_number,
            content: text,
        });
    }

    lines
}

/// Return true if `el` has an ancestor whose tag is in EXCLUDED_TAGS.
fn in_excluded_container(el: ElementRef<'_>) -> bool {
    el.ancestors()
        .filter_map(ElementRef::wrap)
        .any(|ancestor| EXCLUDED_TAGS.contains(&ancestor.value().name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accepts() {
        assert!(accepts(Path::new("index.html")));
        assert!(accepts(Path::new("page.htm")));
        assert!(accepts(Path::new("doc.xhtml")));
        assert!(accepts(Path::new("INDEX.HTML")));
        assert!(!accepts(Path::new("script.js")));
        assert!(!accepts(Path::new("style.css")));
        assert!(!accepts(Path::new("readme.md")));
    }

    #[test]
    fn test_title_and_description() {
        let html = r#"<!DOCTYPE html>
<html>
<head>
  <title>My Page Title</title>
  <meta name="description" content="A great page about stuff">
</head>
<body><p>Hello world</p></body>
</html>"#;

        let lines = extract_from_str(html);
        assert!(lines
            .iter()
            .any(|l| l.line_number == 0 && l.content == "[HTML:title] My Page Title"));
        assert!(lines.iter().any(|l| l.line_number == 0
            && l.content == "[HTML:description] A great page about stuff"));
    }

    #[test]
    fn test_content_extraction() {
        let html = r#"<html><body>
<h1>Main Heading</h1>
<p>A paragraph with <strong>bold</strong> text.</p>
<ul>
  <li>Item one</li>
  <li>Item two</li>
</ul>
</body></html>"#;

        let lines = extract_from_str(html);
        let content: Vec<&str> = lines
            .iter()
            .filter(|l| l.line_number > 0)
            .map(|l| l.content.as_str())
            .collect();

        assert!(content.contains(&"Main Heading"));
        assert!(content.iter().any(|s| s.contains("paragraph")));
        assert!(content.contains(&"Item one"));
        assert!(content.contains(&"Item two"));
    }

    #[test]
    fn test_excluded_containers() {
        let html = r#"<html><body>
<nav><p>Nav link 1</p><p>Nav link 2</p></nav>
<header><p>Site header</p></header>
<footer><p>Footer text</p></footer>
<script>var x = 1;</script>
<style>body { color: red; }</style>
<p>Visible content</p>
</body></html>"#;

        let lines = extract_from_str(html);
        let content: Vec<&str> = lines
            .iter()
            .filter(|l| l.line_number > 0)
            .map(|l| l.content.as_str())
            .collect();

        // Only the outside <p> should appear
        assert_eq!(content, vec!["Visible content"]);
    }

    #[test]
    fn test_table_cells() {
        let html = r#"<html><body>
<table>
  <tr><th>Name</th><th>Value</th></tr>
  <tr><td>Foo</td><td>42</td></tr>
</table>
</body></html>"#;

        let lines = extract_from_str(html);
        let content: Vec<&str> = lines
            .iter()
            .filter(|l| l.line_number > 0)
            .map(|l| l.content.as_str())
            .collect();

        assert!(content.contains(&"Name"));
        assert!(content.contains(&"Value"));
        assert!(content.contains(&"Foo"));
        assert!(content.contains(&"42"));
    }

    #[test]
    fn test_empty_elements_skipped() {
        let html = r#"<html><body>
<p></p>
<p>   </p>
<p>Real content</p>
</body></html>"#;

        let lines = extract_from_str(html);
        let content: Vec<&str> = lines
            .iter()
            .filter(|l| l.line_number > 0)
            .map(|l| l.content.as_str())
            .collect();

        assert_eq!(content, vec!["Real content"]);
    }
}
