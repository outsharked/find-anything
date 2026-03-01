# 038 - Test Coverage

## Overview

This plan establishes an incremental test suite for find-anything. The project
has had several concrete regressions in the past few weeks:

- **Alias column read bug (795a13a):** `fetch_aliases_for_canonical_ids` read
  column 0 (`canonical_file_id`, INTEGER) as `String` instead of column 1
  (`path`). The error was swallowed by the search route, producing empty
  results for any query that hit a file with aliases.

- **Extension stats REVERSE() bug (a025efb):** `get_stats_by_ext` used SQL
  that called `REVERSE()`, which is not a built-in SQLite function. The query
  silently returned empty results.

- **Log-filter bypass through subprocess relay (c029136):**
  `relay_subprocess_logs` emitted tracing events without calling `is_ignored()`,
  so configured patterns had no effect on subprocess log output.

None of these would have survived a simple unit test. The goal is to add
targeted tests that would have caught each regression, plus a small set of
DB integration tests for the most regression-prone query paths.

## Design Decisions

### Unit tests only in Phase 1

The bugs seen were all pure logic bugs testable without any HTTP layer. Phase 1
targets:

1. `logging.rs` — `is_ignored` and pattern matching
2. `subprocess.rs` — relay suppression respecting `is_ignored`
3. `db.rs` — pure helpers (`build_fts_query`, `split_composite_path`, `prefix_bump`)
4. `worker.rs` — `filename_only_file` and the outer-archive bypass check

### Phase 2 — DB-backed tests using `:memory:` SQLite

`rusqlite` supports in-memory DBs. These tests exercise the full SQL path
without the filesystem. No HTTP server or async runtime needed.

Key coverage: `fetch_aliases_for_canonical_ids`, `get_stats_by_ext`, the custom
scalar functions (`file_ext`, `file_basename`), and FTS5 query construction.

### Testability refactors (small, non-breaking)

- Extract `register_scalar_functions` + schema init into a
  `pub(crate) fn init_connection(conn: &Connection)` helper called by both
  `open()` and tests.
- Add `pub(crate) fn is_ignored_with(patterns: &[regex::Regex], msg: &str) -> bool`
  to `logging.rs` so tests can inject patterns without fighting `OnceLock`.
- Make `build_fts_query`, `split_composite_path`, and `prefix_bump` `pub(crate)`
  so `db_tests.rs` can call them.

### Where to place tests

All Phase 1 tests live in `#[cfg(test)] mod tests` blocks in their source
files, matching the existing project convention. Phase 2 DB tests live in a
new `crates/server/src/db_tests.rs`, imported via `#[cfg(test)] mod db_tests;`
at the bottom of `db.rs`.

## Implementation

### Existing coverage (for reference)

The following already have tests:
- `crates/common/src/config.rs` — 13 tests (config parsing, exclude merge)
- `crates/common/src/api.rs` — 9 tests (`detect_kind_from_ext`)
- `crates/server/src/archive.rs` — 4 tests (`chunk_lines`, archive numbering)
- `crates/client/src/batch.rs` — 5 tests (`build_index_files`)
- Various extractors — text/html/epub/office unit tests

The following have **zero tests** and are the highest-risk areas:
- `crates/common/src/logging.rs`
- `crates/client/src/subprocess.rs`
- `crates/server/src/db.rs`
- `crates/server/src/worker.rs`

---

### Phase 1.1 — `crates/common/src/logging.rs`

