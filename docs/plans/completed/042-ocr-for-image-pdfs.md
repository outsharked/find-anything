# OCR for Image-Only PDFs

## Status: Superseded

This plan has been superseded. The approach of building OCR into find-anything's
server/client pipeline was rejected for the following reasons:

1. **Async result delivery**: The indexing pipeline is intentionally async (client
   uploads → server processes in background). There is no clean way to send OCR
   results back to the client for persistence without adding significant complexity
   (polling, callbacks, sidecar download endpoints).

2. **Index persistence**: OCR text stored only in the server index would be lost on
   index rebuild. Text should live with the source files, not only in the index.

3. **Separation of concerns**: OCR and archive normalisation are preprocessing steps,
   not indexing steps. A dedicated tool is cleaner.

## New approach: `archive-assistant`

OCR and archive normalisation are handled by a standalone Rust CLI tool in a separate
repository: `../archive-assistant`.

That tool:
- Traverses a directory tree
- OCRs image-only PDFs in-place using `ocrmypdf`, setting mtime+60s to trigger re-indexing
- Converts non-ZIP archives (7z, tar, tar.gz, etc.) to ZIP for better find-anything compatibility
- Processes PDFs inside archives too
- Tracks processed archives in a local SQLite database for idempotency

find-anything requires no changes. After running `archive-assistant`, a normal
find-scan picks up the modified files (mtime changed) and indexes the new text content.
