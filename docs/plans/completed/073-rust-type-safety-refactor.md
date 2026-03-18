# Plan 073: Rust Type-Safety Refactor

## Overview

A staged refactor to eliminate stringly-typed patterns, anonymous tuple structs, and
other anti-patterns throughout the Rust codebase, replacing them with idiomatic
type-safe constructs. Each stage is independently implementable and mergeable.

The refactor also produces `docs/rust-style.md` — a narrow, codebase-specific style
guide referenced from `CLAUDE.md` — so that the patterns established here become the
default going forward.

**Reference:** [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) +
[Clippy lint catalogue](https://rust-lang.github.io/rust-clippy/master/).

---

## Stage 1: `FileKind` Enum

**Motivation:** `kind: String` appears on every `IndexFile`, `SearchResult`,
`FileRecord`, and related struct. String comparisons (`file.kind == "text"`) are
scattered across the worker, pipeline, scan client, and search routes with no
compile-time exhaustiveness checking and no protection against typos.

### Definition

```rust
// crates/common/src/api.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Text,
    Pdf,
    Archive,
    Image,
    Audio,
    Video,
    Document,
    Executable,
    Epub,
    #[serde(other)]
    Unknown,
}

impl FileKind {
    /// Re-derive kind from file extension. Used when a client sends Unknown.
    pub fn from_extension(ext: &str) -> Self { ... }

    /// True for kinds whose extracted lines are passed through the text normalizer.
    pub fn is_text_like(&self) -> bool {
        matches!(self, Self::Text | Self::Pdf)
    }
}
```

- `#[serde(rename_all = "lowercase")]` — wire format unchanged (`"pdf"`, `"text"`, …)
- `#[serde(other)]` on `Unknown` — any unrecognised string from a client deserialises
  cleanly to `Unknown` rather than returning a deserialisation error
- `Hash` derived — enables use as `HashMap` key (see `by_kind` below)

### Unknown Kind Handling

When the worker receives a file with `kind == FileKind::Unknown`:
1. Log `warn!(path = %file.path, "received unknown file kind — re-detecting from extension")`
2. Re-derive via `FileKind::from_extension(&file.path)`
3. Continue with the re-detected kind

This logic is inserted at the top of the per-file loop in `worker/request.rs`.

### `by_kind` HashMap Key

`SourceStats::by_kind` changes from `HashMap<String, KindStats>` to
`HashMap<FileKind, KindStats>`. This falls out of the enum deriving `Hash` at no
extra cost.

### Call Sites

All `kind: String` fields in `api.rs` are migrated. The full list:

| Field | Struct |
|-------|--------|
| `IndexFile.kind` | write path |
| `SearchResult.kind` | search response |
| `FileResponse.file_kind` | file view response (field renamed `file_kind`, keep name) |
| `ContextResponse.kind` | context endpoint |
| `ContextBatchResult.kind` | context-batch endpoint |
| `DirEntry.kind` | tree endpoint (`Option<String>` → `Option<FileKind>`) |
| `FileRecord.kind` | file listing endpoint |
| `InboxShowFile.kind` | admin inbox endpoint |
| `ResolveLinkResponse.kind` | link sharing |
| `SourceStats.by_kind` key | stats endpoint |

Code changes by file:

| File | Change |
|------|--------|
| `common/src/api.rs` | All fields above; `by_kind` key type |
| `server/src/worker/request.rs` | String comparisons → `matches!` / `match`; Unknown re-detection |
| `server/src/worker/pipeline.rs` | `kind == "archive"` in `is_outer_archive`, `filename_only_file`, `outer_archive_stub` → `FileKind::Archive` |
| `server/src/db/search.rs` | Kind filter uses `.to_string()` for the SQL `?N` param |
| `server/src/db/stats.rs` | `by_kind` aggregation |
| `client/src/scan.rs` | Kind detection and comparison |
| `client/src/batch.rs` | Archive member kind handling |

### Testing

- Unit: each `FileKind` variant round-trips through `serde_json`
- Unit: unknown string deserialises to `FileKind::Unknown`
- Unit: `FileKind::from_extension` covers all known extensions
- Integration: existing pipeline tests in `crates/server/tests/` confirm no regression

---

## Stage 2: Remaining API Enums

**Motivation:** `SearchMode`, `RecentAction`, and `WorkerQueueSlot` are all stringly
typed in `api.rs` and matched with `match x.as_str()` at use sites, with no
exhaustiveness checking when new variants are added.

### `SearchMode`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SearchMode {
    #[default]
    #[serde(other)]   // unknown mode from future client → Fuzzy (safe fallback)
    Fuzzy,
    Exact,
    Regex,
    Document,         // fuzzy multi-term document mode
    FileFuzzy,
    FileExact,
    FileRegex,
    DocExact,
    DocRegex,
}
```

`kebab-case` preserves the existing wire format exactly (`"file-fuzzy"`, `"doc-exact"`).
`Default = Fuzzy` matches the current fallback when `mode` is absent in the request.
`#[serde(other)]` is placed on `Fuzzy` itself — this is the correct placement to make
unknown strings deserialise to `Fuzzy` rather than fail. (A separate `Unknown` variant
would require callers to handle it explicitly everywhere; `Fuzzy` as the catch-all is
semantically correct: fall back to a safe search.)

