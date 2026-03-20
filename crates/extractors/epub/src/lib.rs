use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use find_extract_types::{IndexLine, LINE_METADATA, LINE_CONTENT_START};
use find_extract_types::ExtractorConfig;
use quick_xml::events::Event;

/// Accept .epub files.
pub fn accepts(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("epub"))
        .unwrap_or(false)
}

/// Extract text from EPUB bytes.
///
/// Used by `find-extract-dispatch` for archive members. Writes to a temp file
/// and delegates to `extract`.
pub fn extract_from_bytes(bytes: &[u8], _name: &str, cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    use std::io::Write;
    let mut tmp = tempfile::Builder::new()
        .suffix(".epub")
        .tempfile()?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    extract(tmp.path(), cfg)
}

/// Extract text from an EPUB file.
///
/// Parsing sequence:
///   1. META-INF/container.xml → OPF file path
///   2. OPF → metadata (title, creator, publisher, language) + spine order
///   3. Each spine XHTML file → paragraphs via text-node walk
///
/// Metadata lines use line_number = 0; content lines start at 1.
pub fn extract(path: &Path, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // Step 1: locate OPF
    let opf_path = {
        let mut entry = archive.by_name("META-INF/container.xml")?;
        let mut xml = String::new();
        entry.read_to_string(&mut xml)?;
        find_opf_path(&xml)?
    };

    // Step 2: parse OPF — metadata + spine hrefs
    let (metadata_lines, spine_hrefs) = {
        let mut entry = archive.by_name(&opf_path)?;
        let mut xml = String::new();
        entry.read_to_string(&mut xml)?;
        let opf_dir = opf_path.rfind('/').map(|i| &opf_path[..i]).unwrap_or("");
        parse_opf(&xml, opf_dir)
    };

    let mut lines = metadata_lines;
    let mut content_line = LINE_CONTENT_START - 1;

    // Step 3: extract text from each spine item
    for href in &spine_hrefs {
        let xml = match archive.by_name(href) {
            Ok(mut entry) => {
                let mut s = String::new();
                // Ignore read errors — skip unreadable spine items
                let _ = entry.read_to_string(&mut s);
                s
            }
            Err(_) => continue,
        };

        for text in extract_xhtml_text(&xml) {
            content_line += 1;
            lines.push(IndexLine {
                archive_path: None,
                line_number: content_line,
                content: text,
            });
        }
    }

    Ok(lines)
}

// ── container.xml ─────────────────────────────────────────────────────────────

