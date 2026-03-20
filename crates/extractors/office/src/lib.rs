use std::io::Read;
use std::path::Path;

use find_extract_types::{IndexLine, LINE_METADATA, LINE_CONTENT_START};
use find_extract_types::ExtractorConfig;
use quick_xml::events::Event;

/// Accept Office document formats.
pub fn accepts(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "docx" | "xlsx" | "xls" | "xlsm" | "pptx"
    )
}

/// Extract text from Office document bytes.
///
/// Used by `find-extract-dispatch` for archive members. Writes to a temp file
/// and delegates to `extract` (which needs a real path for some formats).
pub fn extract_from_bytes(bytes: &[u8], name: &str, cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    use std::io::Write;
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("docx");
    let mut tmp = tempfile::Builder::new()
        .suffix(&format!(".{}", ext))
        .tempfile()?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    extract(tmp.path(), cfg)
}

/// Extract text from an Office document.
///
/// - DOCX: paragraphs from word/document.xml + metadata from docProps/core.xml
/// - XLSX/XLS/XLSM: rows from all sheets (via calamine)
/// - PPTX: text runs from each slide, grouped by paragraph
pub fn extract(path: &Path, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "docx" => extract_docx(path),
        "xlsx" | "xls" | "xlsm" => extract_xlsx(path),
        "pptx" => extract_pptx(path),
        _ => Ok(vec![]),
    }
}

// ── DOCX ─────────────────────────────────────────────────────────────────────

fn extract_docx(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut lines = Vec::new();

    // Metadata from docProps/core.xml — consolidated into LINE_METADATA.
    {
        if let Ok(mut entry) = archive.by_name("docProps/core.xml") {
            let mut xml = String::new();
            entry.read_to_string(&mut xml)?;
            if let Some(meta) = parse_docx_metadata(&xml) {
                lines.push(meta);
            }
        }
    }

    // Content from word/document.xml — starts at LINE_CONTENT_START.
    {
        if let Ok(mut entry) = archive.by_name("word/document.xml") {
            let mut xml = String::new();
            entry.read_to_string(&mut xml)?;
            let paragraphs = parse_docx_paragraphs(&xml);
            for (i, text) in paragraphs.into_iter().enumerate() {
                lines.push(IndexLine {
                    archive_path: None,
                    line_number: i + LINE_CONTENT_START,
                    content: text,
                });
            }
        }
    }

    Ok(lines)
}