`Document` is the ninth mode (`"document"` in the wire format) handling fuzzy
multi-term document search — previously absent from the enum draft but present in
`routes/search.rs`.

Replaces `match mode.as_str() { "fuzzy" => … _ => {} }` in `routes/search.rs` with
an exhaustive `match`.

### `RecentAction`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecentAction {
    Added,
    Modified,
    Deleted,
    Renamed,
}
```

Replaces `action: String` on `RecentFile`. Call sites in `worker/request.rs` that
construct `action: "added".into()` become `action: RecentAction::Added`.
TypeScript receives the same lowercase strings — no web changes needed.

No `#[serde(other)]` here: the server is the sole producer of `RecentAction` values,
so an unknown value would be a server bug, not a client compatibility issue.

### `WorkerQueueSlot`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerQueueSlot {
    Pending,
    Failed,
}
```

Replaces `queue: String` on `InboxShowResponse` (used by `GET /api/v1/admin/inbox`).
Same producer-is-server rationale — no `#[serde(other)]`.

### Testing

- Unit: serde round-trip for each variant of each enum
- Unit: `SearchMode` — unrecognised string → `Fuzzy` via `#[serde(other)]`
- Unit: `RecentAction` / `WorkerQueueSlot` — unrecognised string returns a
  deserialisation error (no `#[serde(other)]`)

---

## Stage 3: Named Structs for Anonymous Tuples

**Motivation:** Multiple sites use positional tuples for domain concepts that have
clear named fields. Positional access makes code fragile under refactoring and
eliminates IDE support for understanding intent.

All structs in this stage are internal (`pub(crate)` at most) — no API surface change.

### `WatchSource` (replaces 5-element tuple in `client/src/watch.rs`)

The current type alias is:
```rust
type SourceMap = Vec<(PathBuf, String, String, GlobSet, Option<HashSet<String>>)>;
//                    root    name   root_str excludes  includes
```

`root_str` (index 2) is the normalised root as a `String`, distinct from `root: PathBuf`.

```rust
struct WatchSource {
    root:        PathBuf,
    source_name: String,
    root_str:    String,   // normalised root path as String
    excludes:    GlobSet,
    includes:    Option<HashSet<String>>,
}
type SourceMap = Vec<WatchSource>;
```

### `TreeRow` (new intermediate struct in `db/tree.rs`)

`list_dir` currently uses an inline anonymous tuple inside `query_map` and maps
directly to `DirEntry` in the same closure. This stage introduces an intermediate
named struct to hold the raw DB columns before the `DirEntry` construction:

```rust
// private to module
struct TreeRow {
    path:       String,
    entry_type: String,
    size:       Option<i64>,
    mtime:      i64,
}
```

