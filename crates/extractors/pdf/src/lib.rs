use std::path::Path;
use find_extract_types::IndexLine;
use find_extract_types::ExtractorConfig;
use tracing::{warn, error};

/// Extract text content from PDF files.
///
/// Uses pdf-extract library. Handles malformed PDFs gracefully by catching panics.
pub fn extract(path: &Path, cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let name = path.display().to_string();
    let bytes = std::fs::read(path)?;
    extract_from_bytes(&bytes, &name, cfg)
}

/// Extract text content from PDF bytes.
///
/// Used by the archive extractor to process PDF members without writing to disk.
///
/// Lines are numbered sequentially (1, 2, 3, ...) — empty lines in the raw text
/// are skipped entirely so there are no gaps in the line number sequence. This
/// ensures that context retrieval (±2 lines) always returns the expected window.
///
/// Lines longer than `cfg.max_line_length` characters are split at word boundaries
/// into multiple indexed lines, which makes long PDF paragraphs searchable and
/// provides meaningful surrounding context.
pub fn extract_from_bytes(bytes: &[u8], name: &str, cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    // Pre-check for encryption before calling pdf-extract.
    // We scan the raw bytes for the PDF name token "/Encrypt" rather than loading
    // the document with lopdf, because lopdf::Document::load_mem() itself triggers
    // "corrupt deflate stream" warnings when it encounters encrypted content streams
    // during structural parsing.  The /Encrypt name appears verbatim in the file
    // structure of every encrypted PDF and is not present in unencrypted ones.
    if bytes.windows(8).any(|w| w == b"/Encrypt") {
        warn!("PDF is password-protected, content not indexed: {name}");
        return Ok(vec![IndexLine {
            archive_path: None,
            line_number: 1,
            content: "Content encrypted".to_string(),
        }]);
    }

    // pdf-extract can panic on malformed PDFs; catch_unwind turns that into
    // a recoverable error so the scan can continue with other files.
    //
    // Temporarily install a custom panic hook so the file path appears in
    // the panic output (the default hook prints no context about which file
    // triggered the panic).
    let name_for_hook = name.to_string();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error!("PDF extraction panicked for {name_for_hook}: {info}");
    }));
    let bytes_clone = bytes.to_vec();
    let result = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes_clone));
    std::panic::set_hook(prev_hook);

    let text = match result {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            warn!("PDF extraction error for {name}: {e}");
            return Ok(vec![]);
        }
        Err(_) => return Ok(vec![]),
    };

    let mut lines = Vec::new();
    let mut line_num: usize = 0;
    let max_content_bytes = cfg.max_content_kb * 1024;
    let mut total_content_bytes: usize = 0;

    'outer: for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let chunks = if cfg.max_line_length > 0 && trimmed.chars().count() > cfg.max_line_length {
            wrap_at_words(trimmed, cfg.max_line_length)
        } else {
            vec![trimmed.to_string()]
        };

        for chunk in chunks {
            total_content_bytes += chunk.len();
            if total_content_bytes > max_content_bytes {
                break 'outer;
            }
            line_num += 1;
            lines.push(IndexLine {
                archive_path: None,
                line_number: line_num,
                content: chunk,
            });
        }
    }
    Ok(lines)
}

/// Split `s` at word boundaries into chunks of at most `max_len` characters each.
///
/// Uses whitespace as word boundaries. A single word longer than `max_len` chars
/// is kept as-is (no hard break mid-word). Preserves all non-whitespace content.
fn wrap_at_words(s: &str, max_len: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;

    for word in s.split_whitespace() {
        let word_len = word.chars().count();
        if current_len == 0 {
            // First word on this chunk — always include even if over limit
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= max_len {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            result.push(current.clone());
            current = word.to_string();
            current_len = word_len;
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// Check if a file is a PDF based on extension.
pub fn accepts(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use find_extract_types::ExtractorConfig;

    fn test_cfg() -> ExtractorConfig {
        ExtractorConfig {
            max_content_kb: 10 * 1024,
            max_line_length: 0,
            ..Default::default()
        }
    }

    /// A password-protected PDF (AES-256, /Encrypt in trailer) must trigger the
    /// encryption guard and return exactly one "Content encrypted" line.
    /// pdf-extract is never called — the guard short-circuits on the /Encrypt token.
    #[test]
    fn encrypted_pdf_returns_single_content_encrypted_line() {
        let bytes = include_bytes!("../tests/fixtures/encrypted.pdf");
        let result = extract_from_bytes(bytes, "encrypted.pdf", &test_cfg()).unwrap();
        assert_eq!(result.len(), 1, "expected exactly one line for encrypted PDF");
        assert_eq!(result[0].content, "Content encrypted");
        assert_eq!(result[0].line_number, 1);
        assert!(result[0].archive_path.is_none());
    }

    /// An ordinary unencrypted PDF must never produce a "Content encrypted" line.
    #[test]
    fn unencrypted_pdf_does_not_produce_content_encrypted_line() {
        let bytes = include_bytes!("../tests/fixtures/minimal.pdf");
        let result = extract_from_bytes(bytes, "minimal.pdf", &test_cfg()).unwrap();
        assert!(
            result.iter().all(|l| l.content != "Content encrypted"),
            "unencrypted PDF must not produce 'Content encrypted'"
        );
    }
}