/// Extract dc:title and dc:creator from docProps/core.xml, concatenated into a
/// single IndexLine at LINE_METADATA.
fn parse_docx_metadata(xml: &str) -> Option<IndexLine> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut parts = Vec::new();
    let mut current_field: Option<&'static str> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                current_field = match e.name().as_ref() {
                    b"dc:title" => Some("title"),
                    b"dc:creator" => Some("author"),
                    _ => None,
                };
            }
            Ok(Event::Text(e)) => {
                if let Some(field) = current_field {
                    if let Ok(text) = e.unescape() {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            parts.push(format!("[DOCX:{}] {}", field, text));
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                current_field = None;
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    if parts.is_empty() {
        return None;
    }

    Some(IndexLine {
        archive_path: None,
        line_number: LINE_METADATA,
        content: parts.join(" "),
    })
}

/// Collect non-empty paragraphs from word/document.xml.
fn parse_docx_paragraphs(xml: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut paragraphs = Vec::new();
    let mut current_para = String::new();
    let mut in_t = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"w:t" => in_t = true,
                b"w:p" => current_para.clear(),
                _ => {}
            },
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:t" => in_t = false,
                b"w:p" => {
                    let text = current_para.trim().to_string();
                    if !text.is_empty() {
                        paragraphs.push(text);
                    }
                    current_para.clear();
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if in_t {
                    if let Ok(text) = e.unescape() {
                        current_para.push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    paragraphs
}

// ── XLSX / XLS / XLSM ────────────────────────────────────────────────────────

fn extract_xlsx(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    use calamine::{open_workbook_auto, Data, Reader};

    let mut wb = open_workbook_auto(path)?;
    let mut lines = Vec::new();

    let sheet_names = wb.sheet_names().to_vec();

    // All sheet names concatenated into the metadata slot.
    if !sheet_names.is_empty() {
        let meta = sheet_names.iter()
            .map(|n| format!("[XLSX:sheet] {}", n))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: meta,
        });
    }

    let mut content_line = LINE_CONTENT_START - 1;

    for sheet_name in &sheet_names {
        if let Ok(range) = wb.worksheet_range(sheet_name) {
            for row in range.rows() {
                let cells: Vec<String> = row
                    .iter()
                    .filter_map(|cell| match cell {
                        Data::Empty => None,
                        Data::String(s) if s.trim().is_empty() => None,
                        other => {
                            let s = other.to_string();
                            if s.is_empty() {
                                None
                            } else {
                                Some(s)
                            }
                        }
                    })
                    .collect();

                if !cells.is_empty() {
                    content_line += 1;
                    lines.push(IndexLine {
                        archive_path: None,
                        line_number: content_line,
                        content: cells.join("\t"),
                    });
                }
            }
        }
    }

    Ok(lines)
}

// ── PPTX ─────────────────────────────────────────────────────────────────────

fn extract_pptx(path: &Path) -> anyhow::Result<Vec<IndexLine>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut lines = Vec::new();

    // Collect slide file names first (no entry borrow held)
    let mut slide_names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                slide_names.push(name);
            }
        }
    }

    // Sort numerically: slide1.xml, slide2.xml, …
    slide_names.sort_by_key(|n| {
        n.strip_prefix("ppt/slides/slide")
            .and_then(|s| s.strip_suffix(".xml"))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
    });

    // All slide labels concatenated into the metadata slot.
    if !slide_names.is_empty() {
        let meta = (1..=slide_names.len())
            .map(|i| format!("[PPTX:slide] {}", i))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: meta,
        });
    }

    let mut content_line = LINE_CONTENT_START - 1;

    for slide_name in &slide_names {
        let xml = {
            let mut entry = archive.by_name(slide_name)?;
            let mut s = String::new();
            entry.read_to_string(&mut s)?;
            s
        };

        for text in parse_pptx_paragraphs(&xml) {
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

/// Collect non-empty paragraphs from a PPTX slide XML.
fn parse_pptx_paragraphs(xml: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut paragraphs = Vec::new();
    let mut current_para = String::new();
    let mut in_t = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == b"a:t" {
                    in_t = true;
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"a:t" => in_t = false,
                b"a:p" => {
                    let text = current_para.trim().to_string();
                    if !text.is_empty() {
                        paragraphs.push(text);
                    }
                    current_para.clear();
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if in_t {
                    if let Ok(text) = e.unescape() {
                        current_para.push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    paragraphs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    // ── ZIP builder helpers ───────────────────────────────────────────────────

    fn make_docx(document_xml: &str, core_xml: Option<&str>) -> Vec<u8> {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();
        if let Some(core) = core_xml {
            zip.start_file("docProps/core.xml", opts).unwrap();
            zip.write_all(core.as_bytes()).unwrap();
        }
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    fn make_pptx(slides: &[&str]) -> Vec<u8> {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();
        for (i, xml) in slides.iter().enumerate() {
            zip.start_file(format!("ppt/slides/slide{}.xml", i + 1), opts).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn make_minimal_xlsx() -> Vec<u8> {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();

        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#).unwrap();

        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#).unwrap();

        zip.start_file("xl/workbook.xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#).unwrap();

        zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#).unwrap();

        zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Hello</t></is></c>
      <c r="B1" t="inlineStr"><is><t>World</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="inlineStr"><is><t>Foo</t></is></c>
    </row>
  </sheetData>
</worksheet>"#).unwrap();

        zip.finish().unwrap().into_inner()
    }

    fn write_tmp(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_accepts() {
        assert!(accepts(Path::new("report.docx")));
        assert!(accepts(Path::new("data.xlsx")));
        assert!(accepts(Path::new("data.xls")));
        assert!(accepts(Path::new("data.xlsm")));
        assert!(accepts(Path::new("deck.pptx")));
        assert!(accepts(Path::new("REPORT.DOCX")));
        assert!(!accepts(Path::new("notes.odt")));
        assert!(!accepts(Path::new("data.csv")));
        assert!(!accepts(Path::new("index.html")));
    }

    #[test]
    fn test_parse_docx_metadata() {
        let xml = r#"<?xml version="1.0"?>
<cp:coreProperties xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>My Document</dc:title>
  <dc:creator>Jane Smith</dc:creator>
</cp:coreProperties>"#;

        let meta = parse_docx_metadata(xml).expect("expected metadata");
        assert_eq!(meta.line_number, LINE_METADATA);
        assert!(meta.content.contains("[DOCX:title] My Document"), "content: {}", meta.content);
        assert!(meta.content.contains("[DOCX:author] Jane Smith"), "content: {}", meta.content);
    }

    #[test]
    fn test_parse_docx_paragraphs() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>First paragraph</w:t></w:r></w:p>
    <w:p><w:r><w:t>Second </w:t></w:r><w:r><w:t>paragraph</w:t></w:r></w:p>
    <w:p><w:r><w:t>   </w:t></w:r></w:p>
    <w:p><w:r><w:t>Third paragraph</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let paras = parse_docx_paragraphs(xml);
        assert_eq!(paras.len(), 3); // blank paragraph skipped
        assert_eq!(paras[0], "First paragraph");
        assert_eq!(paras[1], "Second paragraph");
        assert_eq!(paras[2], "Third paragraph");
    }

    #[test]
    fn test_parse_pptx_paragraphs() {
        let xml = r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:txBody>
          <a:p><a:r><a:t>Slide title</a:t></a:r></a:p>
          <a:p><a:r><a:t>Bullet </a:t></a:r><a:r><a:t>point</a:t></a:r></a:p>
          <a:p><a:r><a:t></a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let paras = parse_pptx_paragraphs(xml);
        assert_eq!(paras.len(), 2); // empty paragraph skipped
        assert_eq!(paras[0], "Slide title");
        assert_eq!(paras[1], "Bullet point");
    }

    #[test]
    fn test_docx_line_numbers() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Alpha</w:t></w:r></w:p>
    <w:p><w:r><w:t>Beta</w:t></w:r></w:p>
    <w:p><w:r><w:t>Gamma</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        let paras = parse_docx_paragraphs(xml);
        // Verify we can build IndexLines with sequential numbers
        for (i, text) in paras.iter().enumerate() {
            assert_eq!(*text, ["Alpha", "Beta", "Gamma"][i]);
        }
    }

    // ── extract() dispatch ────────────────────────────────────────────────────

    #[test]
    fn extract_unknown_extension_returns_empty() {
        let cfg = ExtractorConfig::default();
        let f = write_tmp(b"irrelevant", ".odt");
        let lines = extract(f.path(), &cfg).unwrap();
        assert!(lines.is_empty());
    }

    // ── DOCX extraction ───────────────────────────────────────────────────────

    #[test]
    fn docx_extracts_paragraphs() {
        let cfg = ExtractorConfig::default();
        let doc_xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Hello from DOCX</w:t></w:r></w:p>
    <w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let bytes = make_docx(doc_xml, None);
        let f = write_tmp(&bytes, ".docx");
        let lines = extract(f.path(), &cfg).unwrap();
        let contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();
        assert!(contents.contains(&"Hello from DOCX"), "lines: {lines:?}");
        assert!(contents.contains(&"Second paragraph"), "lines: {lines:?}");
    }

    #[test]
    fn docx_extracts_metadata_when_core_xml_present() {
        let cfg = ExtractorConfig::default();
        let doc_xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p></w:body>
</w:document>"#;
        let core_xml = r#"<?xml version="1.0"?>
<cp:coreProperties xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>My Test Doc</dc:title>
  <dc:creator>Test Author</dc:creator>
</cp:coreProperties>"#;
        let bytes = make_docx(doc_xml, Some(core_xml));
        let f = write_tmp(&bytes, ".docx");
        let lines = extract(f.path(), &cfg).unwrap();
        let meta = lines.iter().find(|l| l.line_number == LINE_METADATA)
            .expect("expected metadata line");
        assert!(meta.content.contains("[DOCX:title] My Test Doc"), "meta: {}", meta.content);
        assert!(meta.content.contains("[DOCX:author] Test Author"), "meta: {}", meta.content);
    }

    #[test]
    fn docx_empty_document_returns_empty() {
        let cfg = ExtractorConfig::default();
        let doc_xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body></w:body>
</w:document>"#;
        let bytes = make_docx(doc_xml, None);
        let f = write_tmp(&bytes, ".docx");
        let lines = extract(f.path(), &cfg).unwrap();
        assert!(lines.is_empty(), "empty document should yield no lines, got: {lines:?}");
    }

    #[test]
    fn docx_corrupt_zip_returns_error() {
        let cfg = ExtractorConfig::default();
        let f = write_tmp(b"not a zip", ".docx");
        let result = extract(f.path(), &cfg);
        assert!(result.is_err(), "corrupt DOCX should return Err");
    }

    // ── PPTX extraction ───────────────────────────────────────────────────────

    #[test]
    fn pptx_extracts_slide_text() {
        let cfg = ExtractorConfig::default();
        let slide1 = r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:sp><p:txBody>
      <a:p><a:r><a:t>Slide One Title</a:t></a:r></a:p>
    </p:txBody></p:sp>
  </p:spTree></p:cSld>
</p:sld>"#;
        let bytes = make_pptx(&[slide1]);
        let f = write_tmp(&bytes, ".pptx");
        let lines = extract(f.path(), &cfg).unwrap();
        assert!(lines.iter().any(|l| l.content.contains("Slide One Title")), "lines: {lines:?}");
    }

    #[test]
    fn pptx_multiple_slides_all_extracted() {
        let cfg = ExtractorConfig::default();
        let slide1 = r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree><p:sp><p:txBody>
    <a:p><a:r><a:t>First slide</a:t></a:r></a:p>
  </p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
        let slide2 = r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree><p:sp><p:txBody>
    <a:p><a:r><a:t>Second slide</a:t></a:r></a:p>
  </p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
        let bytes = make_pptx(&[slide1, slide2]);
        let f = write_tmp(&bytes, ".pptx");
        let lines = extract(f.path(), &cfg).unwrap();
        let meta = lines.iter().find(|l| l.line_number == LINE_METADATA)
            .expect("expected metadata line with slide count");
        assert!(meta.content.contains("[PPTX:slide] 1"), "meta: {}", meta.content);
        assert!(meta.content.contains("[PPTX:slide] 2"), "meta: {}", meta.content);
        assert!(lines.iter().any(|l| l.content.contains("First slide")), "lines: {lines:?}");
        assert!(lines.iter().any(|l| l.content.contains("Second slide")), "lines: {lines:?}");
    }

    #[test]
    fn pptx_empty_zip_returns_empty() {
        let cfg = ExtractorConfig::default();
        // A valid ZIP with no slides
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let zip = zip::ZipWriter::new(cursor);
        let bytes = zip.finish().unwrap().into_inner();
        let f = write_tmp(&bytes, ".pptx");
        let lines = extract(f.path(), &cfg).unwrap();
        assert!(lines.is_empty(), "empty PPTX should yield no lines, got: {lines:?}");
    }

    // ── XLSX extraction ───────────────────────────────────────────────────────

    #[test]
    fn xlsx_extracts_sheet_names_and_cell_content() {
        let cfg = ExtractorConfig::default();
        let bytes = make_minimal_xlsx();
        let f = write_tmp(&bytes, ".xlsx");
        let lines = extract(f.path(), &cfg).unwrap();
        let meta = lines.iter().find(|l| l.line_number == LINE_METADATA)
            .expect("expected metadata line");
        assert!(meta.content.contains("[XLSX:sheet] Sheet1"), "meta: {}", meta.content);
        let all_content: String = lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join(" ");
        assert!(all_content.contains("Hello"), "content: {all_content}");
    }

    #[test]
    fn xlsx_corrupt_returns_error() {
        let cfg = ExtractorConfig::default();
        let f = write_tmp(b"not an xlsx", ".xlsx");
        let result = extract(f.path(), &cfg);
        assert!(result.is_err(), "corrupt XLSX should return Err");
    }

    // ── extract_from_bytes() ─────────────────────────────────────────────────

    #[test]
    fn extract_from_bytes_docx() {
        let cfg = ExtractorConfig::default();
        let doc_xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>from bytes</w:t></w:r></w:p></w:body>
</w:document>"#;
        let bytes = make_docx(doc_xml, None);
        let lines = extract_from_bytes(&bytes, "doc.docx", &cfg).unwrap();
        assert!(lines.iter().any(|l| l.content.contains("from bytes")), "lines: {lines:?}");
    }

    #[test]
    fn extract_from_bytes_corrupt_returns_error() {
        let cfg = ExtractorConfig::default();
        let result = extract_from_bytes(b"garbage", "doc.docx", &cfg);
        assert!(result.is_err());
    }
}
