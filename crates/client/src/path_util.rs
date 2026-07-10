/// On Windows, replace backslash separators with forward slashes so that
/// paths are stored consistently regardless of platform. On Unix, backslash
/// is a valid filename character and must not be replaced.
///
/// This function must not mangle `::` composite paths — callers that deal
/// with archive member paths (`outer.zip::inner.txt`) rely on the `::` token
/// being preserved verbatim.
#[cfg(windows)]
pub fn normalise_path_sep(s: &str) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
pub fn normalise_path_sep(s: &str) -> String {
    s.to_string()
}

/// On Windows, normalise a bare drive letter like `"C:"` to `"C:/"` so that
/// `WalkDir` walks the drive root (not the drive's current directory) and
/// `strip_prefix` returns clean relative paths without a leading separator.
/// On non-Windows this is a no-op.
#[cfg(windows)]
pub fn normalise_root(s: &str) -> String {
    if s.len() == 2 && s.as_bytes()[1] == b':' {
        format!("{s}/")
    } else {
        s.to_string()
    }
}

#[cfg(not(windows))]
pub fn normalise_root(s: &str) -> String {
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_sep_unix_no_change() {
        assert_eq!(normalise_path_sep("foo/bar/baz.txt"), "foo/bar/baz.txt");
    }

    #[test]
    fn path_sep_forward_slashes_unchanged() {
        assert_eq!(normalise_path_sep("a/b/c"), "a/b/c");
    }

    #[test]
    fn composite_path_not_mangled() {
        // The :: separator must survive normalise_path_sep unchanged.
        let composite = "archive.zip::inner/file.txt";
        assert_eq!(normalise_path_sep(composite), composite);
    }

    #[cfg(not(windows))]
    #[test]
    fn normalise_root_unix_no_change() {
        assert_eq!(normalise_root("/home/user/docs"), "/home/user/docs");
        assert_eq!(normalise_root("/"), "/");
    }

    #[cfg(windows)]
    #[test]
    fn normalise_root_bare_drive_letter() {
        assert_eq!(normalise_root("C:"), "C:/");
        assert_eq!(normalise_root("D:"), "D:/");
    }

    #[cfg(windows)]
    #[test]
    fn normalise_root_already_has_slash() {
        assert_eq!(normalise_root("C:/"), "C:/");
        assert_eq!(normalise_root("C:/Users"), "C:/Users");
    }

    #[cfg(windows)]
    #[test]
    fn normalise_root_unc_path_unchanged() {
        assert_eq!(normalise_root(r"\\server\share"), r"\\server\share");
    }

    #[cfg(windows)]
    #[test]
    fn path_sep_backslash_replaced() {
        assert_eq!(normalise_path_sep(r"foo\bar\baz.txt"), "foo/bar/baz.txt");
    }

    #[cfg(windows)]
    #[test]
    fn path_sep_mixed_separators() {
        assert_eq!(normalise_path_sep(r"foo\bar/baz\qux"), "foo/bar/baz/qux");
    }
}

/// Given a list of include glob patterns, return the set of **terminal**
/// directory prefixes — the deepest safe literal directory path before any
/// wildcard character in each pattern.
///
/// Returns `None` if no useful pruning can be determined (e.g. `**/*.rs`
/// requires traversing everything).  When `Some`, only directories that are
/// a terminal, an ancestor of a terminal, or inside a terminal need to be
/// visited.
pub fn include_dir_prefixes(patterns: &[String]) -> Option<std::collections::HashSet<String>> {
    let mut terminals = std::collections::HashSet::new();
    for pat in patterns {
        let pat = pat.replace('\\', "/");

        // Negation patterns — fall back to no pruning.
        if pat.starts_with('!') {
            return None;
        }

        // Find the first wildcard. Include `{` for alternations like {a,b}/**.
        let wildcard_pos = pat.find(['*', '?', '[', '{']);

        // Determine the safe literal directory prefix: everything before the
        // last `/` that precedes the first wildcard. This prevents cutting a
        // directory component in half (e.g. `Users/Administrat?r` → `Users`).
        let literal = match wildcard_pos {
            None => pat.as_str(),       // no wildcard — whole pattern is literal
            Some(0) => return None,     // wildcard at root — can't prune anything
            Some(i) => {
                let before = &pat[..i];
                let slash = before.rfind('/')?; // wildcard in the first component — can't prune
                &pat[..slash]
            }
        };

        let literal = literal.trim_end_matches('/');
        if literal.is_empty() {
            return None;
        }

        terminals.insert(literal.to_string());
    }
    Some(terminals)
}

