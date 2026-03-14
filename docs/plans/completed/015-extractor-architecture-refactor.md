# Plan 015: Extractor Architecture Refactor

## Context

Currently, all extractors live in `find-common`, which means:
- **Problem 1**: Server binary includes all extractor dependencies despite never using them
- **Problem 2**: Incremental updates load the entire client binary with all extractors, even for single-file changes
- **Problem 3**: Heavy dependencies (pdf-extract, image processing, etc.) slow startup for lightweight operations

With incremental file watching on the roadmap, we need to optimize for:
- **Minimal footprint** for processing single file changes
- **Fast startup time** for incremental operations
- **Lean server binary** (no extractor bloat)
- **Efficient batch scanning** (don't sacrifice bulk performance)

## Key Insight: Incremental Updates Don't Need Long-Running Processes

When a file changes:
1. File system watcher detects: `document.pdf` changed
2. Need to extract content from this ONE file
3. Send extracted lines to server
4. Exit

This is a **short-lived operation** - we don't need a daemon with all extractors loaded in memory.

## Proposed Architecture: Plugin-Based Extractors

### Crate Structure

```
find-anything/
├── crates/
│   ├── common/              # API types + config ONLY (no extractors)
│   ├── client-core/         # Scanning logic, API client (no extractors)
│   ├── server/              # Server (no extractors)
│   │
│   └── extractors/          # New directory
│       ├── text/            # Text + Markdown frontmatter
│       ├── pdf/             # PDF extraction
│       ├── archive/         # ZIP/TAR/7Z/nested archives
│       └── media/           # Image EXIF, Audio tags, Video metadata
```

Each extractor crate is **both a library AND a binary**:

```toml
# crates/extractors/pdf/Cargo.toml
[package]
name = "find-extract-pdf"

[lib]
name = "find_extract_pdf"

[[bin]]
name = "find-extract-pdf"
path = "src/main.rs"

[dependencies]
find-common = { path = "../../common" }
pdf-extract = "0.7"
```

### Two Client Binaries

**1. `find-scan` - Full batch client**
- Links ALL extractor libraries statically
- Used for: Initial indexing, full re-scans, bulk operations
- One process, all extractors in memory, maximum efficiency for bulk

**2. `find-scan-watch` - Incremental client**
- Minimal binary with NO extractors linked
- Detects file type
- Spawns appropriate extractor subprocess
- Used for: File watcher, single-file updates, incremental changes

### Extractor Binary Protocol

Each extractor binary accepts a file path and outputs JSON:

```bash
$ find-extract-pdf /path/to/document.pdf
{
  "lines": [
    {"line_number": 1, "content": "Introduction", "archive_path": null},
    {"line_number": 2, "content": "This document...", "archive_path": null}
  ]
}
```

**Standard interface** for all extractors:
```bash
find-extract-{type} [--max-size-kb N] <file-path>
```

Output: JSON array of `IndexLine` objects to stdout

### Extractor Grouping Strategy

**Group by dependency weight:**

1. **`find-extract-text`** (~1 MB)
   - Plain text files
   - Markdown (with frontmatter via gray_matter)
   - Code files
   - Config files
   - Dependencies: gray_matter, serde_yaml
   - Most common, needs to be lightweight

2. **`find-extract-pdf`** (~10 MB)
   - PDF text extraction
   - Dependencies: pdf-extract (heavy)

3. **`find-extract-archive`** (~5 MB)
   - ZIP, TAR, GZ, BZ2, XZ, 7Z
   - Nested archive extraction
   - Dependencies: zip, tar, flate2, bzip2, xz2, sevenz-rust
   - Complex: needs to recurse and call other extractors

4. **`find-extract-media`** (~15 MB)
   - Image EXIF (kamadak-exif)
   - Audio tags (id3, metaflac, mp4ameta)
   - Video metadata (audio-video-metadata)
   - Dependencies: Heavy media libraries
   - Grouped together because rarely used in typical dev workflows

### Archive Extractor Special Case

Archives are special because they contain other files that need extraction:

```
taxes.zip
  ├── w2.pdf          (needs PDF extractor)
  ├── 1099.txt        (needs text extractor)
  └── receipts.tar.gz (needs archive extractor, then recurses)
```

**Solution**: Archive extractor acts as an orchestrator:
1. Extracts archive members to temp directory
2. Detects member type
3. Spawns appropriate sub-extractor for each member
4. Aggregates results with composite paths (`archive.zip::member.pdf`)
5. Respects `max_depth` limit

**In batch mode** (`find-scan`):
- Archive extractor uses linked library functions directly (no subprocess spawning)
- Most efficient

**In incremental mode** (`find-scan-watch`):
- Archive extractor spawns sub-extractors as needed
- Slight overhead but keeps individual binaries small

## Incremental Update Flow

### Current (inefficient):
```
File changed: document.pdf (1 MB)
↓
Launch find-scan (50 MB binary, all extractors)
  - Load text extractor (not needed)
  - Load pdf extractor (needed!)
  - Load archive extractor (not needed)
  - Load media extractors (not needed)
↓
Extract + send to server
↓
Exit
```

### Proposed (efficient):
```
File changed: document.pdf (1 MB)
↓
find-scan-watch (2 MB binary)
  - Detect type: PDF
  - Spawn: find-extract-pdf (10 MB, PDF deps only)
    ↓
    Extract PDF → JSON
    ↓
    Return to parent
↓
Send to server
↓
Exit both processes
```

**Memory footprint**: 2 MB (watch) + 10 MB (PDF) = **12 MB** vs 50 MB
**Startup time**: Only load PDF extractor, not all deps

## Batch Scanning Flow

```
find-scan --source=mycode /home/user/code
↓
Statically linked with all extractors (one binary)
↓
Walk filesystem
  - file.rs   → text extractor (in-process function call)
  - image.jpg → media extractor (in-process)
  - doc.pdf   → pdf extractor (in-process)
  - data.zip  → archive extractor → sub-extractors (in-process)
↓
Send batches to server
```

**No subprocess overhead** - same efficiency as current architecture.

## Plugin Support

The extractor architecture naturally supports plugins through a simple protocol:

**Extractor Protocol:**
- Input: File path as command-line argument
- Output: JSON array of `IndexLine` objects to stdout
- Exit code: 0 for success, non-zero for error

**Custom Extractor Configuration:**
```toml
# config.toml
[extractors.custom]
"dwg" = "/usr/local/bin/autocad-extractor"
"msg" = "outlook-msg-extractor"  # searches PATH
```

**Benefits:**
- Users can add extractors for proprietary formats
- Third parties can create extractors without modifying core
- Plugins run in isolated processes (security)
- No complex plugin infrastructure needed

**Grouping Strategy:**
- Group extractors by commonality and dependency weight
- Example: Office extractors (DOCX, XLSX, PPTX) in `find-extract-office`
- Keeps related formats together, reduces binary count

## Implementation Plan

### Phase 1: Restructure Crates (No behavior change)

1. Create `crates/extractors/` directory
2. Move extractors from `common/src/extract/` to individual crates:
   - `crates/extractors/text/` - Text + Markdown frontmatter
   - `crates/extractors/pdf/` - PDF extraction
   - `crates/extractors/archive/` - ZIP/TAR/7Z + nested orchestration
   - `crates/extractors/media/` - Image/Audio/Video metadata
3. Each exposes:
   - Library: `pub fn extract(path: &Path, max_size_kb: usize) -> Result<Vec<IndexLine>>`
   - Binary: CLI wrapper that calls library function, outputs JSON
4. Update `find-scan` to depend on all extractor libraries
5. Remove extractors from `find-common`, keep only API types + config
6. Add plugin support:
   - Document extractor protocol (`docs/EXTRACTOR_PROTOCOL.md`)
   - Add `[extractors.custom]` config section
   - Update file type dispatcher to check custom extractors
   - Create example custom extractor in `examples/`
7. Verify: Batch scanning still works identically

### Phase 2: Create Incremental Client

6. Create `find-scan-watch` binary:
   - File type detection (by extension + content sniffing)
   - Subprocess spawning for extractors
   - JSON parsing from extractor output
   - API client to send results
7. Test: Single file extraction via subprocess
8. Integrate with file watcher (future plan)

### Phase 3: Archive Orchestration

9. Enhance archive extractor to:
   - Detect when running as subprocess vs in-process
   - If subprocess: spawn sub-extractors for members
   - If in-process: call library functions directly
10. Test: Nested archive extraction in both modes

## Crate Dependencies After Refactor

```
find-common (API types, config)
  ├─ serde, serde_json
  └─ (no heavy deps)

find-client-core (scanning logic)
  ├─ find-common
  ├─ walkdir, globset
  └─ reqwest (API client)

find-extract-text (lib + bin)
  ├─ find-common
  ├─ gray_matter
  └─ serde_yaml

find-extract-pdf (lib + bin)
  ├─ find-common
  └─ pdf-extract

find-extract-archive (lib + bin)
  ├─ find-common
  ├─ zip, tar, flate2, bzip2, xz2, sevenz-rust
  └─ (spawns other extractors in subprocess mode)

find-extract-media (lib + bin)
  ├─ find-common
  ├─ kamadak-exif (images)
  ├─ id3, metaflac, mp4ameta (audio)
  └─ audio-video-metadata (video)

find-scan (full batch client)
  ├─ find-client-core
  ├─ find-extract-text (as library)
  ├─ find-extract-pdf (as library)
  ├─ find-extract-archive (as library)
  └─ find-extract-media (as library)

find-scan-watch (incremental client)
  ├─ find-client-core
  └─ (spawns extractor binaries as subprocesses)

find-server
  ├─ find-common
  └─ (no extractors!)
```

## Binary Size Comparison

**Current:**
- `find-server`: ~20 MB (includes unused extractor deps)
- `find-scan`: ~25 MB (all extractors)

**After refactor:**
- `find-server`: ~8 MB (no extractor deps) ✓
- `find-scan`: ~25 MB (all extractors, no change)
- `find-scan-watch`: ~2 MB (no extractors) ✓
- `find-extract-text`: ~2 MB ✓
- `find-extract-pdf`: ~10 MB ✓
- `find-extract-archive`: ~6 MB ✓
- `find-extract-media`: ~15 MB ✓

**For incremental update of a PDF:**
- Current: Load 25 MB binary
- After: Load 2 MB (watch) + 10 MB (PDF) = 12 MB, only PDF deps initialized

## Future Content Types

When adding new extractors (EML, DOCX, XLSX from Plan 013):

1. Create new crate: `crates/extractors/{name}/`
2. Implement library + binary
3. Add to `find-scan` dependencies
4. Add to `find-scan-watch` dispatcher

**Zero impact on:**
- Server binary (doesn't rebuild)
- Other extractors (isolated)
- Users not needing that file type

## Downsides / Tradeoffs

**Cons:**
1. **More binaries**: 4 extractor binaries + 2 clients + server = 7 binaries total
2. **Distribution complexity**: Must ship all binaries, ensure PATH is correct
3. **IPC overhead**: Subprocess spawning + JSON serialization for incremental mode
4. **Testing complexity**: Must test both in-process and subprocess modes

**Mitigations:**
1. Package as single archive with install script that sets up PATH
2. IPC overhead negligible compared to extraction work (PDFs, archives are slow)
3. Test matrix: batch mode (in-process) + incremental mode (subprocess)

**When NOT to use this architecture:**
- If you never plan to implement file watching (but it's on roadmap)
- If binary size and startup time don't matter
- If subprocess overhead is critical (it's not for file I/O-bound tasks)

## Alternatives Considered

### Alt A: Keep extractors in common, accept bloat
- Pros: Simple
- Cons: Server stays bloated, incremental updates slow

### Alt B: Single client binary with feature flags
```toml
find-scan = { features = ["pdf", "archive", "media"] }
find-scan-minimal = { default-features = false }
```
- Pros: Fewer binaries
- Cons: Still loads all linked extractors at runtime, doesn't solve footprint issue

### Alt C: Dynamic library loading (dlopen)
- Pros: True lazy loading
- Cons: Platform-specific, complex, distribution nightmare

### Alt D: Extractor microservices
- Each extractor runs as HTTP service
- Client makes REST calls
- Pros: Ultimate isolation
- Cons: Massive overkill, network overhead, complexity

**Recommendation: Stick with Plugin-Based Extractors (Option B in this plan)**

## Success Criteria

- [ ] Server binary has zero extractor dependencies
- [ ] Server binary size < 10 MB
- [ ] `find-scan` works identically to current (batch mode)
- [ ] `find-scan-watch` can extract single files via subprocesses
- [ ] Memory footprint for incremental PDF update < 15 MB
- [ ] All extractor binaries output valid JSON
- [ ] Archive extractor correctly orchestrates nested extraction
- [ ] All existing tests pass
- [ ] Binary distribution includes all executables

## Rollout Strategy

1. **Phase 1**: Refactor (no user-visible changes)
   - Release as 0.2.0 (minor bump for architecture change)
   - Users still use `find-scan` for everything
2. **Phase 2**: Introduce `find-scan-watch` (optional)
   - Release as 0.3.0
   - Users can opt-in to incremental client
3. **Phase 3**: Implement file watching
   - Release as 0.4.0
   - Automatic incremental updates

---

## Questions for Discussion

1. **Binary distribution**: Single tarball with all binaries, or separate packages?
2. **Naming**: `find-extract-{type}` or `find-{type}-extractor`?
3. **Archive orchestration**: Should archive extractor directly call other extractors as libraries in subprocess mode, or always spawn binaries?
4. **CLI interface**: Should extractor binaries support batch mode (multiple files)?
5. **Error handling**: If an extractor subprocess crashes, how should parent handle it?
