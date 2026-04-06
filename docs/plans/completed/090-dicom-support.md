# 090 — DICOM Support

## Overview

Add first-class support for DICOM medical image files (`.dcm`, `.dicom`): metadata
extraction for search indexing, and inline PNG preview via a dedicated converter
binary so `find-server`'s binary size is unaffected.

---

## Design Decisions

### Two-binary approach

DICOM support splits across two new binaries that mirror the existing extractor
pattern:

| Binary | Crate | Purpose |
|--------|-------|---------|
| `find-extract-dicom` | `crates/extractors/dicom` | Metadata extraction — indexes patient info, modality, dates, dimensions. **No pixel deps.** Pure Rust. |
| `find-preview-dicom` | `crates/preview-dicom` | On-demand PNG conversion — spawned by the server per preview request, writes PNG to stdout. **All heavy deps here.** |

`find-server` spawns `find-preview-dicom` the same way upload delegation spawns
`find-scan` — shell out, capture stdout, return the bytes. The server binary stays
the same size.

### JPEG2000 as a Cargo feature flag

`find-preview-dicom` gets an optional feature `jpeg2000`:

```toml
[features]
default = []
jpeg2000 = ["dicom-pixeldata/openjp2"]
```

- Without the flag: pure Rust, handles uncompressed + JPEG + RLE.
- With `--features jpeg2000`: links OpenJPEG (C library). The CI/release build
  enables it; local dev without a C toolchain can omit it.
- The `find-server` binary is **never** affected regardless of whether the feature
  is enabled, since the pixel code lives in a separate crate.

### Windowing strategy

CT/MRI images are 12–16 bit. We apply `VoiLutOption::Normalize` which reads the
embedded Window Center / Window Width DICOM tags and maps them to 8-bit output.
This gives a diagnostically reasonable default image without user interaction.
Multi-frame files (CT series) preview frame 0 only; the metadata extractor records
the total frame count.

### Preview API

A new server route:

```
GET /api/v1/dicom-preview?source=X&path=Y
```

Returns `image/png`. The server:
1. Resolves the file path from the files table (using `source` + `path`).
2. Spawns `find-preview-dicom <abs_path>` with a timeout (default 30 s).
3. Streams the subprocess stdout directly as the response body.
4. On non-zero exit or timeout: returns 422 with a JSON error.

The preview binary writes PNG bytes to stdout and errors to stderr. No temp files.

### UI integration

DICOM files get `kind = "dicom"`. The `FileViewer` already routes on `kind` — add
a `DicomViewer` component that:
- Requests `/api/v1/dicom-preview?…` and renders the result in `<img>`.
- Shows metadata chips (modality, body part, study date, dimensions, frame count)
  in the existing metadata bar.
- Displays a "Cannot preview" placeholder if the route returns 422 (e.g. JPEG2000
  without the feature enabled, or a corrupt file).

---

## Implementation

### Phase 1 — Metadata extraction

1. Create `crates/extractors/dicom/` following the `find-extract-media` pattern.
2. Dependencies: `dicom-object`, `dicom-dictionary-std` (pure Rust, no pixel code).
3. Extract and index:
   - `PatientName` (0010,0010)
   - `StudyDate` (0008,0020), `SeriesDate` (0008,0021)
   - `Modality` (0008,0060) — CT, MRI, X-ray, etc.
   - `BodyPartExamined` (0018,0015)
   - `StudyDescription` (0008,1030), `SeriesDescription` (0008,103E)
   - `InstitutionName` (0008,0080)
   - `Rows` (0028,0010), `Columns` (0028,0011)
   - `NumberOfFrames` (0028,0008)
   - `TransferSyntaxUID` (0002,0010) — stored as metadata, used for preview fallback
4. Register `.dcm` / `.dicom` in `crates/extractors/dispatch/` and kind detection.
5. Add `kind = "dicom"` to `KindOptions` in the web UI.

### Phase 2 — Preview binary

1. Create `crates/preview-dicom/` as a standalone binary crate.
2. Dependencies:
   ```toml
   dicom-object    = "0.9"
   dicom-pixeldata = "0.9"
   image           = { version = "0.25", default-features = false, features = ["png"] }

   [features]
   jpeg2000 = ["dicom-pixeldata/openjp2"]
   ```
3. Binary reads path from `argv[1]`, decodes frame 0 with
   `VoiLutOption::Normalize`, writes PNG to stdout.
4. Exits non-zero on any error, printing to stderr.
5. Add `find-preview-dicom` to the release build and packaging scripts alongside
   the other extractor binaries.

### Phase 3 — Server route

1. Add `GET /api/v1/dicom-preview` route in `crates/server/src/routes/`.
2. Config: `dicom_preview_timeout_secs` in `ServerScanConfig` (default 30).
3. Locate `find-preview-dicom` the same way the server locates `find-scan`
   (sibling binary resolution).
4. Auth: require bearer token (same as all other API routes).

### Phase 4 — Web UI

1. Add `DicomViewer.svelte` component.
2. Register it in `FileViewer.svelte` for `kind === 'dicom'`.
3. Metadata bar shows modality, body part, study date, dimensions, frame count
   sourced from the search result's extracted metadata fields.

---

## DICOM Detection — Extension and Magic Bytes

DICOM files in the wild are often extensionless (the format predates the convention
of using `.dcm`). The dispatch crate must detect them by magic bytes when there is
no extension to match on.

### Magic bytes

Modern DICOM (post-1993) has a fixed signature:

