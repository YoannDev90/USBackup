//! Exclusion filtering: explicit patterns + `.gitignore` support.
//!
//! Two sources of exclusions are combined into a single matcher:
//!
//! 1. **Explicit patterns** from the backup rule (`rule.exclude`), the same
//!    set the old rsync invocation passed via `--exclude=`.
//! 2. **`.gitignore` files**, matched with real gitignore semantics via the
//!    `ignore` crate (negation `!`, directory-only `d/`, anchoring `/d`, `**`).
//!
//! A directory that is excluded is pruned from traversal: nothing under it can
//! be included, so the walker can skip the whole subtree.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Path, PathBuf};

/// Why a path was excluded. Useful for verbose logging and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcludedBy {
    /// An explicit `rule.exclude` pattern. Carries the matched glob text.
    Pattern(Option<String>),
    /// A `.gitignore` file.
    Gitignore(PathBuf),
}

/// A built matcher. Immutable and cheap to clone across threads.
#[derive(Debug, Clone)]
pub struct Filter {
    /// Explicit patterns, kept separately so we can report which one matched.
    explicit: Gitignore,
    /// Merged `.gitignore` matchers (one per file, in load order).
    gitignores: Vec<(PathBuf, Gitignore)>,
}

/// Accumulates patterns before building an immutable [`Filter`].
#[derive(Debug)]
pub struct FilterBuilder {
    explicit: GitignoreBuilder,
    explicit_count: usize,
    gitignore_roots: Vec<PathBuf>,
}

impl FilterBuilder {
    pub fn new() -> Self {
        // An empty root path makes patterns match against relative paths.
        Self {
            explicit: GitignoreBuilder::new(""),
            explicit_count: 0,
            gitignore_roots: Vec::new(),
        }
    }

    /// Add a single explicit exclude pattern (rsync `--exclude=` semantics,
    /// which map closely onto gitignore globs).
    pub fn add_pattern(mut self, pattern: &str) -> Self {
        // `GitignoreBuilder::add_line` returns Err for a malformed pattern;
        // we skip bad patterns rather than failing the whole backup.
        if self.explicit.add_line(None, pattern).is_ok() {
            self.explicit_count += 1;
        }
        self
    }

    /// Add many explicit patterns.
    pub fn add_patterns<I, S>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for p in patterns {
            self = self.add_pattern(p.as_ref());
        }
        self
    }

    /// Load a `.gitignore` file. A missing or unreadable file is ignored.
    pub fn add_gitignore(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        self.gitignore_roots.push(path);
        self
    }

    /// Build the matcher. Invalid `.gitignore` files are skipped, not fatal.
    pub fn build(self) -> Filter {
        let explicit = self.explicit.build().unwrap_or_else(|_| {
            // Fallback: a matcher that never matches.
            GitignoreBuilder::new("").build().expect("empty gitignore")
        });

        let gitignores = self
            .gitignore_roots
            .into_iter()
            .filter_map(|root| {
                let mut b = GitignoreBuilder::new("");
                // `add` returns Option<Error>: None means success.
                if b.add(&root).is_some() {
                    return None;
                }
                let gi = b.build().ok()?;
                Some((root, gi))
            })
            .collect();

        Filter {
            explicit,
            gitignores,
        }
    }
}

impl Filter {
    pub fn builder() -> FilterBuilder {
        FilterBuilder::new()
    }

    /// An empty filter that excludes nothing.
    pub fn none() -> Self {
        FilterBuilder::new().build()
    }

    /// True when `rel_path` is excluded. `is_dir` enables directory-only
    /// patterns. Returns the reason when excluded.
    pub fn excluded(&self, rel_path: &Path, is_dir: bool) -> Option<ExcludedBy> {
        // Explicit patterns win first, so a rule's own excludes always apply.
        // `matched` returns Match<&Glob>; a Whitelist (`!pattern`) means the
        // path is explicitly re-included and must NOT be excluded.
        let m = self.explicit.matched(rel_path, is_dir);
        if m.is_ignore() {
            let pattern = m.inner().map(|g| g.original().to_string());
            return Some(ExcludedBy::Pattern(pattern));
        }
        if m.is_whitelist() {
            return None;
        }

        for (root, gi) in &self.gitignores {
            let m = gi.matched(rel_path, is_dir);
            if m.is_ignore() {
                return Some(ExcludedBy::Gitignore(root.clone()));
            }
            if m.is_whitelist() {
                // A `!` rule re-includes the path; stop checking further.
                return None;
            }
        }

        None
    }

    /// Convenience boolean wrapper around [`Filter::excluded`].
    pub fn is_excluded(&self, rel_path: &Path, is_dir: bool) -> bool {
        self.excluded(rel_path, is_dir).is_some()
    }

    /// Should this entry be **skipped**, accounting for the fact that excluding
    /// a directory excludes everything beneath it?
    ///
    /// [`Filter::excluded`] matches a single path against the rules, which is
    /// the right primitive for "is *this* path named by a rule". But gitignore
    /// semantics say `dist/` also excludes `dist/out.js`, and that you **cannot
    /// re-include** a file whose parent directory is excluded. This method adds
    /// that ancestor pass, so it is the predicate to use when filtering a scan
    /// or a sync plan — otherwise an excluded tree's children would be copied
    /// file-by-file.
    pub fn is_excluded_deep(&self, rel_path: &Path, is_dir: bool) -> bool {
        if self.excluded(rel_path, is_dir).is_some() {
            return true;
        }
        // An excluded ancestor directory wins over any `!` re-inclusion of this
        // path (gitignore forbids re-including under an excluded parent).
        self.has_excluded_ancestor(rel_path)
    }