The `query_map` closure returns `TreeRow`; a second `.map()` converts to `DirEntry`.
This is a net addition of ~5 lines but makes column mapping explicit.

### `DocumentGroup` (replaces inner tuple in `db/search.rs`)

```rust
pub(crate) struct DocumentGroup {
    pub representative: CandidateRow,
    pub members:        Vec<CandidateRow>,
}
pub type DocumentCandidates = (usize, Vec<DocumentGroup>);
```

The outer `(usize, Vec<…>)` is a count paired with results — a scalar pairing that
doesn't warrant a separate struct.

### `ScoredResult` (replaces `(SearchResult, i64)` in `routes/search.rs`)

The `i64` is the file's mtime, used for post-processing sort order:

```rust
struct ScoredResult {
    result: SearchResult,
    mtime:  i64,
}
```

### `AliasRow` (replaces `(i64, String)` in `db/mod.rs`)

```rust
struct AliasRow {
    file_id: i64,
    path:    String,
}
```

### Testing

No behaviour change. Clippy CI confirms no regressions. Existing integration tests
provide full coverage of the affected code paths.

---

## Stage 4: SQL Query Hardening

**Motivation:** Dynamic WHERE clauses in `db/search.rs` are built with string
interpolation and manually numbered placeholders (`?1`, `?2`, `?3`…). Inserting or
removing a clause requires renumbering every downstream `?N` by hand — a silent
correctness hazard.

### `ParamBinder` Helper

```rust
// private to db/search.rs
struct ParamBinder {
    params: Vec<Box<dyn rusqlite::ToSql>>,
}

impl ParamBinder {
    fn new() -> Self { Self { params: vec![] } }

    /// Append a value and return its `?N` placeholder string.
    fn push(&mut self, v: impl rusqlite::ToSql + 'static) -> String {
        self.params.push(Box::new(v));
        format!("?{}", self.params.len())
    }

    fn as_refs(&self) -> Vec<&dyn rusqlite::ToSql> {
        self.params.iter().map(|p| p.as_ref()).collect()
    }
}
```

**Lifetime note:** `as_refs()` returns a `Vec` that borrows from `self`. The result
must be bound to a variable before passing to `stmt.query()` — the temporary cannot
be created inline in the same expression:

```rust
let mut p = ParamBinder::new();
let fts_ph   = p.push(query_text);
let limit_ph = p.push(limit);
let from_ph  = p.push(date_from);
let to_ph    = p.push(date_to);

let kind_clause = match kind_filter {
    Some(k) => format!("AND f.kind = {}", p.push(k.to_string())),
    None    => String::new(),
};

let sql = format!("SELECT … WHERE lines_fts MATCH {fts_ph}
                   AND f.mtime BETWEEN {from_ph} AND {to_ph}
                   {kind_clause} LIMIT {limit_ph}");

let refs = p.as_refs();                // bind before query
stmt.query(refs.as_slice())?;         // borrow from `refs`, not a temporary
```

Applied to the three dynamic query sites: `fts_count`, `fts_candidates`,
`document_candidates`.

### Testing

- Unit: `ParamBinder::push` returns sequentially numbered placeholders (`?1`, `?2`, …)
- Unit: `as_refs()` length matches push count
- Integration: existing `search_modes.rs` tests exercise all three query paths

---

## Stage 5: Function Context Structs

**Motivation:** Two functions carry `#[allow(clippy::too_many_arguments)]`. Both
split cleanly into named context structs following the `ExtractorConfig` pattern
documented in `CLAUDE.md`.

### `ExtractedFile` (client/src/scan.rs — 8 params)

```rust
pub struct ExtractedFile {
    pub rel_path:   String,
    pub abs_path:   PathBuf,
    pub mtime:      i64,
    pub size:       i64,
    pub kind:       FileKind,   // FileKind enum from Stage 1
    pub lines:      Vec<IndexLine>,
    pub extract_ms: u64,
    pub is_new:     bool,
}

async fn push_non_archive_files(
    ctx:  &mut ScanContext<'_>,
    file: &ExtractedFile,
) -> Result<()>
```

