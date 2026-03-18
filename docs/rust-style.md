# Rust Style Guide

Codebase-specific patterns and idioms for find-anything. Every example below
references real, tested code in this repo.

When a situation is not covered here, refer to the
[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and the
[Clippy lint catalogue](https://rust-lang.github.io/rust-clippy/master/).

---

## 1. Enums over strings for fixed value sets

Any field that can only take a known set of string values should be an enum,
not a `String`. String comparisons have no exhaustiveness checking and silently
accept typos at runtime.

**Do this:**

```rust
// crates/common/src/api.rs:15
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Text, Pdf, Archive, Image, Audio, Video,
    Document, Executable, Epub,
    #[serde(other)]
    Unknown,
}
```

**Not this:**

```rust
pub kind: String  // was the old shape — scattered "kind == "pdf"" checks everywhere
```

The `serde` attribute preserves the wire format, so existing clients and the
database are not affected. `Hash + Eq` derived — enables typed `HashMap` keys
(see §5).

Other examples in this codebase:
- `SearchMode` (`crates/common/src/api.rs:91`) — nine search modes, `kebab-case` wire format
- `RecentAction` (`crates/common/src/api.rs:113`) — `Added | Modified | Deleted | Renamed`
- `WorkerQueueSlot` (`crates/common/src/api.rs:137`) — `Pending | Failed`
- `FileKind` (`crates/common/src/api.rs:15`) — all file type variants

---

## 2. `#[serde(other)]` policy

`#[serde(other)]` on an enum variant makes every unrecognised string deserialise
to that variant instead of returning an error.

**Use it when the server is a *consumer*:** the client sends the value, so an
unknown string could arrive from an older or newer client version. Choose the
safe-fallback variant (the one that produces a valid no-crash response) and put
`#[serde(other)]` on it.

```rust
// SearchMode: unknown mode from a future client → fall back to Fuzzy (safe search)
#[serde(other)]
Fuzzy,
```

```rust
// FileKind: unknown kind from an older client → Unknown (worker re-detects from extension)
#[serde(other)]
Unknown,
```

**Omit it when the server is the sole *producer*:** the server constructs the
value, so receiving an unrecognised string would be a server bug, not a
compatibility issue. Let deserialisation fail loudly.

```rust
// RecentAction — only the server writes these; no #[serde(other)]
pub enum RecentAction { Added, Modified, Deleted, Renamed }

// WorkerQueueSlot — only the server writes these; no #[serde(other)]
pub enum WorkerQueueSlot { Pending, Failed }
```

---

## 3. Config structs over parameter threading

**Rule:** as soon as a function would thread **more than one parameter** through
a call chain, introduce a config struct instead.

**Why:** function signatures stay stable when new settings are added — only the
struct definition and its single construction site change.

### Named examples

`ExtractorConfig` (`crates/extract-types/src/extractor_config.rs:10`) bundles
`max_size_kb`, `max_depth`, and `max_line_length`. It is built once from the
top-level `ScanConfig` and passed down through the PDF, archive, and HTML
extractors without each intermediate function needing to know which fields
exist.

```rust
// Build once, pass by reference everywhere downstream
let cfg = extractor_config_from_scan(&scan);  // crates/common/src/config.rs:487
extract_pdf(path, &cfg)?;
extract_archive(path, &cfg)?;
```

`ExtractedFile` (`crates/client/src/scan.rs:382`) bundles the eight outputs of
a single-file extraction step so `push_non_archive_files` takes two arguments
instead of nine:

```rust
push_non_archive_files(ctx, &ExtractedFile {
    rel_path, abs_path, mtime, size, kind, lines, extract_ms, is_new,
}).await?;
```

`RequestContext` + `IndexerHandles` (`crates/server/src/worker/request.rs:31,39`)
split the nine parameters of `process_request_async` into two structs with clear
lifetimes:

```rust
// IndexerHandles: constructed once per worker task lifetime
let handles = IndexerHandles { status, cfg, archive_notify, shared_archive, recent_tx };

// RequestContext: constructed once per inbox request
let ctx = RequestContext { data_dir, request_path, failed_dir, to_archive_dir };

process_request_async(&ctx, &handles).await;
```

### Constructor pattern

Provide a `from_X` constructor (or equivalent) so call sites build the struct
once from the authoritative config and pass it down, rather than unpacking
fields at every level:

```rust
impl ExtractorConfig {
    pub fn from_scan(scan: &ScanConfig) -> Self { … }
}
```

---

## 4. Named structs over anonymous tuples

Any tuple that is passed between functions, stored in a collection, or used
outside its construction site should be a named struct.

**Rule of thumb:** if you need to write a comment explaining what each index
means, it should be a struct.

```rust
// Before — positional access hides intent and is fragile under refactoring:
type SourceMap = Vec<(PathBuf, String, String, GlobSet, Option<HashSet<String>>)>;
//                    root    name   root_str excludes  includes

// After — self-documenting, IDE-navigable:
// crates/client/src/watch.rs:33
struct WatchSource {
    root:        PathBuf,
    source_name: String,
    root_str:    String,
    excludes:    GlobSet,
    includes:    Option<HashSet<String>>,
}
```

Other examples:
- `DocumentGroup` (`crates/server/src/db/search.rs:288`) — `representative + members`, replaces `(CandidateRow, Vec<CandidateRow>)`
- `ScoredResult` (`crates/server/src/routes/search.rs:17`) — `result + mtime`, replaces `(SearchResult, i64)`
- `AliasRow` (`crates/server/src/db/mod.rs:450`) — `file_id + path`, replaces `(i64, String)`

The outer `(usize, Vec<…>)` in `DocumentCandidates` (a count paired with results)
is a scalar pairing and does not warrant its own struct.

---

## 5. Typed HashMap keys

Prefer the domain enum as the key type over `String`:

```rust
// crates/common/src/api.rs:478
pub by_kind: HashMap<FileKind, KindStats>,  // ✓ typed key

// old shape:
pub by_kind: HashMap<String, KindStats>,    // ✗ accepts any string
```

This requires deriving `Hash + Eq` on the key enum, which `FileKind` already
has. Any typo in a key expression is now a compile error rather than a silent
miss at runtime.

---

## 6. Dynamic SQL with `ParamBinder`

Dynamic `WHERE` clauses built with string interpolation require manually
numbered placeholders (`?1`, `?2`, `?3`…). Inserting or removing a clause
forces every downstream `?N` to be renumbered by hand — a silent correctness
hazard.

Use `ParamBinder` (`crates/server/src/db/search.rs:49`) instead:

```rust
let mut p = ParamBinder::new();
let fts_ph   = p.push(query_text);      // returns "?1"
let limit_ph = p.push(limit);           // returns "?2"
let from_ph  = p.push(date_from);       // returns "?3"
let to_ph    = p.push(date_to);         // returns "?4"

let kind_clause = match kind_filter {
    Some(k) => format!("AND f.kind = {}", p.push(k.to_string())),  // "?5"
    None    => String::new(),
};

let sql = format!("SELECT … WHERE lines_fts MATCH {fts_ph}
                   AND f.mtime BETWEEN {from_ph} AND {to_ph}
                   {kind_clause} LIMIT {limit_ph}");

// ⚠ Lifetime note: bind as_refs() to a variable BEFORE passing to query.
// The refs borrow from `p`, so the temporary cannot be created inline.
let refs = p.as_refs();
stmt.query(refs.as_slice())?;
```

Adding a new filter means one extra `p.push(…)` call — no renumbering required.

---

## 7. Error handling conventions

**Crate boundaries:** use `anyhow::Result` at public function boundaries and
in binary entry points. It carries a `context()` chain, prints cleanly, and
requires no per-crate error enum boilerplate.

```rust
// Every public extractor, DB function, and route handler uses this:
pub fn extract(path: &Path, cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>>
```

**Internal helpers:** plain `Result<T, E>` with a concrete error type is fine
when the error type is already defined and adding `anyhow` would obscure it.

**Future typed errors:** if a crate boundary needs machine-readable error
variants (e.g. to distinguish "DB locked" from "parse error" without
string-matching), introduce a `thiserror`-derived enum at that point. The
`is_db_locked` helper in `crates/server/src/worker/request.rs` is a candidate
for this treatment.

---

## 8. Where to find more

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) — naming,
  interoperability, documentation, and predictability conventions
- [Clippy lint catalogue](https://rust-lang.github.io/rust-clippy/master/) —
  searchable index of all Clippy lints with rationale and examples