    /// True when any ancestor directory of `rel_path` is excluded.
    fn has_excluded_ancestor(&self, rel_path: &Path) -> bool {
        rel_path
            .ancestors()
            .skip(1) // the path itself is handled by `excluded`
            .take_while(|a| !a.as_os_str().is_empty()) // stop at the root
            .any(|anc| self.excluded(anc, true).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn excludes_by_explicit_pattern() {
        let f = Filter::builder().add_pattern("*.tmp").build();
        assert!(f.is_excluded(Path::new("foo/bar.tmp"), false));
        assert!(!f.is_excluded(Path::new("foo/bar.txt"), false));
    }

    #[test]
    fn excludes_directory_only_pattern() {
        let f = Filter::builder().add_pattern("cache/").build();
        assert!(f.is_excluded(Path::new("app/cache"), true));
        // A file literally named "cache" is not a dir, so not excluded.
        assert!(!f.is_excluded(Path::new("app/cache"), false));
    }

    #[test]
    fn anchored_pattern_matches_from_root() {
        let f = Filter::builder().add_pattern("/build").build();
        assert!(f.is_excluded(Path::new("build"), true));
        // Nested "build" is not anchored to root, so not excluded.
        assert!(!f.is_excluded(Path::new("src/build"), true));
    }

    #[test]
    fn multiple_patterns_all_match() {
        let f = Filter::builder()
            .add_patterns(["*.log", "node_modules/", ".DS_Store"])
            .build();
        assert!(f.is_excluded(Path::new("a.log"), false));
        assert!(f.is_excluded(Path::new("x/node_modules"), true));
        assert!(f.is_excluded(Path::new(".DS_Store"), false));
        assert!(!f.is_excluded(Path::new("src/main.rs"), false));
    }

    #[test]
    fn gitignore_file_is_honoured() {
        let dir = tempdir().unwrap();
        let gi = dir.path().join(".gitignore");
        fs::write(&gi, "*.o\ndist/\n").unwrap();

        let f = Filter::builder().add_gitignore(&gi).build();
        assert!(f.is_excluded(Path::new("src/foo.o"), false));
        assert!(f.is_excluded(Path::new("dist"), true));
        assert!(!f.is_excluded(Path::new("src/foo.rs"), false));
    }

    #[test]
    fn missing_gitignore_is_not_fatal() {
        let f = Filter::builder()
            .add_gitignore("/nonexistent/.gitignore")
            .build();
        assert!(!f.is_excluded(Path::new("anything"), false));
    }

    #[test]
    fn child_of_excluded_dir_is_excluded() {
        // A dir-only pattern must exclude the directory's contents too, not
        // just the directory entry -- plan filtering relies on this so an
        // excluded tree is never copied file-by-file.
        let f = Filter::builder().add_pattern("dist/").build();
        assert!(f.is_excluded_deep(Path::new("dist"), true));
        assert!(f.is_excluded_deep(Path::new("dist/out.js"), false));
        assert!(f.is_excluded_deep(Path::new("dist/deep/nested/file.js"), false));
        assert!(!f.is_excluded_deep(Path::new("src/lib.js"), false));
        // The single-path primitive does NOT match descendants (it reports only
        // what names the path directly); that's what `is_excluded_deep` adds.
        assert!(!f.is_excluded(Path::new("dist/out.js"), false));
    }

    #[test]
    fn reinclude_under_excluded_parent_stays_excluded() {
        // gitignore forbids re-including a file whose parent directory is
        // excluded, so `!dist/keep.txt` must NOT rescue it from `dist/`.
        let f = Filter::builder()
            .add_pattern("dist/")
            .add_pattern("!dist/keep.txt")
            .build();
        assert!(f.is_excluded_deep(Path::new("dist/keep.txt"), false));
    }

    #[test]
    fn reinclude_with_no_excluded_parent_wins() {
        // A plain `!` re-inclusion with no excluded ancestor must succeed.
        let f = Filter::builder()
            .add_pattern("*.log")
            .add_pattern("!important.log")
            .build();
        assert!(!f.is_excluded_deep(Path::new("important.log"), false));
        assert!(!f.is_excluded_deep(Path::new("sub/important.log"), false));
        assert!(f.is_excluded_deep(Path::new("other.log"), false));
    }

    #[test]
    fn empty_filter_excludes_nothing() {
        let f = Filter::none();
        assert!(!f.is_excluded(Path::new("whatever/file.bin"), false));
    }

    #[test]
    fn negation_reincludes_path() {
        // "*.log" then "!important.log" — the negation must win.
        let f = Filter::builder()
            .add_pattern("*.log")
            .add_pattern("!important.log")
            .build();
        assert!(f.is_excluded(Path::new("a.log"), false));
        assert!(!f.is_excluded(Path::new("important.log"), false));
    }

    #[test]
    fn excluded_reports_matched_pattern_text() {
        let f = Filter::builder().add_pattern("*.tmp").build();
        let reason = f.excluded(Path::new("x.tmp"), false).unwrap();
        assert_eq!(reason, ExcludedBy::Pattern(Some("*.tmp".to_string())));
    }

    #[test]
    fn malformed_pattern_is_skipped_not_fatal() {
        // "!" alone is invalid; builder must not panic.
        let f = Filter::builder()
            .add_pattern("!")
            .add_pattern("*.bak")
            .build();
        assert!(f.is_excluded(Path::new("x.bak"), false));
    }
}