Add `pub(crate) fn is_ignored_with(patterns: &[regex::Regex], msg: &str) -> bool`
and have the public `is_ignored` delegate to it. Then add tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn pats(s: &[&str]) -> Vec<regex::Regex> {
        s.iter().map(|p| regex::Regex::new(p).unwrap()).collect()
    }

    #[test]
    fn no_patterns_never_ignored() {
        assert!(!is_ignored_with(&[], "anything"));
    }

    #[test]
    fn substring_pattern_matches() {
        let p = pats(&["pdf_extract: unknown glyph"]);
        assert!(is_ignored_with(&p, "pdf_extract: unknown glyph name 'box3' for font ArialMT"));
    }

    #[test]
    fn non_matching_pattern_passes_through() {
        let p = pats(&["lopdf::reader"]);
        assert!(!is_ignored_with(&p, "pdf_extract: unknown glyph name 'box3' for font ArialMT"));
    }

    #[test]
    fn regex_special_chars_work() {
        let p = pats(&["unknown glyph name '.*'"]);
        assert!(is_ignored_with(&p, "pdf_extract: unknown glyph name 'box3' for font ArialMT"));
    }

    #[test]
    fn multiple_patterns_any_match_ignored() {
        let p = pats(&["no match here", "pdf_extract: unknown"]);
        assert!(is_ignored_with(&p, "pdf_extract: unknown glyph foo"));
    }

    #[test]
    fn set_ignore_patterns_rejects_invalid_regex() {
        // Calling set_ignore_patterns with invalid regex returns Err
        // (can't use set_ignore_patterns itself due to OnceLock, but we can
        // test Regex compilation directly)
        assert!(regex::Regex::new("[invalid").is_err());
    }
}
```

---

### Phase 1.2 — `crates/client/src/subprocess.rs`

Extract the per-line suppression decision into a testable helper:

```rust
/// Parses a tracing-subscriber fmt stderr line into (level_tag, message).
/// Returns None if the line is empty or matches an ignore pattern.
pub(crate) fn parse_relay_line<'a>(
    line: &'a str,
    is_ignored: impl Fn(&str) -> bool,
) -> Option<(&'static str, &'a str)> {
    let line = line.trim();
    if line.is_empty() { return None; }
    let rest = line.trim_start_matches(|c: char| !c.is_alphanumeric());
    for (prefix, tag) in [("ERROR ", "ERROR"), ("WARN ", "WARN"), ("INFO ", "INFO"),
                           ("DEBUG ", "DEBUG"), ("TRACE ", "TRACE")] {
        if let Some(msg) = rest.strip_prefix(prefix) {
            return if is_ignored(msg) { None } else { Some((tag, msg)) };
        }
    }
    // Unknown format — emit as WARN if not ignored
    if is_ignored(line) { None } else { Some(("WARN", line)) }
}
```

Tests:

```rust
#[cfg(test)]
mod tests {
    use super::parse_relay_line;
    use find_common::logging::is_ignored_with;

    fn no_patterns(msg: &str) -> bool { false }
    fn ignore_pdf(msg: &str) -> bool { msg.contains("pdf_extract: unknown glyph") }

    #[test]
    fn suppresses_matching_warn_line() {
        // This is the exact regression from a025efb/c029136
        let line = "WARN pdf_extract: unknown glyph name 'box3' for font ArialMT";
        assert!(parse_relay_line(line, ignore_pdf).is_none());
    }

    #[test]
    fn passes_non_matching_warn_line() {
        let line = "WARN find_server::worker: slow step took 1200ms";
        assert!(parse_relay_line(line, ignore_pdf).is_some());
    }

    #[test]
    fn parses_error_prefix() {
        let r = parse_relay_line("ERROR some::crate: bad thing", no_patterns).unwrap();
        assert_eq!(r.0, "ERROR");
        assert_eq!(r.1, "some::crate: bad thing");
    }

    #[test]
    fn empty_line_returns_none() {
        assert!(parse_relay_line("   ", no_patterns).is_none());
    }

