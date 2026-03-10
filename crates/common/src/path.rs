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
/// - All SQL `LIKE` predicates for members use `composite_like_prefix(outer)`.
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
#[inline]
pub fn composite_like_prefix(outer_path: &str) -> String {
    format!("{outer_path}{SEP}%")
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
}