### `RequestContext` + `IndexerHandles` (worker/request.rs — 9 params)

The existing `worker/mod.rs` already defines a `WorkerHandles` struct for a different
purpose. The per-request structs use distinct names to avoid collision:

```rust
pub(super) struct RequestContext {
    pub data_dir:       PathBuf,
    pub request_path:   PathBuf,
    pub failed_dir:     PathBuf,
    pub to_archive_dir: PathBuf,
}

pub(super) struct IndexerHandles {
    pub status:         StatusHandle,
    pub cfg:            WorkerConfig,
    pub archive_notify: Arc<tokio::sync::Notify>,
    pub shared_archive: Arc<SharedArchiveState>,
    pub recent_tx:      broadcast::Sender<RecentFile>,
}

pub(super) async fn process_request_async(
    ctx:     &RequestContext,
    handles: &IndexerHandles,
)
```

`IndexerHandles` is constructed once when the worker task starts and passed by
reference to each request. `RequestContext` is constructed per-request from the
inbox path.

### Testing

No behaviour change. Removal of `#[allow(clippy::too_many_arguments)]` annotations
confirms Clippy is satisfied.

---

## Stage 6: Style Guide

**File:** `docs/rust-style.md`

Written after stages 1–5 are implemented so every example references real, tested
code rather than aspirational snippets.

**`CLAUDE.md` addition** (in "Project Conventions"):

```markdown
### Rust style
See [`docs/rust-style.md`](docs/rust-style.md) for binding patterns and idioms
specific to this codebase. When a situation is not covered there, refer to the
[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and the
[Clippy lint catalogue](https://rust-lang.github.io/rust-clippy/master/).
```

**Guide sections:**

1. **Enums over strings for fixed value sets** — `FileKind`, `SearchMode` as examples; when to add `#[serde(other)]`
2. **`#[serde(other)]` policy** — use on the safe-fallback variant when the server is a *consumer* (client sends the value); omit when the server is the sole *producer*
3. **Config structs over parameter threading** — `ExtractorConfig`, `ExtractedFile`, `IndexerHandles` as examples; threshold: more than one threaded param
4. **Named structs over anonymous tuples** — `WatchSource`, `DocumentGroup` as examples; any tuple used outside its construction site
5. **Typed HashMap keys** — `HashMap<FileKind, KindStats>` vs. `HashMap<String, KindStats>`; derive `Hash` + `Eq` on the key enum
6. **Dynamic SQL with `ParamBinder`** — sequential-push pattern; lifetime note about binding `as_refs()` before query
7. **Error handling conventions** — `anyhow::Result` at crate boundaries; `thiserror` path for future typed errors
8. **Where to find more** — Rust API Guidelines + Clippy master index (links only)

---

## Files Changed (summary)

| Stage | Primary files |
|-------|--------------|
| 1 | `common/src/api.rs`, `server/src/worker/request.rs`, `server/src/worker/pipeline.rs`, `server/src/db/search.rs`, `server/src/db/stats.rs`, `client/src/scan.rs`, `client/src/batch.rs` |
| 2 | `common/src/api.rs`, `server/src/routes/search.rs`, `server/src/worker/request.rs`, `server/src/routes/admin.rs` |
| 3 | `client/src/watch.rs`, `server/src/db/tree.rs`, `server/src/db/search.rs`, `server/src/routes/search.rs`, `server/src/db/mod.rs` |
| 4 | `server/src/db/search.rs` |
| 5 | `client/src/scan.rs`, `server/src/worker/request.rs`, `server/src/worker/mod.rs` |
| 6 | `docs/rust-style.md` (new), `CLAUDE.md` |

## Breaking Changes

None externally. JSON wire format is preserved by `serde` attributes on all new enums.
`MIN_CLIENT_VERSION` does not need bumping.

## Deferred

- **`SourceName` / `FilePath` newtypes** — real value but touches every API call site; disruption exceeds benefit at this time
- **Composite path type** — significant architectural change; deserves its own dedicated plan