    #[test]
    fn unknown_format_emitted_as_warn_if_not_ignored() {
        let r = parse_relay_line("bare message with no level", no_patterns).unwrap();
        assert_eq!(r.0, "WARN");
    }
}
```

Update `relay_subprocess_logs` to call `parse_relay_line` internally so tests
cover the actual production code path.

---

### Phase 1.3 — `crates/server/src/db.rs` pure helpers

Add `#[cfg(test)] mod tests` for the pure functions:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ── split_composite_path ────────────────────────────────────────────────

    #[test]
    fn split_no_separator() {
        let (outer, inner) = split_composite_path("docs/report.pdf");
        assert_eq!(outer, "docs/report.pdf");
        assert!(inner.is_none());
    }

    #[test]
    fn split_single_separator() {
        let (outer, inner) = split_composite_path("archive.zip::member.txt");
        assert_eq!(outer, "archive.zip");
        assert_eq!(inner.as_deref(), Some("member.txt"));
    }

    #[test]
    fn split_nested_separators_splits_at_first() {
        let (outer, inner) = split_composite_path("a.zip::b.zip::c.txt");
        assert_eq!(outer, "a.zip");
        assert_eq!(inner.as_deref(), Some("b.zip::c.txt"));
    }

    // ── prefix_bump ─────────────────────────────────────────────────────────

    #[test]
    fn prefix_bump_increments_last_byte() {
        assert_eq!(prefix_bump("foo/bar/"), "foo/bar0");
    }

    #[test]
    fn prefix_bump_empty_does_not_panic() {
        let _ = prefix_bump(""); // should return "" or handle gracefully
    }

    // ── build_fts_query ──────────────────────────────────────────────────────

    #[test]
    fn fts_query_phrase_wraps_in_quotes() {
        assert_eq!(build_fts_query("hello world", true).as_deref(), Some("\"hello world\""));
    }

    #[test]
    fn fts_query_phrase_short_returns_none() {
        assert!(build_fts_query("ab", true).is_none());
    }

    #[test]
    fn fts_query_fuzzy_joins_terms_with_and() {
        let q = build_fts_query("foo bar", false).unwrap();
        assert!(q.contains("foo"));
        assert!(q.contains("AND"));
        assert!(q.contains("bar"));
    }

    #[test]
    fn fts_query_fuzzy_filters_short_terms() {
        // All terms < 3 chars → None
        assert!(build_fts_query("to go", false).is_none());
    }

    #[test]
    fn fts_query_fuzzy_strips_fts5_special_chars() {
        let q = build_fts_query("test^query", false).unwrap();
        assert!(!q.contains('^'));
    }
}
```

---

### Phase 1.4 — `crates/server/src/worker.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use find_common::api::{IndexFile, IndexLine};

    fn make_file(path: &str, kind: &str) -> IndexFile {
        IndexFile {
            path: path.to_string(),
            mtime: 1000,
            size: 100,
            kind: kind.to_string(),
            lines: vec![IndexLine { archive_path: None, line_number: 0, content: path.to_string() }],
            extract_ms: None,
            content_hash: None,
        }
    }

    #[test]
    fn filename_only_file_converts_archive_kind_to_unknown() {
        // Must not record kind="archive" so the fallback doesn't re-trigger
        // the is_outer_archive delete path on the next process_file call.
        let f = make_file("data.zip", "archive");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, "unknown");
    }

    #[test]
    fn filename_only_file_keeps_non_archive_kind() {
        let f = make_file("notes.md", "text");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.kind, "text");
    }

    #[test]
    fn filename_only_file_has_single_path_line() {
        let f = make_file("docs/report.pdf", "pdf");
        let fallback = filename_only_file(&f);
        assert_eq!(fallback.lines.len(), 1);
        assert_eq!(fallback.lines[0].line_number, 0);
        assert_eq!(fallback.lines[0].content, "docs/report.pdf");
    }

    #[test]
    fn is_outer_archive_check() {
        // The production code checks: kind == "archive" && !path.contains("::")
        // Verify the logic is correct for each case.
        assert!(is_outer_archive_path("data.zip", "archive"));
        assert!(!is_outer_archive_path("data.zip::inner.txt", "archive"));
        assert!(!is_outer_archive_path("data.txt", "text"));
    }
}