/// Find the `full-path` attribute from the first `<rootfile>` element.
fn find_opf_path(xml: &str) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if e.local_name().as_ref() == b"rootfile" =>
            {
                if let Some(path) = get_attr(&e, b"full-path") {
                    return Ok(path);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    anyhow::bail!("rootfile not found in META-INF/container.xml")
}

// ── OPF ───────────────────────────────────────────────────────────────────────

/// Parse the OPF package document.
///
/// Returns:
///   - metadata IndexLines (single line at LINE_METADATA, or empty vec)
///   - ordered list of content file paths (resolved relative to OPF dir)
fn parse_opf(xml: &str, opf_dir: &str) -> (Vec<IndexLine>, Vec<String>) {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut parts: Vec<String> = Vec::new();
    let mut manifest: HashMap<String, String> = HashMap::new();
    let mut spine_idrefs: Vec<String> = Vec::new();

    let mut current_field: Option<&'static str> = None;
    let mut in_manifest = false;
    let mut in_spine = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                match e.local_name().as_ref() {
                    b"manifest" => in_manifest = true,
                    b"spine" => in_spine = true,
                    b"title" => current_field = Some("title"),
                    b"creator" => current_field = Some("creator"),
                    b"publisher" => current_field = Some("publisher"),
                    b"language" => current_field = Some("language"),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"manifest" => in_manifest = false,
                b"spine" => in_spine = false,
                _ => current_field = None,
            },
            Ok(Event::Empty(e)) => {
                if in_manifest && e.local_name().as_ref() == b"item" {
                    if let (Some(id), Some(href)) =
                        (get_attr(&e, b"id"), get_attr(&e, b"href"))
                    {
                        let full = if opf_dir.is_empty() {
                            href
                        } else {
                            format!("{}/{}", opf_dir, href)
                        };
                        manifest.insert(id, full);
                    }
                } else if in_spine && e.local_name().as_ref() == b"itemref" {
                    if let Some(idref) = get_attr(&e, b"idref") {
                        spine_idrefs.push(idref);
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if let Some(field) = current_field {
                    if let Ok(text) = e.unescape() {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            parts.push(format!("[EPUB:{}] {}", field, text));
                        }
                    }
                    current_field = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let metadata = if parts.is_empty() {
        vec![]
    } else {
        vec![IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: parts.join(" "),
        }]
    };

    let spine_hrefs = spine_idrefs
        .into_iter()
        .filter_map(|id| manifest.get(&id).cloned())
        .collect();

    (metadata, spine_hrefs)
}

// ── XHTML content ─────────────────────────────────────────────────────────────

/// Block elements whose closing tag triggers a line flush.
const BLOCK_ELEMENTS: &[&[u8]] = &[
    b"h1", b"h2", b"h3", b"h4", b"h5", b"h6",
    b"p", b"li", b"dt", b"dd",
    b"td", b"th",
    b"pre", b"blockquote", b"figcaption",
];

/// Elements whose content is skipped entirely (invisible to users).
const SKIP_ELEMENTS: &[&[u8]] = &[b"script", b"style", b"head"];

/// Walk XHTML and return non-empty paragraph strings.
fn extract_xhtml_text(xml: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut skip_depth: usize = 0;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if SKIP_ELEMENTS.contains(&e.local_name().as_ref()) {
                    skip_depth += 1;
                }
            }
            Ok(Event::End(e)) => {
                let local = e.local_name();
                if SKIP_ELEMENTS.contains(&local.as_ref()) {
                    skip_depth = skip_depth.saturating_sub(1);
                } else if skip_depth == 0 && BLOCK_ELEMENTS.contains(&local.as_ref()) {
                    let text = current.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !text.is_empty() {
                        paragraphs.push(text);
                    }
                    current.clear();
                }
            }
            Ok(Event::Text(e)) => {
                if skip_depth == 0 {
                    if let Ok(text) = e.unescape() {
                        current.push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    paragraphs
}

// ── Utility ───────────────────────────────────────────────────────────────────

fn get_attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    e.attributes()
        .filter_map(|a| a.ok())
        .find(|a| a.key.as_ref() == name)
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accepts() {
        assert!(accepts(Path::new("book.epub")));
        assert!(accepts(Path::new("BOOK.EPUB")));
        assert!(!accepts(Path::new("book.pdf")));
        assert!(!accepts(Path::new("book.mobi")));
        assert!(!accepts(Path::new("book.txt")));
    }

    #[test]
    fn test_find_opf_path() {
        let xml = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf"
              media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

        let path = find_opf_path(xml).unwrap();
        assert_eq!(path, "OEBPS/content.opf");
    }

    #[test]
    fn test_parse_opf_metadata() {
        let xml = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf"
         xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata>
    <dc:title>The Great Novel</dc:title>
    <dc:creator>Jane Author</dc:creator>
    <dc:publisher>Big Press</dc:publisher>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="ch1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
  </spine>
</package>"#;

        let (meta, hrefs) = parse_opf(xml, "OEBPS");

        assert_eq!(meta.len(), 1, "expected one consolidated metadata line");
        let m = &meta[0];
        assert_eq!(m.line_number, LINE_METADATA);
        assert!(m.content.contains("[EPUB:title] The Great Novel"), "content: {}", m.content);
        assert!(m.content.contains("[EPUB:creator] Jane Author"), "content: {}", m.content);
        assert!(m.content.contains("[EPUB:publisher] Big Press"), "content: {}", m.content);
        assert!(m.content.contains("[EPUB:language] en"), "content: {}", m.content);

        assert_eq!(hrefs, vec!["OEBPS/chapter1.xhtml"]);
    }

    #[test]
    fn test_extract_xhtml_text() {
        let xhtml = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Chapter 1</title><style>body{}</style></head>
<body>
  <h1>Chapter One</h1>
  <p>The story begins <em>here</em> and continues.</p>
  <p>Second paragraph.</p>
  <p>  </p>
  <script>var x = 1;</script>
  <ul><li>List item one</li><li>List item two</li></ul>
</body>
</html>"#;

        let paras = extract_xhtml_text(xhtml);

        assert!(paras.contains(&"Chapter One".to_string()));
        assert!(paras.iter().any(|s| s.contains("story begins") && s.contains("continues")));
        assert!(paras.contains(&"Second paragraph.".to_string()));
        assert!(paras.contains(&"List item one".to_string()));
        assert!(paras.contains(&"List item two".to_string()));

        // Style and script content must not appear
        assert!(!paras.iter().any(|s| s.contains("body{}")));
        assert!(!paras.iter().any(|s| s.contains("var x")));
        // Empty paragraph skipped
        assert!(!paras.contains(&"".to_string()));
    }

    #[test]
    fn test_spine_ordering() {
        let xml = r#"<package xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata><dc:title>Test</dc:title></metadata>
  <manifest>
    <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
    <item id="b" href="b.xhtml" media-type="application/xhtml+xml"/>
    <item id="c" href="c.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="b"/>
    <itemref idref="a"/>
    <itemref idref="c"/>
  </spine>
</package>"#;

        let (_, hrefs) = parse_opf(xml, "");
        assert_eq!(hrefs, vec!["b.xhtml", "a.xhtml", "c.xhtml"]);
    }

    #[test]
    fn test_find_opf_path_missing_rootfile_returns_error() {
        let xml = r#"<?xml version="1.0"?><container version="1.0"></container>"#;
        assert!(find_opf_path(xml).is_err(), "missing rootfile should return error");
    }

    #[test]
    fn test_parse_opf_no_metadata_returns_empty_meta_vec() {
        let xml = r#"<package><metadata></metadata><manifest></manifest><spine></spine></package>"#;
        let (meta, hrefs) = parse_opf(xml, "");
        assert!(meta.is_empty(), "no DC fields → no metadata line");
        assert!(hrefs.is_empty());
    }

    // ── extract() — full EPUB round-trip ─────────────────────────────────────

    /// Build a minimal but valid EPUB zip into `buf`.
    fn build_minimal_epub() -> Vec<u8> {
        use std::io::{Cursor, Write as _};
        let mut buf = Vec::new();
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();

        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#).unwrap();

        zip.start_file("content.opf", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata>
    <dc:title>Test Book</dc:title>
    <dc:creator>Test Author</dc:creator>
  </metadata>
  <manifest>
    <item id="ch1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
  </spine>
</package>"#).unwrap();

        zip.start_file("chapter1.xhtml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<body>
  <h1>Chapter One</h1>
  <p>This is the first paragraph of the test book.</p>
</body>
</html>"#).unwrap();

        zip.finish().unwrap();
        buf
    }

    #[test]
    fn test_extract_from_minimal_epub_file() {
        use find_extract_types::ExtractorConfig;
        let epub_bytes = build_minimal_epub();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.epub");
        std::fs::write(&path, &epub_bytes).unwrap();

        let lines = extract(&path, &ExtractorConfig::default()).unwrap();

        // Metadata line
        let meta = lines.iter().find(|l| l.line_number == LINE_METADATA).expect("metadata line");
        assert!(meta.content.contains("Test Book"), "metadata should contain title");
        assert!(meta.content.contains("Test Author"), "metadata should contain creator");

        // Content lines
        assert!(lines.iter().any(|l| l.line_number >= LINE_CONTENT_START && l.content.contains("Chapter One")));
        assert!(lines.iter().any(|l| l.line_number >= LINE_CONTENT_START && l.content.contains("first paragraph")));
    }

    #[test]
    fn test_extract_from_bytes_round_trip() {
        use find_extract_types::ExtractorConfig;
        let epub_bytes = build_minimal_epub();
        let lines = extract_from_bytes(&epub_bytes, "test.epub", &ExtractorConfig::default()).unwrap();
        assert!(lines.iter().any(|l| l.content.contains("Test Book")), "bytes extraction should find title");
        assert!(lines.iter().any(|l| l.content.contains("first paragraph")));
    }

    #[test]
    fn test_extract_from_bytes_empty_returns_error() {
        use find_extract_types::ExtractorConfig;
        let result = extract_from_bytes(b"", "empty.epub", &ExtractorConfig::default());
        assert!(result.is_err(), "empty bytes should fail to parse as EPUB");
    }
}
