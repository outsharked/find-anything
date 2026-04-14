use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::GlobSet;
use walkdir::WalkDir;

use find_common::config::{load_dir_override, ScanConfig};
pub(crate) use find_common::build_globset;

use crate::path_util::{include_dir_prefixes, normalise_path_sep};

/// A single item yielded to the callback by [`walk_source_tree`].
// Each binary uses only one variant (Dir for find-watch, File for find-scan).
#[allow(dead_code)]
pub(crate) enum WalkItem {
    /// A directory that passed all walk-level filters.
    Dir(PathBuf),
    /// A file that passed all walk-level filters.
    ///
    /// Hidden file filtering, `.index` control-file skipping, and
    /// source-level include globs are NOT applied here — callers handle
    /// those in the callback.
    File {
        abs: PathBuf,
        /// Path relative to `strip_root`, forward-slash normalised.
        rel: String,
        /// Depth from `walk_root` (1 = immediate child of walk_root).
        depth: usize,
        /// File name component (UTF-8 decoded, empty on decode failure).
        name: String,
    },
}

/// Walk `walk_root` applying the filtering rules shared by `find-scan` and
/// `find-watch`, invoking `callback` for every directory and file that passes.
///
/// **Parameters**
/// * `walk_root`  — where `WalkDir` starts.  May differ from `strip_root`
///   when scanning a specific subdirectory while keeping paths relative to
///   the source root (e.g. `walk_root = /home/user/code/subdir`,
///   `strip_root = /home/user/code`).
/// * `strip_root` — base used for computing relative paths for glob matching
///   and the `rel` field in `WalkItem::File`.  Usually equal to `walk_root`;
///   set to the source root when a subdir is provided.
/// * `scan`       — effective `ScanConfig`; controls `follow_symlinks`,
///   `include_hidden`, and `noindex_file`.
/// * `excludes`   — compiled globset of `scan.exclude` patterns, relative
///   to `strip_root`.
/// * `terminals`  — from [`crate::path_util::include_dir_prefixes`]; prunes
///   directories that cannot contain any matching files.  `None` means
///   traverse everything (e.g. patterns like `**/*.rs`).
///
/// Per-directory `.index` files with `include` patterns are also respected
/// during traversal: when a directory contains a `.index` with an `include`
/// field, only sub-paths that match those patterns are descended into.  This
/// ensures `find-scan` and `find-watch` share identical directory-pruning
/// behaviour.
/// * `callback`   — receives each `WalkItem` that passes all filters.
///
/// Walk errors are logged at `warn`/`debug` level and skipped — the walk
/// always continues past inaccessible or excluded paths.
pub(crate) fn walk_source_tree(
    walk_root: &Path,
    strip_root: &Path,
    scan: &ScanConfig,
    excludes: &GlobSet,
    terminals: Option<&HashSet<String>>,
    mut callback: impl FnMut(WalkItem),
) {
    // Stack of (depth, terminals) from .index include overrides encountered
    // during the DFS.  DFS order ensures that when we move from a subtree to
    // a sibling, the sibling's shallower-or-equal depth triggers a pop of the
    // previous subtree's entries.  All active entries must allow a directory
    // before we descend into it.
    let mut override_stack: Vec<(usize, HashSet<String>)> = Vec::new();

    // Device ID of the walk root, captured once for filesystem-boundary checks.
    // None when cross_filesystems = true (check disabled) or on non-Unix.
    let root_dev: Option<u64> = if !scan.cross_filesystems {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(walk_root).ok().map(|m| m.dev())
        }
        #[cfg(not(unix))]
        { None }
    } else {
        None
    };

    for entry in WalkDir::new(walk_root)
        .follow_links(scan.follow_symlinks)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if e.file_type().is_dir() {
                let depth = e.depth();

                // Pop overrides from completed subtrees: any entry at depth >=
                // current was pushed by a sibling (or its descendant) and is
                // no longer an ancestor of this directory.
                override_stack.retain(|(d, _)| *d < depth);

                // Skip hidden directories when include_hidden is false.
                // Hidden files are intentionally left for the callback so
                // that control files (.index) remain visible regardless of
                // the setting.
                if !scan.include_hidden {
                    let name = e.file_name().to_str().unwrap_or("");
                    if name.starts_with('.') {
                        return false;
                    }
                }
                // Don't descend into directories containing a .noindex marker.
                if e.path().join(&scan.noindex_file).exists() {
                    tracing::debug!("walk: skipping {} (.noindex present)", e.path().display());
                    return false;
                }

                // Filesystem boundary check: skip directories on a different
                // device than the walk root (mount points, bind mounts, etc.).
                #[cfg(unix)]
                if let Some(dev) = root_dev {
                    use std::os::unix::fs::MetadataExt;
                    if let Ok(meta) = e.metadata() {
                        if meta.dev() != dev {
                            tracing::debug!(
                                "walk: skipping filesystem boundary: {}",
                                e.path().display()
                            );
                            return false;
                        }
                    }
                }

                if let Ok(rel) = e.path().strip_prefix(strip_root) {
                    let rel_str = normalise_path_sep(&rel.to_string_lossy());

                    // Terminal pruning: skip dirs that cannot contain any file
                    // matching the source-level include patterns.
                    if let Some(terms) = terminals {
                        let allowed = terms.iter().any(|t| {
                            t == &rel_str
                                || t.starts_with(&format!("{rel_str}/"))
                                || rel_str.starts_with(&format!("{t}/"))
                        });
                        if !allowed {
                            return false;
                        }
                    }

                    // .index override pruning: every active per-directory
                    // override must also allow this directory.
                    for (_, ov_terms) in &override_stack {
                        let allowed = ov_terms.iter().any(|t| {
                            t == &rel_str
                                || t.starts_with(&format!("{rel_str}/"))
                                || rel_str.starts_with(&format!("{t}/"))
                        });
                        if !allowed {
                            return false;
                        }
                    }
                }

                // If this directory has a .index with include patterns, push
                // an override so its children are pruned accordingly.
                // Guard with exists() first to avoid an open() syscall on every
                // directory — the common case is no .index file present.
                let index_path = e.path().join(&scan.index_file);
                if index_path.exists() {
                    if let Some(ov) = load_dir_override(e.path(), &scan.index_file) {
                        if let Some(inc) = ov.include {
                            if let Some(rel_terms) = include_dir_prefixes(&inc) {
                                if let Ok(dir_rel) = e.path().strip_prefix(strip_root) {
                                    let dir_rel_str = normalise_path_sep(&dir_rel.to_string_lossy());
                                    let abs_terms: HashSet<String> = rel_terms
                                        .iter()
                                        .map(|t| {
                                            if dir_rel_str.is_empty() {
                                                t.clone()
                                            } else {
                                                format!("{dir_rel_str}/{t}")
                                            }
                                        })
                                        .collect();
                                    override_stack.push((depth, abs_terms));
                                }
                            }
                        }
                    }
                }
            }
            // Exclude globs applied to all entry types (dirs and files).
            if let Ok(rel) = e.path().strip_prefix(strip_root) {
                let rel_str = normalise_path_sep(&rel.to_string_lossy());
                if excludes.is_match(&*rel_str) {
                    return false;
                }
            }
            true
        })
    {
        match entry {
            Ok(e) => {
                let abs = e.path().to_path_buf();
                if e.file_type().is_dir() {
                    tracing::debug!("walk: entering dir {}", abs.display());
                    callback(WalkItem::Dir(abs));
                } else if e.file_type().is_file()
                    && e.file_name().to_str().unwrap_or("") != scan.index_file
                {
                    let rel = abs
                        .strip_prefix(strip_root)
                        .map(|r| normalise_path_sep(&r.to_string_lossy()))
                        .unwrap_or_else(|_| normalise_path_sep(&abs.to_string_lossy()));
                    let name = e.file_name().to_str().unwrap_or("").to_owned();
                    let depth = e.depth();
                    callback(WalkItem::File { abs, rel, name, depth });
                }
            }
            Err(e) => {
                let access_denied = e
                    .io_error()
                    .map(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
                    .unwrap_or(false);
                let excluded = e
                    .path()
                    .and_then(|p| p.strip_prefix(strip_root).ok())
                    .map(|rel| {
                        excludes.is_match(&*normalise_path_sep(&rel.to_string_lossy()))
                    })
                    .unwrap_or(false);
                if excluded {
                    tracing::debug!("walk: skipping excluded path: {e}");
                } else if access_denied {
                    tracing::warn!("walk: skipping inaccessible path: {e}");
                } else {
                    tracing::warn!("walk: error: {e:#}");
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use find_common::config::ScanConfig;
    use tempfile::TempDir;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Minimal ScanConfig with no excludes for isolated testing.
    fn bare_scan() -> ScanConfig {
        ScanConfig {
            exclude: vec![],
            noindex_file: ".noindex".to_string(),
            index_file: ".index".to_string(),
            include_hidden: false,
            follow_symlinks: false,
            ..ScanConfig::default()
        }
    }

    fn empty_gs() -> GlobSet { build_globset(&[]).unwrap() }

    fn gs(patterns: &[&str]) -> GlobSet {
        build_globset(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    fn terms(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    /// Create a file tree under `root`. Each entry creates the path as a file
    /// (intermediate directories are created automatically).
    fn mktree(root: &Path, files: &[&str]) {
        for f in files {
            let path = root.join(f);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        }
    }

    /// Run walk and return sorted relative paths of all yielded File items.
    fn walk_files(
        root: &Path,
        scan: &ScanConfig,
        excludes: &GlobSet,
        terminals: Option<&HashSet<String>>,
    ) -> Vec<String> {
        let mut out = vec![];
        walk_source_tree(root, root, scan, excludes, terminals, |item| {
            if let WalkItem::File { rel, .. } = item {
                out.push(rel);
            }
        });
        out.sort();
        out
    }

    /// Run walk and return sorted relative paths of all yielded Dir items
    /// (excluding the root itself).
    fn walk_dirs(
        root: &Path,
        scan: &ScanConfig,
        excludes: &GlobSet,
        terminals: Option<&HashSet<String>>,
    ) -> Vec<String> {
        let mut out = vec![];
        walk_source_tree(root, root, scan, excludes, terminals, |item| {
            if let WalkItem::Dir(path) = item {
                if let Ok(rel) = path.strip_prefix(root) {
                    let s = rel.to_string_lossy().replace('\\', "/");
                    if !s.is_empty() {
                        out.push(s);
                    }
                }
            }
        });
        out.sort();
        out
    }

    // ── basic ────────────────────────────────────────────────────────────────

    #[test]
    fn flat_dir_all_files_returned() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &["a.txt", "b.rs", "c.md"]);
        let scan = bare_scan();
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["a.txt", "b.rs", "c.md"]);
    }

    #[test]
    fn nested_dirs_all_files_returned() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "root.txt", "src/main.rs", "src/lib.rs", "docs/readme.md",
        ]);
        let scan = bare_scan();
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["docs/readme.md", "root.txt", "src/lib.rs", "src/main.rs"]);
    }

    // ── include_hidden ───────────────────────────────────────────────────────

    #[test]
    fn hidden_dirs_pruned_when_include_hidden_false() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "visible.txt",
            ".git/HEAD",          // inside hidden dir — pruned
            ".git/config",
            "src/main.rs",
        ]);
        let scan = bare_scan(); // include_hidden = false
        // .git/ directory is never descended into
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["src/main.rs", "visible.txt"]);
        let dirs = walk_dirs(tmp.path(), &scan, &empty_gs(), None);
        assert!(!dirs.iter().any(|d| d.starts_with(".git")), "hidden dirs should be pruned");
    }

    #[test]
    fn hidden_files_in_visible_dirs_still_yielded() {
        // Hidden file filtering is the callback's responsibility, not the walk's.
        // Only hidden DIRECTORIES are pruned by walk_source_tree.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &["src/.gitignore", "src/main.rs"]);
        let scan = bare_scan(); // include_hidden = false
        // .gitignore is a hidden file inside a visible dir — still yielded
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["src/.gitignore", "src/main.rs"]);
    }

    #[test]
    fn include_hidden_true_traverses_hidden_dirs() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "visible.txt",
            ".git/HEAD",
            ".git/config",
        ]);
        let mut scan = bare_scan();
        scan.include_hidden = true;
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec![".git/HEAD", ".git/config", "visible.txt"]);
    }

    // ── .noindex marker ──────────────────────────────────────────────────────

    #[test]
    fn noindex_prunes_directory_and_all_contents() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "keep.txt",
            "private/.noindex",
            "private/secret.txt",
            "private/sub/deeper.bin",
        ]);
        let scan = bare_scan();
        // private/ is pruned entirely — .noindex and its siblings/descendants never yielded
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["keep.txt"]);
        let dirs = walk_dirs(tmp.path(), &scan, &empty_gs(), None);
        assert!(!dirs.iter().any(|d| d.starts_with("private")), "private/ should be pruned");
    }

    #[test]
    fn noindex_only_prunes_its_own_subtree() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "a/file.txt",
            "b/.noindex",
            "b/secret.txt",
            "c/file.txt",
        ]);
        let scan = bare_scan();
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["a/file.txt", "c/file.txt"]);
    }

    #[test]
    fn custom_noindex_filename_is_respected() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "a/file.txt",
            "b/SKIP",       // custom noindex marker
            "b/secret.txt",
        ]);
        let mut scan = bare_scan();
        scan.noindex_file = "SKIP".to_string();
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["a/file.txt"]);
    }

    // ── .index control file ──────────────────────────────────────────────────

    #[test]
    fn index_control_file_not_yielded_as_content() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &["src/.index", "src/main.rs", "src/lib.rs"]);
        let scan = bare_scan();
        // .index is a per-directory config override file — never a content file
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn index_file_without_include_fully_traversed() {
        // A .index with no include field does not prune the directory.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "sub/a.txt",
            "sub/b.txt",
            "other/c.txt",
        ]);
        std::fs::write(tmp.path().join("sub/.index"), b"max_content_size_mb = 5").unwrap();
        let scan = bare_scan();
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["other/c.txt", "sub/a.txt", "sub/b.txt"]);
    }

    #[test]
    fn index_file_with_include_prunes_sibling_dirs() {
        // backups/.index with include = ["FromMomMac/**"] should prune all
        // sibling directories inside backups/ except FromMomMac.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "backups/FromMomMac/photo.jpg",
            "backups/OldPC/data.zip",
            "backups/OldPC/more/stuff.txt",
            "code/main.rs",
        ]);
        std::fs::write(
            tmp.path().join("backups/.index"),
            b"include = [\"FromMomMac/**\"]",
        ).unwrap();
        let scan = bare_scan();
        let files = walk_files(tmp.path(), &scan, &empty_gs(), None);
        assert_eq!(files, vec!["backups/FromMomMac/photo.jpg", "code/main.rs"]);
        let dirs = walk_dirs(tmp.path(), &scan, &empty_gs(), None);
        assert!(dirs.contains(&"backups".to_string()),              "backups/ itself is traversed");
        assert!(dirs.contains(&"backups/FromMomMac".to_string()),   "FromMomMac matches the include");
        assert!(!dirs.contains(&"backups/OldPC".to_string()),       "OldPC is pruned");
        assert!(!dirs.contains(&"backups/OldPC/more".to_string()),  "OldPC/more is pruned");
        assert!(dirs.contains(&"code".to_string()),                 "code/ unaffected");
    }

    #[test]
    fn index_file_include_does_not_affect_siblings_outside_dir() {
        // The .index override in backups/ must not affect dirs outside backups/.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "backups/FromMomMac/photo.jpg",
            "documents/report.pdf",
            "documents/notes/note.txt",
        ]);
        std::fs::write(
            tmp.path().join("backups/.index"),
            b"include = [\"FromMomMac/**\"]",
        ).unwrap();
        let scan = bare_scan();
        let files = walk_files(tmp.path(), &scan, &empty_gs(), None);
        assert_eq!(files, vec!["backups/FromMomMac/photo.jpg", "documents/notes/note.txt", "documents/report.pdf"]);
    }

    #[test]
    fn custom_index_filename_is_respected() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &["src/SETTINGS", "src/main.rs"]);
        let mut scan = bare_scan();
        scan.index_file = "SETTINGS".to_string();
        assert_eq!(walk_files(tmp.path(), &scan, &empty_gs(), None),
                   vec!["src/main.rs"]);
    }

    // ── exclude globs ────────────────────────────────────────────────────────

    #[test]
    fn exclude_dir_glob_short_circuits_entire_subtree() {
        // **/node_modules/** adds **/node_modules via build_globset, which
        // matches the directory entry itself so filter_entry prunes the whole tree.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "src/index.js",
            "node_modules/lodash/index.js",
            "node_modules/react/index.js",
            "nested/node_modules/foo/bar.js",
        ]);
        assert_eq!(walk_files(tmp.path(), &bare_scan(), &gs(&["**/node_modules/**"]), None),
                   vec!["src/index.js"]);
        // node_modules directories are not traversed at all
        let dirs = walk_dirs(tmp.path(), &bare_scan(), &gs(&["**/node_modules/**"]), None);
        assert!(!dirs.iter().any(|d| d.contains("node_modules")));
    }

    #[test]
    fn exclude_file_glob_filters_files_without_pruning_dirs() {
        // **/*.tmp excludes files but does not prevent directories from being entered.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "build/output.o",
            "build/temp.tmp",
            "src/main.rs",
            "scratch.tmp",
        ]);
        let files = walk_files(tmp.path(), &bare_scan(), &gs(&["**/*.tmp"]), None);
        assert_eq!(files, vec!["build/output.o", "src/main.rs"]);
        // build/ is still traversed despite containing a .tmp file
        let dirs = walk_dirs(tmp.path(), &bare_scan(), &gs(&["**/*.tmp"]), None);
        assert!(dirs.contains(&"build".to_string()));
    }

    #[test]
    fn exclude_specific_subpath_short_circuits_only_that_subtree() {
        // build/generated/** prunes build/generated/ but not build/ itself.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "build/output.o",
            "build/generated/types.rs",
            "build/generated/schema.rs",
            "src/main.rs",
        ]);
        let files = walk_files(tmp.path(), &bare_scan(), &gs(&["build/generated/**"]), None);
        assert_eq!(files, vec!["build/output.o", "src/main.rs"]);
    }

    #[test]
    fn multiple_exclude_patterns_applied_together() {
        // Simulates exclude + exclude_extra both being compiled into one GlobSet.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "src/main.rs",
            "target/debug/binary",
            "node_modules/foo/index.js",
            "dist/bundle.js",
            "notes.tmp",
            "src/scratch.log",
        ]);
        let files = walk_files(
            tmp.path(), &bare_scan(),
            &gs(&["**/target/**", "**/node_modules/**", "**/dist/**", "**/*.tmp", "**/*.log"]),
            None,
        );
        assert_eq!(files, vec!["src/main.rs"]);
    }

    // ── terminal pruning (from include patterns) ─────────────────────────────

    #[test]
    fn terminal_full_subpath_short_circuits_other_dirs() {
        // terminals = {"src/components"} — mirrors include = ["src/components/**"].
        // Only src/ and src/components/ are descended into; all other top-level
        // dirs and src/utils/ are pruned without being traversed.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "src/components/Button.tsx",
            "src/components/Input.tsx",
            "src/utils/helpers.ts",   // src/utils not on path to terminal
            "other/ignored.ts",
            "docs/readme.md",
        ]);
        let t = terms(&["src/components"]);
        let files = walk_files(tmp.path(), &bare_scan(), &empty_gs(), Some(&t));
        assert_eq!(files, vec!["src/components/Button.tsx", "src/components/Input.tsx"]);
        let dirs = walk_dirs(tmp.path(), &bare_scan(), &empty_gs(), Some(&t));
        assert!(dirs.contains(&"src".to_string()),            "src/ is an ancestor of the terminal — must be traversed");
        assert!(dirs.contains(&"src/components".to_string()), "src/components/ is the terminal");
        assert!(!dirs.contains(&"other".to_string()),         "other/ has no terminal — pruned");
        assert!(!dirs.contains(&"docs".to_string()),          "docs/ has no terminal — pruned");
        assert!(!dirs.contains(&"src/utils".to_string()),     "src/utils/ is not on path to terminal — pruned");
    }

    #[test]
    fn terminal_multiple_disjoint_paths() {
        // terminals from include = ["home/alice/**", "tmp/projects/**"]
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "home/alice/notes.txt",
            "home/bob/notes.txt",    // home/bob not in any terminal
            "tmp/projects/code.rs",
            "tmp/cache/data.bin",    // tmp/cache not in any terminal
            "var/log/app.log",
        ]);
        let t = terms(&["home/alice", "tmp/projects"]);
        let files = walk_files(tmp.path(), &bare_scan(), &empty_gs(), Some(&t));
        assert_eq!(files, vec!["home/alice/notes.txt", "tmp/projects/code.rs"]);
    }

    #[test]
    fn terminal_none_traverses_all_dirs_for_wildcard_patterns() {
        // Patterns like **/path/*.tmp cannot determine terminals → pass None.
        // Everything is traversed; the include glob filter is the caller's job.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "a/path/file.tmp",
            "b/path/other.tmp",
            "unrelated/file.rs",
        ]);
        let dirs = walk_dirs(tmp.path(), &bare_scan(), &empty_gs(), None);
        // All directories are entered when terminals=None
        assert!(dirs.contains(&"a".to_string()));
        assert!(dirs.contains(&"b".to_string()));
        assert!(dirs.contains(&"unrelated".to_string()));
        // All files are yielded — the callback decides which match **/path/*.tmp
        let files = walk_files(tmp.path(), &bare_scan(), &empty_gs(), None);
        assert_eq!(files, vec!["a/path/file.tmp", "b/path/other.tmp", "unrelated/file.rs"]);
    }

    // ── combined scenarios ───────────────────────────────────────────────────

    #[test]
    fn terminal_pruning_with_exclude_inside_terminal() {
        // terminals = {"src"}, exclude = ["**/generated/**"]
        // src/ is entered (terminal), but src/generated/ is excluded within it.
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "src/main.rs",
            "src/generated/types.rs",
            "src/lib.rs",
            "other/ignored.rs",
        ]);
        let t = terms(&["src"]);
        let files = walk_files(tmp.path(), &bare_scan(), &gs(&["**/generated/**"]), Some(&t));
        assert_eq!(files, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn noindex_inside_terminal_still_prunes_its_subtree() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "src/public/index.html",
            "src/private/.noindex",
            "src/private/secret.txt",
            "src/lib.rs",
        ]);
        let t = terms(&["src"]);
        let files = walk_files(tmp.path(), &bare_scan(), &empty_gs(), Some(&t));
        assert_eq!(files, vec!["src/lib.rs", "src/public/index.html"]);
    }

    #[test]
    fn index_file_inside_terminal_not_yielded_directory_still_walked() {
        let tmp = TempDir::new().unwrap();
        mktree(tmp.path(), &[
            "src/.index",        // config override — not a content file
            "src/main.rs",
            "src/sub/helper.rs",
            "other/skip.rs",     // outside terminal
        ]);
        let t = terms(&["src"]);
        let files = walk_files(tmp.path(), &bare_scan(), &empty_gs(), Some(&t));
        assert_eq!(files, vec!["src/main.rs", "src/sub/helper.rs"]);
    }
}