// Helper (add to worker.rs as pub(crate) for testability)
pub(crate) fn is_outer_archive_path(path: &str, kind: &str) -> bool {
    kind == "archive" && !path.contains("::")
}
```

---

### Phase 2 — `crates/server/src/db_tests.rs`

Add `pub(crate) fn init_connection(conn: &Connection) -> Result<()>` to
`db.rs`, extracted from `open()`, and create `db_tests.rs`:

```rust
// crates/server/src/db_tests.rs

use rusqlite::Connection;
use crate::db;

fn open_mem() -> Connection {
    let conn = Connection::open(":memory:").unwrap();
    db::init_connection(&conn).unwrap();
    conn
}

fn insert_file(conn: &Connection, path: &str, kind: &str) {
    conn.execute(
        "INSERT INTO files (path, mtime, size, kind) VALUES (?1, 0, 0, ?2)",
        rusqlite::params![path, kind],
    ).unwrap();
}

fn insert_file_with_size(conn: &Connection, path: &str, kind: &str, size: i64) {
    conn.execute(
        "INSERT INTO files (path, mtime, size, kind) VALUES (?1, 0, ?3, ?2)",
        rusqlite::params![path, kind, size],
    ).unwrap();
}

// ── Alias column-index regression (795a13a) ─────────────────────────────────

#[test]
fn fetch_aliases_reads_path_column_not_id_column() {
    let conn = open_mem();
    // Insert canonical file
    conn.execute(
        "INSERT INTO files (path, mtime, size, kind) VALUES ('a.txt', 0, 0, 'text')",
        [],
    ).unwrap();
    let cid: i64 = conn.last_insert_rowid();
    // Insert alias
    conn.execute(
        "INSERT INTO files (path, mtime, size, kind, content_hash, canonical_file_id)
         VALUES ('b.txt', 0, 0, 'text', 'h1', ?1)",
        rusqlite::params![cid],
    ).unwrap();

    let map = db::fetch_aliases_for_canonical_ids(&conn, &[cid]).unwrap();
    let aliases = map.get(&cid).expect("must have alias entry");
    // Before the fix, this would have panicked or returned wrong data because
    // the query read column 0 (INTEGER canonical_file_id) as String.
    assert_eq!(aliases, &["b.txt"]);
}

// ── Extension stats SQL regression (a025efb) ────────────────────────────────

#[test]
fn get_stats_by_ext_returns_extension_counts() {
    let conn = open_mem();
    insert_file(&conn, "docs/report.pdf", "pdf");
    insert_file(&conn, "src/main.rs", "text");
    insert_file(&conn, "src/lib.rs", "text");

    let by_ext = db::get_stats_by_ext(&conn).unwrap();
    // Before the fix, REVERSE() caused this to return an empty vec.
    assert!(!by_ext.is_empty(), "extension stats must not be empty");
    let rs = by_ext.iter().find(|e| e.ext == "rs").expect("must have .rs");
    assert_eq!(rs.count, 2);
    let pdf = by_ext.iter().find(|e| e.ext == "pdf").expect("must have .pdf");
    assert_eq!(pdf.count, 1);
}

#[test]
fn get_stats_by_ext_excludes_archive_members() {
    let conn = open_mem();
    // Only archive member paths — should be excluded by WHERE NOT LIKE '%::%'
    insert_file(&conn, "outer.zip::inner.rs", "text");
    let by_ext = db::get_stats_by_ext(&conn).unwrap();
    assert!(by_ext.is_empty());
}

#[test]
fn get_stats_by_ext_excludes_files_without_extension() {
    let conn = open_mem();
    insert_file(&conn, "Makefile", "text");
    insert_file(&conn, "script.sh", "text");
    let by_ext = db::get_stats_by_ext(&conn).unwrap();
    // "sh" is present; "Makefile" (no extension) is absent
    assert!(by_ext.iter().any(|e| e.ext == "sh"));
    assert!(!by_ext.iter().any(|e| e.ext.is_empty()));
}

// ── Custom scalar functions ──────────────────────────────────────────────────