```
offset 0–127:   128-byte preamble (arbitrary content, conventionally zeros)
offset 128–131: ASCII "DICM"
```

Legacy DICOM (pre-1993, rare) has no preamble and no magic marker; it starts
directly with a tag group/element pair. We do not attempt to detect legacy DICOM
without an extension — the false-positive risk on arbitrary binary files is too high.

### Two acceptance functions in `find-extract-dicom`

```rust
/// True if path has a .dcm or .dicom extension (case-insensitive).
pub fn accepts(path: &Path) -> bool { … }

/// True if bytes contain the DICM preamble marker at offset 128.
/// Requires at least 132 bytes.
pub fn accepts_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 132 && &bytes[128..132] == b"DICM"
}
```

### Changes to `dispatch_from_bytes`

Add a DICOM check near the top of the dispatch chain (before HTML/office/text),
using either function:

```rust
// ── DICOM ────────────────────────────────────────────────────────────────────
if find_extract_dicom::accepts(member_path) || find_extract_dicom::accepts_bytes(bytes) {
    match find_extract_dicom::extract_from_bytes(bytes, name, cfg) {
        Ok(lines) => return lines,
        Err(e)    => warn!("DICOM extraction failed for '{}': {}", name, e),
    }
    return vec![];
}
```

This covers extensionless DICOM files arriving as archive members, where full bytes
are always provided.

### Changes to `dispatch_from_path`

Two additions needed:

**1. Add to `claimed_by_specialist`** (so extension-matched files get a full read):

```rust
let claimed_by_specialist = find_extract_pdf::accepts(path)
    || find_extract_dicom::accepts(path)   // ← add
    || find_extract_media::accepts(path)
    …
```

**2. Magic-byte re-read in the sniff branch** (extensionless files):

The sniff buffer is already 512 bytes — enough to cover offset 132. After the sniff
read, check DICOM magic before the text check. If matched, re-open and read the
full file:

```rust
// In the sniff branch, after reading first 512 bytes into `sniff`:
if find_extract_dicom::accepts_bytes(&sniff) {
    // Re-read full file; DICOM headers can be large.
    let mut buf = Vec::new();
    let mut f2 = open!(path);
    let _ = f2.take(limit).read_to_end(&mut buf);
    return Ok(dispatch_from_bytes(&buf, &name, cfg));
}
// … existing text sniff continues
```

### `mime_to_kind` update

Add DICOM to the MIME mapping (the `infer` crate does recognise DICOM via its
magic bytes, returning `application/dicom`):

```rust
if mime == "application/dicom" { return "dicom"; }
```

This provides a fallback `kind` for the rare case where the file slips past the
explicit accepts checks (e.g. a future code path that skips dispatch).

---

## Files Changed

| File | Change |
|------|--------|
| `crates/extractors/dicom/` | New crate — metadata extractor; exposes `accepts`, `accepts_bytes`, `extract_from_bytes` |
| `crates/preview-dicom/` | New crate — PNG converter binary |
| `crates/extractors/dispatch/src/lib.rs` | Add DICOM to dispatch chain, `claimed_by_specialist`, sniff re-read, and `mime_to_kind` |
| `crates/common/src/config.rs` | Add `dicom_preview_timeout_secs` to `ServerScanConfig` |
| `crates/server/src/routes/` | New `dicom_preview.rs` route |
| `crates/server/src/routes.rs` | Mount new route |
| `web/src/lib/kindOptions.ts` | Add `dicom` kind |
| `web/src/lib/FileViewer.svelte` | Route `dicom` kind to `DicomViewer` |
| `web/src/lib/DicomViewer.svelte` | New component |
| `mise.toml` / packaging scripts | Include `find-preview-dicom` in release builds |

---

## Build Flags

In `mise.toml` (or equivalent):

```toml
[tasks.build-release]
# existing flags...
# Add jpeg2000 feature to preview binary
run = "cargo build --release -p find-preview-dicom --features jpeg2000 && ..."
```

CI builds with `jpeg2000`; developer builds without (pure Rust, faster, no C toolchain needed).

---

## Testing

| Test | Location |
|------|----------|
| Metadata extraction parses known DICOM tags | `crates/extractors/dicom/tests/` |
| Preview binary exits 0 and produces valid PNG for an uncompressed test file | `crates/preview-dicom/tests/` |
| Preview route returns 200 + PNG content-type for indexed DICOM | `crates/server/tests/dicom_preview.rs` |
| Preview route returns 422 for non-DICOM file path | Same |
| DICOM files appear in search results and are findable by modality/body-part text | `crates/server/tests/dicom_preview.rs` |
| Extensionless DICOM file is correctly detected and indexed (not silently dropped as `binary`) | `crates/extractors/dicom/tests/` |
| Extensionless DICOM inside a ZIP archive member is detected and indexed | `crates/server/tests/dicom_preview.rs` |

A small public-domain DICOM test file should be checked in under `tests/fixtures/`
(several are available from the OsiriX sample library under open licences).

---

## Breaking Changes

None. New kind, new route, new binaries. Existing installs gain DICOM support when
the new binaries are deployed alongside the updated server.

---

## Open Questions

- Should `find-preview-dicom` accept an optional `--frame N` argument for future
  multi-frame navigation? Easy to add now, costs nothing if unused.
- Should the preview route cache the PNG to disk to avoid re-converting on every
  view? Not needed for v1 — DICOM files are typically local and conversion is fast
  for single-frame images. Can revisit if CT series (200+ frames) become common.
