/// Helpers for composite archive-member paths.
///
/// Archive members are stored under a composite path using `::` as separator:
///   `taxes/w2.zip::wages.pdf`           — one level
///   `data.tar.gz::report.txt::inner.zip::file.txt`  — nested
///
/// Rules:
/// - `::` is reserved; it cannot appear in regular file paths.
/// - The outer file is everything before the first `::`.
/// - `is_composite` is the correct way to distinguish outer files from members.
/// - New SQL for members should prefer `composite_range_bounds(outer)` (a
///   `path >= lo AND path < hi` range) over `composite_like_prefix(outer)`
///   (`path LIKE 'outer::%'`). On `files.path` (default BINARY collation),
///   SQLite disables its LIKE-to-index-range optimization whenever
///   `case_sensitive_like` is off (the default) — so `LIKE` here always forces
///   a full table scan, verified taking ~50s on a ~600K-row table where the
///   equivalent range scan takes single-digit milliseconds. `composite_like_prefix`
///   is kept for existing call sites and contexts where a literal LIKE pattern
///   is actually wanted (e.g. building a display string), not for query performance.
const SEP: &str = "::";

/// Return `true` if `path` is an archive-member path (contains `::`)
#[inline]
pub fn is_composite(path: &str) -> bool {
    path.contains(SEP)
}

/// Return the outer archive path (everything before the first `::`, or the
/// whole path if there is no `::`)
#[inline]
pub fn composite_outer(path: &str) -> &str {
    match path.find(SEP) {
        Some(pos) => &path[..pos],
        None => path,
    }
}

/// Return the member portion (everything after the first `::`) or `None` if
/// `path` is not composite.
#[inline]
pub fn composite_member(path: &str) -> Option<&str> {
    path.find(SEP).map(|pos| &path[pos + SEP.len()..])
}

/// Split a composite path into `(outer, member)`.  Returns `None` if `path` is
/// not composite.
#[inline]
pub fn split_composite(path: &str) -> Option<(&str, &str)> {
    path.find(SEP).map(|pos| (&path[..pos], &path[pos + SEP.len()..]))
}

/// Join an outer archive path and a member name into a composite path.
#[inline]
pub fn make_composite(outer: &str, member: &str) -> String {
    format!("{outer}{SEP}{member}")
}

/// Build the SQL `LIKE` prefix used to match all members of `outer_path`.
/// Usage: `WHERE path LIKE ?`, binding `composite_like_prefix(outer)`.
///
/// Prefer `composite_range_bounds` for queries — see the module doc comment.
#[inline]
pub fn composite_like_prefix(outer_path: &str) -> String {
    format!("{outer_path}{SEP}%")
}

/// Build an index-friendly `(lo, hi)` byte range matching all members of
/// `outer_path`, for `WHERE path >= lo AND path < hi`.
///
/// Equivalent to `path LIKE 'outer_path::%'` but usable by SQLite's index
/// range scan / `MULTI-INDEX OR` optimizer — see the module doc comment for
/// why the LIKE form can't be. `hi` is `lo` with its last byte incremented,
/// which is safe here because `lo` always ends in the second `:` of `SEP`
/// (0x3A), never 0xFF.
#[inline]
pub fn composite_range_bounds(outer_path: &str) -> (String, String) {
    let lo = format!("{outer_path}{SEP}");
    let mut hi_bytes = lo.as_bytes().to_vec();
    // Safe: `lo` always ends in ':' (0x3A), which has room to increment.
    *hi_bytes.last_mut().unwrap() += 1;
    let hi = String::from_utf8(hi_bytes).expect("incrementing an ASCII ':' byte stays valid UTF-8");
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path() {
        assert!(!is_composite("docs/readme.txt"));
        assert_eq!(composite_outer("docs/readme.txt"), "docs/readme.txt");
        assert_eq!(composite_member("docs/readme.txt"), None);
        assert_eq!(split_composite("docs/readme.txt"), None);
    }

    #[test]
    fn single_level() {
        let p = "archive.zip::member.txt";
        assert!(is_composite(p));
        assert_eq!(composite_outer(p), "archive.zip");
        assert_eq!(composite_member(p), Some("member.txt"));
        assert_eq!(split_composite(p), Some(("archive.zip", "member.txt")));
    }

    #[test]
    fn nested() {
        let p = "outer.tar.gz::inner.zip::file.txt";
        assert!(is_composite(p));
        assert_eq!(composite_outer(p), "outer.tar.gz");
        assert_eq!(composite_member(p), Some("inner.zip::file.txt"));
        assert_eq!(split_composite(p), Some(("outer.tar.gz", "inner.zip::file.txt")));
    }

    #[test]
    fn make_composite_roundtrip() {
        let c = make_composite("a.zip", "b.txt");
        assert_eq!(c, "a.zip::b.txt");
        assert_eq!(split_composite(&c), Some(("a.zip", "b.txt")));
    }

    #[test]
    fn like_prefix() {
        assert_eq!(composite_like_prefix("taxes/w2.zip"), "taxes/w2.zip::%");
    }

    #[test]
    fn range_bounds_matches_like_prefix_semantics() {
        let (lo, hi) = composite_range_bounds("taxes/w2.zip");
        assert_eq!(lo, "taxes/w2.zip::");
        assert_eq!(hi, "taxes/w2.zip:;"); // second ':' (0x3A) bumped to ';' (0x3B)

        // Anything that would match `LIKE 'taxes/w2.zip::%'` falls in [lo, hi).
        let lo = lo.as_str();
        let hi = hi.as_str();
        assert!("taxes/w2.zip::wages.pdf" >= lo && "taxes/w2.zip::wages.pdf" < hi);
        assert!("taxes/w2.zip::inner.zip::x.txt" >= lo && "taxes/w2.zip::inner.zip::x.txt" < hi);

        // The outer path itself and unrelated paths must NOT fall in range.
        assert!(!("taxes/w2.zip" >= lo && "taxes/w2.zip" < hi));
        assert!(!("taxes/w2.zipper" >= lo && "taxes/w2.zipper" < hi));
    }
}