#[test]
fn scalar_file_ext_extracts_extension() {
    let conn = open_mem();
    let ext: String = conn.query_row("SELECT file_ext('report.pdf')", [], |r| r.get(0)).unwrap();
    assert_eq!(ext, "pdf");
}

#[test]
fn scalar_file_ext_lowercases() {
    let conn = open_mem();
    let ext: String = conn.query_row("SELECT file_ext('IMAGE.PNG')", [], |r| r.get(0)).unwrap();
    assert_eq!(ext, "png");
}

#[test]
fn scalar_file_ext_no_extension_returns_empty() {
    let conn = open_mem();
    let ext: String = conn.query_row("SELECT file_ext('Makefile')", [], |r| r.get(0)).unwrap();
    assert_eq!(ext, "");
}

#[test]
fn scalar_file_basename_strips_directory() {
    let conn = open_mem();
    let name: String = conn
        .query_row("SELECT file_basename('docs/sub/report.pdf')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "report.pdf");
}

#[test]
fn scalar_file_basename_no_slash_returns_name() {
    let conn = open_mem();
    let name: String = conn
        .query_row("SELECT file_basename('report.pdf')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "report.pdf");
}

// ── get_stats kind totals ────────────────────────────────────────────────────

#[test]
fn get_stats_totals_by_kind() {
    let conn = open_mem();
    insert_file_with_size(&conn, "a.pdf", "pdf", 1000);
    insert_file_with_size(&conn, "b.pdf", "pdf", 2000);
    insert_file_with_size(&conn, "c.rs", "text", 500);
    let (total_files, total_size, by_kind) = db::get_stats(&conn).unwrap();
    assert_eq!(total_files, 3);
    assert_eq!(total_size, 3500);
    let pdf = by_kind.get("pdf").expect("pdf kind");
    assert_eq!(pdf.count, 2);
    assert_eq!(pdf.size, 3000);
    let text = by_kind.get("text").expect("text kind");
    assert_eq!(text.count, 1);
    assert_eq!(text.size, 500);
}
```

---

### Phase 3 — Future (out of scope)

- End-to-end `process_file` tests with temp-dir ZIP archives
- HTTP integration tests using `axum::test` or `reqwest` against a
  bound-to-port server

## Files Changed

- `crates/common/src/logging.rs` — add `pub(crate) fn is_ignored_with()`; add `#[cfg(test)] mod tests`
- `crates/client/src/subprocess.rs` — extract `pub(crate) fn parse_relay_line()`; add `#[cfg(test)] mod tests`
- `crates/server/src/db.rs` — add `pub(crate) fn init_connection()`; make `build_fts_query`, `split_composite_path`, `prefix_bump` `pub(crate)`; add `pub(crate) fn fetch_aliases_for_canonical_ids` (already needed externally, but confirm visibility); add `#[cfg(test)] mod tests` for pure helpers; add `#[cfg(test)] mod db_tests;` import
- `crates/server/src/db_tests.rs` — new file with all Phase 2 DB tests
- `crates/server/src/worker.rs` — add `pub(crate) fn is_outer_archive_path()`; add `#[cfg(test)] mod tests`

## Testing

```sh
# Run all tests
cargo test --workspace

# Run only the new regression-guard tests
cargo test -p find-common logging
cargo test -p find-client subprocess
cargo test -p find-server db

# Verify no clippy regressions
mise run clippy
```

### Regression validation checklist

After implementation, confirm each known bug would have been caught:

- [ ] **Alias column read (795a13a):** `fetch_aliases_reads_path_column_not_id_column`
      fails when `row.get(0)` is used instead of `row.get(1)`
- [ ] **Extension stats REVERSE() (a025efb):** `get_stats_by_ext_returns_extension_counts`
      returns empty vec when `REVERSE()` is used in SQL (no scalar functions registered)
- [ ] **Subprocess log bypass (c029136):** `suppresses_matching_warn_line`
      fails when `parse_relay_line` calls `warn!()` without checking `is_ignored()`

## Breaking Changes

None. All changes are additive. No public API surface changes.
