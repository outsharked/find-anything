use anyhow::Result;
use rusqlite::{Connection, params};

use find_common::api::DirEntry;

// ── Directory listing ─────────────────────────────────────────────────────────

/// List the immediate children (dirs + files) of `prefix` within the source.
///
/// `prefix` should end with `/` for non-root directory queries (e.g. `"src/"`).
/// For archive member listings, `prefix` ends with `"::"` (e.g. `"archive.zip::"`).
/// An empty string means the root of the source.
pub fn list_dir(conn: &Connection, prefix: &str) -> Result<Vec<DirEntry>> {
    let is_archive_listing = prefix.contains("::");

    let (low, high) = if prefix.is_empty() {
        (String::new(), "\u{FFFF}".to_string())
    } else {
        (prefix.to_string(), prefix_bump(prefix))
    };

    let mut stmt = conn.prepare(
        "SELECT path, kind, size, mtime FROM files WHERE path >= ?1 AND path < ?2 ORDER BY path",
    )?;

    let rows: Vec<(String, String, Option<i64>, i64)> = stmt
        .query_map(params![low, high], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dirs: Vec<DirEntry> = Vec::new();
    let mut files: Vec<DirEntry> = Vec::new();

    // First pass: collect all actual files to avoid creating duplicate virtual dirs
    if is_archive_listing {
        for (path, _, _, _) in &rows {
            let rest = path.strip_prefix(prefix).unwrap_or(path);
            if !rest.contains("::") && !rest.contains('/') {
                seen_files.insert(rest.to_string());
            }
        }
    }

    // Second pass: build the directory listing
    for (path, kind, size, mtime) in rows {
        let rest = path.strip_prefix(prefix).unwrap_or(&path);

        if is_archive_listing {
            // Inside an archive: split at whichever separator comes first —
            // "/" (subdirectory) or "::" (nested archive). Taking the wrong one
            // first (e.g. "::" in "docs/inner.zip::file.txt") would produce a
            // child name containing a slash, breaking the tree and causing the
            // UI to recurse infinitely.
            let colon_pos = rest.find("::");
            let slash_pos = rest.find('/');
            let sep_pos = match (colon_pos, slash_pos) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let Some(pos) = sep_pos {
                let child_name = &rest[..pos];
                // Only create virtual dir if we haven't seen a real file with this path
                if !seen_files.contains(child_name) && seen_dirs.insert(child_name.to_string()) {
                    if colon_pos == Some(pos) {
                        // The separator is "::" — child_name is a nested archive.
                        // Return it as a file with kind="archive" so the UI calls
                        // listArchiveMembers (appends "::") rather than listDir
                        // (appends "/", which finds nothing since members use "::").
                        files.push(DirEntry {
                            name: child_name.to_string(),
                            path: format!("{}{}", prefix, child_name),
                            entry_type: "file".to_string(),
                            kind: Some("archive".to_string()),
                            size: None,
                            mtime: None,
                        });
                    } else {
                        // The separator is "/" — child_name is a subdirectory.
                        // Append "/" so the next listDir call gets a properly-terminated
                        // prefix. Without this, strip_prefix() leaves a leading "/" in
                        // `rest`, sep_pos hits position 0, child_name is empty, and the
                        // same path is returned forever → infinite UI recursion.
                        dirs.push(DirEntry {
                            name: child_name.to_string(),
                            path: format!("{}{}/", prefix, child_name),
                            entry_type: "dir".to_string(),
                            kind: None,
                            size: None,
                            mtime: None,
                        });
                    }
                }
            } else {
                // Leaf member within the archive.
                files.push(DirEntry {
                    name: rest.to_string(),
                    path,
                    entry_type: "file".to_string(),
                    kind: Some(kind),
                    size,
                    mtime: Some(mtime),
                });
            }
        } else {
            // Regular directory listing.
            // Skip inner archive members (composite paths) — they appear only when
            // the user explicitly expands the archive.
            if rest.contains("::") {
                continue;
            }

            if let Some(slash_pos) = rest.find('/') {
                let dir_name = &rest[..slash_pos];
                if seen_dirs.insert(dir_name.to_string()) {
                    dirs.push(DirEntry {
                        name: dir_name.to_string(),
                        path: format!("{}{}/", prefix, dir_name),
                        entry_type: "dir".to_string(),
                        kind: None,
                        size: None,
                        mtime: None,
                    });
                }
            } else {
                files.push(DirEntry {
                    name: rest.to_string(),
                    path,
                    entry_type: "file".to_string(),
                    kind: Some(kind),
                    size,
                    mtime: Some(mtime),
                });
            }
        }
    }

    let mut entries = dirs;
    entries.extend(files);
    Ok(entries)
}

/// Produce the upper-bound key for a prefix range scan by incrementing the last byte.
pub fn prefix_bump(prefix: &str) -> String {
    let mut bytes = prefix.as_bytes().to_vec();
    if let Some(last) = bytes.last_mut() {
        *last += 1;
    }
    String::from_utf8(bytes).unwrap_or_else(|_| "\u{FFFF}".to_string())
}

/// Split a potentially composite path ("zip::member") into (outer_path, archive_path).
/// Returns (path, None) for non-composite paths.
pub fn split_composite_path(path: &str) -> (String, Option<String>) {
    if let Some(pos) = path.find("::") {
        (path[..pos].to_string(), Some(path[pos + 2..].to_string()))
    } else {
        (path.to_string(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── split_composite_path ─────────────────────────────────────────────────

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

    #[test]
    fn split_empty_string() {
        let (outer, inner) = split_composite_path("");
        assert_eq!(outer, "");
        assert!(inner.is_none());
    }

    // ── prefix_bump ──────────────────────────────────────────────────────────

    #[test]
    fn prefix_bump_increments_last_byte() {
        assert_eq!(prefix_bump("foo/bar/"), "foo/bar0");
    }

    #[test]
    fn prefix_bump_empty_string_returns_empty() {
        assert_eq!(prefix_bump(""), "");
    }

    // ── list_dir ─────────────────────────────────────────────────────────────
    //
    // These tests use an in-memory SQLite database with just the `files` table
    // so they run without touching the filesystem.

    fn test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (
                id    INTEGER PRIMARY KEY AUTOINCREMENT,
                path  TEXT    NOT NULL UNIQUE,
                mtime INTEGER NOT NULL,
                size  INTEGER,
                kind  TEXT    NOT NULL DEFAULT 'text'
            );",
        )
        .unwrap();
        conn
    }

    fn ins(conn: &rusqlite::Connection, path: &str, kind: &str) {
        conn.execute(
            "INSERT INTO files (path, mtime, size, kind) VALUES (?1, 0, 0, ?2)",
            rusqlite::params![path, kind],
        )
        .unwrap();
    }

    fn ins_no_size(conn: &rusqlite::Connection, path: &str, kind: &str) {
        conn.execute(
            "INSERT INTO files (path, mtime, size, kind) VALUES (?1, 0, NULL, ?2)",
            rusqlite::params![path, kind],
        )
        .unwrap();
    }

    fn names(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    // ── root listing ─────────────────────────────────────────────────────────

    #[test]
    fn list_dir_root_flat_files() {
        let conn = test_db();
        ins(&conn, "README.md", "text");
        ins(&conn, "main.rs", "text");
        let entries = list_dir(&conn, "").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.entry_type == "file"));
    }

    #[test]
    fn list_dir_root_shows_subdirectory() {
        let conn = test_db();
        ins(&conn, "src/main.rs", "text");
        ins(&conn, "src/lib.rs", "text");
        let entries = list_dir(&conn, "").unwrap();
        assert_eq!(entries.len(), 1);
        let dir = &entries[0];
        assert_eq!(dir.entry_type, "dir");
        assert_eq!(dir.name, "src");
        assert_eq!(dir.path, "src/");
    }

    #[test]
    fn list_dir_root_skips_archive_members() {
        // Composite paths must not appear in the root listing — they're only
        // visible when the outer archive is explicitly expanded.
        let conn = test_db();
        ins(&conn, "archive.zip", "archive");
        ins(&conn, "archive.zip::member.txt", "text");
        let entries = list_dir(&conn, "").unwrap();
        // Only the outer archive, not the member.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "archive.zip");
        assert_eq!(entries[0].kind.as_deref(), Some("archive"));
    }

    #[test]
    fn list_dir_dirs_sorted_before_files() {
        let conn = test_db();
        ins(&conn, "README.md", "text");
        ins(&conn, "src/main.rs", "text");
        let entries = list_dir(&conn, "").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entry_type, "dir");
        assert_eq!(entries[1].entry_type, "file");
    }

    // ── subdirectory listing ──────────────────────────────────────────────────

    #[test]
    fn list_dir_subdir() {
        let conn = test_db();
        ins(&conn, "src/main.rs", "text");
        ins(&conn, "src/lib.rs", "text");
        let entries = list_dir(&conn, "src/").unwrap();
        let mut ns = names(&entries);
        ns.sort_unstable();
        assert_eq!(ns, ["lib.rs", "main.rs"]);
        assert!(entries.iter().all(|e| e.entry_type == "file"));
    }

    // ── archive member listing ────────────────────────────────────────────────

    #[test]
    fn list_dir_archive_flat_members() {
        let conn = test_db();
        ins(&conn, "data.zip", "archive");
        ins(&conn, "data.zip::a.txt", "text");
        ins(&conn, "data.zip::b.txt", "text");
        let entries = list_dir(&conn, "data.zip::").unwrap();
        let mut ns = names(&entries);
        ns.sort_unstable();
        assert_eq!(ns, ["a.txt", "b.txt"]);
        assert!(entries.iter().all(|e| e.entry_type == "file"));
    }

    #[test]
    fn list_dir_archive_with_inner_subdir() {
        let conn = test_db();
        ins(&conn, "data.zip::docs/readme.txt", "text");
        ins(&conn, "data.zip::src/main.rs", "text");
        let entries = list_dir(&conn, "data.zip::").unwrap();
        // Both children are virtual dirs.
        assert!(entries.iter().all(|e| e.entry_type == "dir"));
        let mut ns = names(&entries);
        ns.sort_unstable();
        assert_eq!(ns, ["docs", "src"]);
        // Paths must end with "/" so the next listDir call gets the right prefix.
        assert!(entries.iter().all(|e| e.path.ends_with('/')));
    }

    #[test]
    fn list_dir_archive_subdir_listing() {
        let conn = test_db();
        ins(&conn, "data.zip::docs/a.txt", "text");
        ins(&conn, "data.zip::docs/b.txt", "text");
        let entries = list_dir(&conn, "data.zip::docs/").unwrap();
        let mut ns = names(&entries);
        ns.sort_unstable();
        assert_eq!(ns, ["a.txt", "b.txt"]);
    }

    // ── nested archive (regression for the "inner zip shows Empty" bug) ───────

    /// When an archive member is itself an archive, listing the outer archive
    /// must return it as entry_type="file" with kind="archive" (not as a "dir"
    /// with a trailing "/"). The UI uses kind="archive" to call
    /// listArchiveMembers (which appends "::") instead of listDir (which appends
    /// "/", finding nothing because members use "::" not "/").
    #[test]
    fn list_dir_nested_archive_returned_as_archive_file() {
        let conn = test_db();
        // Only the members are indexed; the inner zip itself has no row.
        ins(&conn, "outer.zip::inner.zip::data.txt", "text");
        ins(&conn, "outer.zip::inner.zip::notes.txt", "text");

        let entries = list_dir(&conn, "outer.zip::").unwrap();
        assert_eq!(entries.len(), 1, "expected exactly one child");

        let inner = &entries[0];
        assert_eq!(inner.name, "inner.zip");
        assert_eq!(inner.entry_type, "file",
            "nested archive must be entry_type='file', not 'dir'");
        assert_eq!(inner.kind.as_deref(), Some("archive"),
            "nested archive must have kind='archive' so the UI calls listArchiveMembers");
        assert_eq!(inner.path, "outer.zip::inner.zip",
            "path must not have a trailing '/' or '::'");
    }

    /// Drilling into the nested archive must return its members.
    #[test]
    fn list_dir_nested_archive_members() {
        let conn = test_db();
        ins(&conn, "outer.zip::inner.zip::data.txt", "text");
        ins(&conn, "outer.zip::inner.zip::notes.txt", "text");

        // The UI calls listArchiveMembers(source, "outer.zip::inner.zip")
        // which translates to listDir(source, "outer.zip::inner.zip::").
        let entries = list_dir(&conn, "outer.zip::inner.zip::").unwrap();
        let mut ns = names(&entries);
        ns.sort_unstable();
        assert_eq!(ns, ["data.txt", "notes.txt"]);
        assert!(entries.iter().all(|e| e.entry_type == "file"));
    }

    /// If the inner archive IS explicitly indexed as a row (kind="archive"),
    /// it should still appear correctly via the real-file path in the DB.
    #[test]
    fn list_dir_nested_archive_with_explicit_row() {
        let conn = test_db();
        ins(&conn, "outer.zip::inner.zip", "archive");
        ins(&conn, "outer.zip::inner.zip::data.txt", "text");

        let entries = list_dir(&conn, "outer.zip::").unwrap();
        assert_eq!(entries.len(), 1);
        let inner = &entries[0];
        assert_eq!(inner.name, "inner.zip");
        // Whether the row comes from the explicit entry or the virtual inference,
        // the result must be kind="archive" and no trailing "/" in the path.
        assert_eq!(inner.kind.as_deref(), Some("archive"));
        assert!(!inner.path.ends_with('/'), "path must not end with '/'");
        assert!(!inner.path.ends_with("::"), "path must not end with '::'");
    }

    /// Three levels of nesting: outer.zip :: middle.zip :: inner.zip :: file.txt
    #[test]
    fn list_dir_triple_nested_archive() {
        let conn = test_db();
        ins(&conn, "outer.zip::middle.zip::inner.zip::file.txt", "text");

        // Listing outer.zip should show middle.zip as kind="archive".
        let entries = list_dir(&conn, "outer.zip::").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "middle.zip");
        assert_eq!(entries[0].kind.as_deref(), Some("archive"));
        assert_eq!(entries[0].path, "outer.zip::middle.zip");

        // Listing middle.zip should show inner.zip as kind="archive".
        let entries = list_dir(&conn, "outer.zip::middle.zip::").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "inner.zip");
        assert_eq!(entries[0].kind.as_deref(), Some("archive"));
        assert_eq!(entries[0].path, "outer.zip::middle.zip::inner.zip");

        // Listing inner.zip should show the file.
        let entries = list_dir(&conn, "outer.zip::middle.zip::inner.zip::").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "file.txt");
        assert_eq!(entries[0].entry_type, "file");
    }

    /// `size` can be NULL in the DB (unknown size for archive members).
    /// list_dir must not crash and must return size=None for those entries.
    #[test]
    fn list_dir_null_size_does_not_crash() {
        let conn = test_db();
        // Archive members may have NULL size when the extractor couldn't
        // determine it (e.g. streaming archives, certain zip variants).
        ins_no_size(&conn, "archive.zip::readme.txt", "text");
        ins_no_size(&conn, "archive.zip::data.csv", "text");
        let entries = list_dir(&conn, "archive.zip::").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().all(|e| e.size.is_none()),
            "NULL size in DB must map to size=None in DirEntry, not crash"
        );
    }

    /// A nested archive next to regular files and subdirs in an outer archive.
    #[test]
    fn list_dir_mixed_archive_contents() {
        let conn = test_db();
        ins(&conn, "outer.zip::plain.txt", "text");
        ins(&conn, "outer.zip::subdir/readme.md", "text");
        ins(&conn, "outer.zip::nested.zip::inside.txt", "text");

        let entries = list_dir(&conn, "outer.zip::").unwrap();
        // Dirs first, then files.
        let dirs: Vec<_> = entries.iter().filter(|e| e.entry_type == "dir").collect();
        let files: Vec<_> = entries.iter().filter(|e| e.entry_type == "file").collect();

        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].name, "subdir");
        assert!(dirs[0].path.ends_with('/'));

        assert_eq!(files.len(), 2);
        let nested = files.iter().find(|e| e.name == "nested.zip").unwrap();
        assert_eq!(nested.kind.as_deref(), Some("archive"));
        assert_eq!(nested.path, "outer.zip::nested.zip");

        let plain = files.iter().find(|e| e.name == "plain.txt").unwrap();
        assert_eq!(plain.entry_type, "file");
    }
}
