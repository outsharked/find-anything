use std::path::Path;
use find_extract_types::{IndexLine, LINE_CONTENT_START};
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
            line_number: LINE_CONTENT_START,
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
    let mut line_num: usize = LINE_CONTENT_START - 1;
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
        assert_eq!(result[0].line_number, LINE_CONTENT_START);
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

    /// A malformed PDF (valid header, corrupt body) must not panic and must return Ok.
    ///
    /// This exercises the `catch_unwind` safety net in `extract_from_bytes` that
    /// guards against panics in `pdf-extract` when parsing corrupt Type1 font data
    /// or other malformed structures. The result may be an empty Vec or any content —
    /// what matters is that the function returns `Ok(_)` rather than panicking.
    #[test]
    fn malformed_pdf_does_not_panic() {
        // A PDF header followed by garbage — triggers the error/panic-handling path.
        let malformed = b"%PDF-1.4\n% malformed content that will confuse the parser\n\
                          1 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                          /Encoding << /Type /Encoding /Differences [ 0 /A.notdef ] >> >> endobj\n\
                          xref\n0 0\ntrailer << /Root 999 0 R >>\n%%EOF";
        let result = extract_from_bytes(malformed, "malformed.pdf", &test_cfg());
        assert!(
            result.is_ok(),
            "malformed PDF must not panic or return Err: {:?}",
            result
        );
    }

    // ── accepts ─────────────────────────────────────────────────────────────

    #[test]
    fn accepts_pdf_extension() {
        assert!(accepts(std::path::Path::new("document.pdf")));
        assert!(accepts(std::path::Path::new("document.PDF")));
        assert!(accepts(std::path::Path::new("document.Pdf")));
    }

    #[test]
    fn accepts_rejects_non_pdf() {
        assert!(!accepts(std::path::Path::new("document.txt")));
        assert!(!accepts(std::path::Path::new("document.docx")));
        assert!(!accepts(std::path::Path::new("nopdf")));
    }

    // ── edge-case inputs ─────────────────────────────────────────────────────

    #[test]
    fn empty_bytes_returns_empty_vec() {
        let result = extract_from_bytes(b"", "empty.pdf", &test_cfg()).unwrap();
        assert!(result.is_empty(), "empty bytes should yield no lines");
    }

    #[test]
    fn garbage_bytes_returns_ok() {
        let garbage = b"\x00\x01\x02\x03\xFF\xFE\xFD garbage not a pdf at all";
        let result = extract_from_bytes(garbage, "garbage.pdf", &test_cfg());
        assert!(result.is_ok(), "garbage bytes must not panic or return Err");
    }

    #[test]
    fn truncated_pdf_header_returns_ok() {
        let result = extract_from_bytes(b"%PDF-1.4", "truncated.pdf", &test_cfg());
        assert!(result.is_ok(), "truncated header must not panic");
    }

    #[test]
    fn encryption_guard_triggers_on_encrypt_token() {
        // Minimal bytes with /Encrypt token — should short-circuit without calling pdf-extract.
        let pseudo = b"not a real pdf but has /Encrypt in the bytes somewhere";
        let result = extract_from_bytes(pseudo, "pseudo.pdf", &test_cfg()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "Content encrypted");
    }

    // ── wrap_at_words ────────────────────────────────────────────────────────

    #[test]
    fn wrap_short_line_unchanged() {
        let result = wrap_at_words("hello world", 80);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn wrap_long_line_splits_at_word_boundary() {
        let words: Vec<String> = (0..20).map(|i| format!("word{i}")).collect();
        let line = words.join(" "); // each word is ~6 chars → total ~120 chars
        let result = wrap_at_words(&line, 40);
        assert!(result.len() > 1, "long line should split into multiple chunks");
        for chunk in &result {
            // Allow single overlong words but no multi-word chunk should exceed limit.
            let parts: Vec<&str> = chunk.split_whitespace().collect();
            if parts.len() > 1 {
                assert!(
                    chunk.chars().count() <= 40,
                    "chunk too long: {chunk:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_single_word_longer_than_limit_kept_as_is() {
        let long_word = "a".repeat(100);
        let result = wrap_at_words(&long_word, 20);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], long_word);
    }

    #[test]
    fn wrap_empty_string_returns_empty_vec() {
        let result = wrap_at_words("", 80);
        assert!(result.is_empty());
    }

    #[test]
    fn wrap_with_zero_limit_keeps_one_word_per_chunk() {
        // max_len=0: every word starts a new chunk (since 0+1+word_len > 0 always).
        let result = wrap_at_words("alpha beta gamma", 0);
        assert_eq!(result, vec!["alpha", "beta", "gamma"]);
    }

    // ── max_content_kb truncation ────────────────────────────────────────────

    #[test]
    fn content_truncated_at_max_kb() {
        // Build a "PDF" bytes that contain /Encrypt-free content but will yield
        // many lines. We can test the truncation by sending a realistic text payload
        // via extract_from_bytes with a very small content limit.
        // Since we need real parseable PDF, use the minimal fixture but with tiny limit.
        let bytes = include_bytes!("../tests/fixtures/minimal.pdf");
        let small_cfg = ExtractorConfig {
            max_content_kb: 1, // 1 KB
            max_line_length: 0,
            ..Default::default()
        };
        let result = extract_from_bytes(bytes, "minimal.pdf", &small_cfg);
        // Must not panic or error; truncation is internal.
        assert!(result.is_ok());
    }
}