/// Resolve the effective scan target for a `find-scan <PATH>` argument.
///
/// A `.index`/`.noindex` control file changes what's indexed in its own
/// directory (an include filter or exclusion marker), so a scan targeting it
/// must rescan an ancestor directory rather than indexing the control file's
/// own content. `is_file` must reflect whether `abs` currently exists as a
/// regular file (checked by the caller, since this function does no I/O).
///
/// The scan target is the *grandparent* of the control file — i.e. the parent
/// of the directory the control file lives in — not that directory itself.
/// `.noindex`-presence and `.index`-override loading are only evaluated by the
/// walker for non-root entries (the walk root is always traversed
/// unconditionally, so its own control files never apply to itself). Starting
/// one level higher makes the control file's own directory a normal, prunable
/// entry, so a newly added/changed/removed `.noindex` or `.index` there is
/// actually re-applied.
///
/// Returns `(scan_target, was_control_file)`. Falls back to the control
/// file's own directory when that directory has no parent (e.g. it's a
/// filesystem root) — an unreachable scenario in practice, since source paths
/// are never a bare root.
pub fn resolve_scan_target(
    abs: &std::path::Path,
    is_file: bool,
    index_file: &str,
    noindex_file: &str,
) -> (std::path::PathBuf, bool) {
    if is_file {
        let is_control = abs
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == index_file || n == noindex_file);
        if is_control {
            if let Some(control_dir) = abs.parent() {
                let target = control_dir.parent().unwrap_or(control_dir);
                return (target.to_path_buf(), true);
            }
        }
    }
    (abs.to_path_buf(), false)
}

#[cfg(test)]
mod resolve_scan_target_tests {
    use super::resolve_scan_target;
    use std::path::PathBuf;

    #[test]
    fn noindex_file_resolves_to_grandparent() {
        // /src/sub/.noindex affects whether *sub* is descended into, which is
        // only evaluated when sub's *parent* (/src) walks it as a normal entry.
        let (target, was_control) =
            resolve_scan_target(&PathBuf::from("/src/sub/.noindex"), true, ".index", ".noindex");
        assert_eq!(target, PathBuf::from("/src"));
        assert!(was_control);
    }

    #[test]
    fn index_file_resolves_to_grandparent() {
        let (target, was_control) =
            resolve_scan_target(&PathBuf::from("/src/sub/.index"), true, ".index", ".noindex");
        assert_eq!(target, PathBuf::from("/src"));
        assert!(was_control);
    }

    #[test]
    fn custom_control_filenames_are_respected() {
        let (target, was_control) =
            resolve_scan_target(&PathBuf::from("/src/sub/SKIP"), true, "SETTINGS", "SKIP");
        assert_eq!(target, PathBuf::from("/src"));
        assert!(was_control);
    }

    #[test]
    fn control_file_at_filesystem_root_falls_back_to_its_own_dir() {
        // /.noindex: its directory is "/", which has no parent to walk from —
        // fall back to scanning "/" itself (the pre-existing, imperfect
        // depth-0 behaviour) rather than erroring or escaping above the root.
        let (target, was_control) =
            resolve_scan_target(&PathBuf::from("/.noindex"), true, ".index", ".noindex");
        assert_eq!(target, PathBuf::from("/"));
        assert!(was_control);
    }

    #[test]
    fn one_level_below_root_resolves_to_root() {
        let (target, was_control) =
            resolve_scan_target(&PathBuf::from("/src/.noindex"), true, ".index", ".noindex");
        assert_eq!(target, PathBuf::from("/"));
        assert!(was_control);
    }

    #[test]
    fn normal_file_is_unchanged() {
        let (target, was_control) =
            resolve_scan_target(&PathBuf::from("/src/sub/notes.txt"), true, ".index", ".noindex");
        assert_eq!(target, PathBuf::from("/src/sub/notes.txt"));
        assert!(!was_control);
    }

    #[test]
    fn directory_argument_is_unchanged() {
        // is_file = false, even though the name coincidentally matches — a
        // directory named ".noindex" is not a control file.
        let (target, was_control) =
            resolve_scan_target(&PathBuf::from("/src/.noindex"), false, ".index", ".noindex");
        assert_eq!(target, PathBuf::from("/src/.noindex"));
        assert!(!was_control);
    }
}
