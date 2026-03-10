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
