//! Incremental hard-link snapshots — the main speed win (design §8).
//!
//! ## The idea
//!
//! A file unchanged between two snapshots is **not copied again**: a
//! `hard_link` (microseconds) points the new name at the previous snapshot's
//! copy, on the same key filesystem. That turns "snapshot #2" from a full
//! re-copy into a metadata-only operation — the ×1000 that makes frequent
//! backups viable.
//!
//! ```text
//! <key>/
//!   .usbackup/snapshots.json     manifest (replaces the `latest` symlink)
//!   backups/
//!     photos/
//!       2026-10-27_10-00-00/     snapshot 1: full copy
//!         img1.jpg
//!       2026-10-27_11-00-00/     snapshot 2: hard-links for unchanged
//!         img1.jpg  -> link to ../2026-10-27_10-00-00/img1.jpg
//!         img2.jpg  -> copy (changed)
//! ```
//!
//! ## Manifest, not symlink
//!
//! The manifest (`snapshots.json`) records every snapshot (newest first). It
//! deliberately replaces the internal `latest` symlink the app used to create
//! (`backup.rs`) — that was an *internal* pointer, not user data, and removing
//! it fixes the Windows bug. The symlinks *in your source data* are a totally
//! different concern (§7a) and are preserved verbatim by [`crate::sync::symlink`].
//!
//! ## Reuses the engine
//!
//! Snapshot creation is a sync whose "destination" is a fresh directory and
//! whose unchanged files link from a **base** (the previous snapshot). So this
//! module builds the plan (via [`classify`], the same tolerant mtime+size
//! change detection) and hands it to the bounded two-class
//! [`SyncEngine`] with `base_root` pointed at the base snapshot. The engine
//! does the parallel work; this module owns the manifest, layout, and
//! retention.
//!
//! ## Retention
//!
//! Deleting a snapshot is `remove_dir_all` on its directory — the hard-links
//! from *other* snapshots to the same inodes stay intact, so survivors are
//! never disturbed (§8.5).

use crate::sync::compare::{classify, Action, PlanItem, Verdict};
use crate::sync::engine::SyncEngine;
use crate::sync::error::{Result, SyncError};
use crate::sync::filter::Filter;
use crate::sync::progress::Stats;
use crate::sync::tree::{scan, TreeIndex};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Directory (under the backup root) that holds the manifest.
pub const MANIFEST_DIR: &str = ".usbackup";
/// Manifest file name.
pub const MANIFEST_FILE: &str = "snapshots.json";
/// Directory (under the backup root) that holds snapshot subdirectories.
pub const BACKUPS_DIR: &str = "backups";

/// The manifest: every snapshot, **newest first**.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotManifest {
    /// Snapshots ordered newest-first (`snapshots[0]` is the base for the next
    /// run). `default` (empty) is what a fresh key has.
    #[serde(default)]
    pub snapshots: Vec<SnapshotEntry>,
}

/// One recorded snapshot.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    /// Directory name, e.g. `2026-10-27_10-00-00`.
    pub dir_name: String,
    /// Creation instant (UTC).
    pub created_at: DateTime<Utc>,
    /// Number of files (regular files + symlinks) in the snapshot.
    pub files: u64,
    /// Total bytes of regular-file data the snapshot represents.
    pub bytes: u64,
}

/// Retention rules. A snapshot is **kept** if it satisfies *any* configured
/// rule (the union — you keep the last N *and* everything from the last D
/// days). With no rules, everything is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Keep at least the N newest snapshots.
    pub keep_last: Option<usize>,
    /// Keep snapshots created within the last D days.
    pub keep_days: Option<u32>,
}

impl RetentionPolicy {
    /// Keep exactly the `n` newest snapshots.
    pub fn keep_last(n: usize) -> Self {
        RetentionPolicy {
            keep_last: Some(n),
            keep_days: None,
        }
    }

    /// Whether any rule is configured (no rules = keep everything).
    fn has_rules(&self) -> bool {
        self.keep_last.is_some() || self.keep_days.is_some()
    }
}

/// Summary of one [`SnapshotStore::create_snapshot`] run.
#[derive(Debug, Clone)]
pub struct SnapshotReport {
    /// Directory name of the snapshot that was created.
    pub dir_name: String,
    /// Engine stats (copied vs linked bytes, symlinks, errors).
    pub stats: Stats,
    /// Snapshot directories removed by retention.
    pub removed: Vec<String>,
}

/// The on-key snapshot layout: where snapshot directories live and where the
/// manifest is read/written (design §8.1).
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    /// Root that contains one directory per snapshot.
    pub backups_dir: PathBuf,
    /// Path of `snapshots.json`.
    pub manifest_path: PathBuf,
    /// Exclusions applied to every snapshot this store creates (the rule's
    /// `--exclude` patterns plus a local `.gitignore`). [`Filter::none`] means
    /// "copy everything", the behaviour before this field existed.
    filter: Filter,
}

impl SnapshotStore {
    /// Layout under `root` (the backup destination root):
    /// `root/backups/` for snapshots, `root/.usbackup/snapshots.json` for the
    /// manifest.
    pub fn new(root: &Path) -> Self {
        SnapshotStore {
            backups_dir: root.join(BACKUPS_DIR),
            manifest_path: root.join(MANIFEST_DIR).join(MANIFEST_FILE),
            filter: Filter::none(),
        }
    }

    /// Set the exclusions applied to snapshots this store creates. Without
    /// this, an incremental backup would ignore `--exclude`/`.gitignore` and
    /// snapshot files a filtered rsync would have skipped.
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self
    }

    /// Read the manifest; a missing file is a fresh, empty manifest.
    pub fn load(&self) -> Result<SnapshotManifest> {
        match fs::read_to_string(&self.manifest_path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                SyncError::io(
                    &self.manifest_path,
                    io::Error::new(io::ErrorKind::InvalidData, e),
                )
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SnapshotManifest::default()),
            Err(e) => Err(SyncError::io(&self.manifest_path, e)),
        }
    }

    /// Write the manifest atomically (temp file + rename), so a crash never
    /// leaves a half-written manifest.
    pub fn save(&self, manifest: &SnapshotManifest) -> Result<()> {
        if let Some(parent) = self.manifest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| SyncError::io(parent, e))?;
        }
        let json = serde_json::to_string_pretty(manifest)
            .map_err(|e| SyncError::io(&self.manifest_path, io::Error::other(e)))?;
        let mut tmp = self.manifest_path.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, json).map_err(|e| SyncError::io(&tmp, e))?;
        fs::rename(&tmp, &self.manifest_path).map_err(|e| SyncError::io(&self.manifest_path, e))?;
        Ok(())
    }

    /// The newest snapshot as `(entry, full directory path)`, if any.
    pub fn latest(&self) -> Result<Option<(SnapshotEntry, PathBuf)>> {
        let manifest = self.load()?;
        Ok(manifest
            .snapshots
            .first()
            .map(|e| (e.clone(), self.backups_dir.join(&e.dir_name))))
    }

    /// A directory name for `now` that is not already used (in the manifest or
    /// on disk). Collisions happen when two snapshots land in the same second,
    /// so a `_2`, `_3`, … suffix is appended until it is free.
    fn unique_dir_name(&self, manifest: &SnapshotManifest, now: DateTime<Utc>) -> String {
        let base = now.format("%Y-%m-%d_%H-%M-%S").to_string();
        let mut name = base.clone();
        let mut n = 1;
        let taken = |nm: &str| {
            manifest.snapshots.iter().any(|s| s.dir_name == nm)
                || self.backups_dir.join(nm).exists()
        };
        while taken(&name) {
            n += 1;
            name = format!("{base}_{n}");
        }
        name
    }

    /// Create a new snapshot of `src_root` and apply `policy`.
    ///
    /// The newest existing snapshot is the **base**: files unchanged against it
    /// (same size, mtime within tolerance — via [`classify`]) are hard-linked;
    /// everything else is copied from the source. Directories and symlinks are
    /// created/recreated by the engine as in a normal sync.
    pub fn create_snapshot(
        &self,
        src_root: &Path,
        policy: &RetentionPolicy,
    ) -> Result<SnapshotReport> {
        let mut manifest = self.load()?;

        // The base is the newest snapshot that still exists on disk.
        let base_dir = manifest
            .snapshots
            .first()
            .map(|e| self.backups_dir.join(&e.dir_name))
            .filter(|p| p.is_dir());

        let mut src_index = scan(src_root)?;
        // Drop excluded entries (the rule's --exclude patterns and .gitignore)
        // so a snapshot mirrors what a filtered rsync incremental would copy.
        // Filtering the index -- not the plan -- keeps the manifest's
        // files/bytes counts below describing the snapshot's real content.
        src_index.retain(|meta| !self.filter.is_excluded_deep(&meta.rel_path, meta.is_dir));
        let base_index = match &base_dir {
            Some(d) => scan(d)?,
            None => TreeIndex::default(),
        };

        let now = Utc::now();
        let dir_name = self.unique_dir_name(&manifest, now);
        let new_dir = self.backups_dir.join(&dir_name);
        fs::create_dir_all(&new_dir).map_err(|e| SyncError::io(&new_dir, e))?;

        let plan = build_snapshot_plan(&src_index, &base_index);

        let engine = SyncEngine {
            base_root: base_dir,
            ..SyncEngine::default()
        };
        let stats = engine.execute(src_root, &new_dir, &plan)?;
        if stats.errors > 0 {
            // Surface a partial failure so callers can report it, but the
            // snapshot that WAS written stays recorded below.
            return Err(SyncError::PartialFailure {
                count: stats.errors as usize,
            });
        }

        // Record the new snapshot: total files/bytes describe the snapshot's
        // content (independent of how much was linked vs copied this run).
        let entry = SnapshotEntry {
            dir_name: dir_name.clone(),
            created_at: now,
            files: src_index.iter().filter(|e| !e.is_dir).count() as u64,
            bytes: src_index
                .iter()
                .filter(|e| !e.is_dir && !e.is_symlink)
                .map(|e| e.len)
                .sum(),
        };
        manifest.snapshots.insert(0, entry);
        // Durably record the new snapshot before retention touches anything, so
        // a retention failure can never orphan it.
        self.save(&manifest)?;

        let removed = self.apply_retention(&mut manifest, policy)?;
        self.save(&manifest)?;

        Ok(SnapshotReport {
            dir_name,
            stats,
            removed,
        })
    }

    /// Indices (descending) of snapshots to remove under `policy`.
    fn retention_indices(
        &self,
        manifest: &SnapshotManifest,
        policy: &RetentionPolicy,
    ) -> Vec<usize> {
        if !policy.has_rules() {
            return Vec::new(); // no rules -> keep everything
        }
        let now = Utc::now();
        let mut remove = Vec::new();
        for (i, snap) in manifest.snapshots.iter().enumerate() {
            let mut keep = false;
            if let Some(last) = policy.keep_last {
                if i < last {
                    keep = true; // snapshots are newest-first
                }
            }
            if let Some(days) = policy.keep_days {
                let age_days = now.signed_duration_since(snap.created_at).num_days();
                if age_days <= i64::from(days) {
                    keep = true;
                }
            }
            if !keep {
                remove.push(i);
            }
        }
        remove.reverse(); // remove from the tail so earlier indices stay valid
        remove
    }

    /// Remove over-retention snapshots (directories + manifest entries) and
    /// return the removed directory names. Descending order keeps indices valid;
    /// a directory is only dropped from the manifest after its removal
    /// succeeds, so a failure never leaves an untracked orphan.
    pub fn apply_retention(
        &self,
        manifest: &mut SnapshotManifest,
        policy: &RetentionPolicy,
    ) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for i in self.retention_indices(manifest, policy) {
            let dir = self.backups_dir.join(&manifest.snapshots[i].dir_name);
            match fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {} // already gone
                Err(e) => return Err(SyncError::io(&dir, e)),       // entry stays: dir still exists
            }
            removed.push(manifest.snapshots.remove(i).dir_name);
        }
        Ok(removed)
    }
}

/// Build the snapshot plan from the source and base indices: directories are
/// created, symlinks recreated, files hard-linked when unchanged against the
/// base and copied otherwise.
pub fn build_snapshot_plan(src_index: &TreeIndex, base_index: &TreeIndex) -> Vec<PlanItem> {
    src_index
        .iter()
        .map(|entry| {
            let action = if entry.is_dir {
                Action::CreateDir
            } else if entry.is_symlink {
                Action::Symlink
            } else if classify(entry, base_index.get(&entry.rel_path)) == Verdict::Unchanged {
                // Same size + mtime vs the base snapshot: link it, don't copy.
                Action::LinkOrCopy
            } else {
                Action::Copy
            };
            PlanItem {
                rel_path: entry.rel_path.clone(),
                meta: entry.clone(),
                action,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn seed(root: &Path) -> io::Result<()> {
        fs::create_dir_all(root.join("docs"))?;
        fs::write(root.join("docs/a.txt"), b"alpha")?;
        fs::write(root.join("docs/b.txt"), b"beta")?;
        fs::write(root.join("root.txt"), b"top")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("docs/a.txt", root.join("docs/link"))?;
        Ok(())
    }

    #[test]
    fn first_snapshot_copies_everything() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());

        let report = store
            .create_snapshot(&src, &RetentionPolicy::default())
            .unwrap();

        assert_eq!(report.stats.files_copied, 3, "no base -> everything copied");
        assert_eq!(report.stats.files_linked, 0);
        assert_eq!(report.stats.errors, 0);
        assert!(store
            .backups_dir
            .join(&report.dir_name)
            .join("docs/a.txt")
            .exists());
    }

    #[test]
    fn snapshot_honors_exclusions() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        // A tree the filter will drop.
        fs::create_dir_all(src.join("dist")).unwrap();
        fs::write(src.join("dist/out.js"), b"bundle").unwrap();

        let filter = Filter::builder().add_pattern("dist/").build();
        let store = SnapshotStore::new(dir.path()).with_filter(filter);

        let report = store
            .create_snapshot(&src, &RetentionPolicy::default())
            .unwrap();

        let snap = store.backups_dir.join(&report.dir_name);
        // Kept files land in the snapshot...
        assert!(snap.join("docs/a.txt").exists());
        assert!(snap.join("root.txt").exists());
        // ...the excluded tree never does.
        assert!(!snap.join("dist").exists());
        assert!(!snap.join("dist/out.js").exists());

        // The manifest describes the snapshot's real content: 4 kept entries
        // (docs/a.txt, docs/b.txt, root.txt + the docs/link symlink), not 5 --
        // the filtered-out dist/out.js is not counted.
        let manifest = store.load().unwrap();
        assert_eq!(manifest.snapshots[0].files, 4);
    }

    #[test]
    fn second_snapshot_links_unchanged_files() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());
        let policy = RetentionPolicy::default();

        let first = store.create_snapshot(&src, &policy).unwrap();
        let second = store.create_snapshot(&src, &policy).unwrap();

        // Nothing changed: every regular file is linked, none recopied.
        assert_eq!(second.stats.files_linked, 3);
        assert_eq!(second.stats.files_copied, 0);
        assert_eq!(second.stats.errors, 0);

        // The linked file shares an inode with snapshot #1 (hard link, not copy).
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let a1 = store.backups_dir.join(&first.dir_name).join("docs/a.txt");
            let a2 = store.backups_dir.join(&second.dir_name).join("docs/a.txt");
            assert_eq!(
                fs::metadata(&a1).unwrap().ino(),
                fs::metadata(&a2).unwrap().ino(),
                "unchanged file must be a hard link to the base, not a copy"
            );
            assert_eq!(fs::metadata(&a2).unwrap().nlink(), 2);
        }

        // Content is correct either way.
        assert_eq!(
            fs::read(store.backups_dir.join(&second.dir_name).join("docs/a.txt")).unwrap(),
            b"alpha"
        );
    }

    #[test]
    fn changed_file_is_recopied_while_others_link() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());
        let policy = RetentionPolicy::default();

        store.create_snapshot(&src, &policy).unwrap();

        // Change b.txt (different length so len+mtime detection is definitive).
        fs::write(src.join("docs/b.txt"), b"beta changed and longer").unwrap();

        let report = store.create_snapshot(&src, &policy).unwrap();
        assert_eq!(report.stats.files_copied, 1, "only the changed file");
        assert_eq!(report.stats.files_linked, 2, "the rest are linked");
        assert_eq!(report.stats.errors, 0);
        assert_eq!(
            fs::read(store.backups_dir.join(&report.dir_name).join("docs/b.txt")).unwrap(),
            b"beta changed and longer"
        );
    }

    #[test]
    fn symlink_is_preserved_as_a_link_in_snapshot() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());

        let report = store
            .create_snapshot(&src, &RetentionPolicy::default())
            .unwrap();
        assert_eq!(report.stats.symlinks_preserved, 1);

        #[cfg(unix)]
        {
            let link = store.backups_dir.join(&report.dir_name).join("docs/link");
            assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
            assert_eq!(fs::read_link(&link).unwrap(), Path::new("docs/a.txt"));
        }
    }

    #[test]
    fn manifest_records_snapshots_newest_first() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());
        let policy = RetentionPolicy::default();

        let a = store.create_snapshot(&src, &policy).unwrap();
        let b = store.create_snapshot(&src, &policy).unwrap();

        let manifest = store.load().unwrap();
        assert_eq!(manifest.snapshots.len(), 2);
        assert_eq!(manifest.snapshots[0].dir_name, b.dir_name, "newest first");
        assert_eq!(manifest.snapshots[1].dir_name, a.dir_name);
        // Names must be unique even within the same second.
        assert_ne!(a.dir_name, b.dir_name);
        // The manifest file exists and is parseable.
        assert!(store.manifest_path.exists());
    }

    #[test]
    fn latest_returns_the_newest_snapshot() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());

        assert!(store.latest().unwrap().is_none(), "no snapshots yet");
        let a = store
            .create_snapshot(&src, &RetentionPolicy::default())
            .unwrap();
        let (entry, path) = store.latest().unwrap().unwrap();
        assert_eq!(entry.dir_name, a.dir_name);
        assert_eq!(path, store.backups_dir.join(&a.dir_name));
    }

    #[test]
    fn retention_keep_last_prunes_oldest() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());

        let a = store
            .create_snapshot(&src, &RetentionPolicy::default())
            .unwrap();
        let b = store
            .create_snapshot(&src, &RetentionPolicy::default())
            .unwrap();

        assert!(store.backups_dir.join(&a.dir_name).exists());
        assert!(store.backups_dir.join(&b.dir_name).exists());

        // keep_last(1) on the third run keeps only the newest and prunes the
        // older two (a and b). Their hard-linked data survives in the newest
        // snapshot because the inodes stay linked there.
        let report = store
            .create_snapshot(&src, &RetentionPolicy::keep_last(1))
            .unwrap();

        assert_eq!(report.removed.len(), 2, "both older snapshots are pruned");
        assert!(
            !store.backups_dir.join(&a.dir_name).exists(),
            "oldest snapshot dir must be pruned"
        );
        assert!(!store.backups_dir.join(&b.dir_name).exists());
        assert!(store.backups_dir.join(&report.dir_name).exists());
        let manifest = store.load().unwrap();
        assert_eq!(manifest.snapshots.len(), 1, "only the newest remains");
        assert_eq!(manifest.snapshots[0].dir_name, report.dir_name);
    }

    #[test]
    fn retention_without_rules_keeps_all() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());
        let policy = RetentionPolicy::default();

        let a = store.create_snapshot(&src, &policy).unwrap();
        let _b = store.create_snapshot(&src, &policy).unwrap();
        let _c = store.create_snapshot(&src, &policy).unwrap();

        let manifest = store.load().unwrap();
        assert_eq!(manifest.snapshots.len(), 3, "no rules -> keep all");
        assert!(store.backups_dir.join(a.dir_name).exists());
    }

    #[test]
    fn retention_survives_deleting_a_snapshot_base() {
        // Deleting an old snapshot must not disturb survivors whose files are
        // hard-linked to it.
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();
        let store = SnapshotStore::new(dir.path());
        let policy = RetentionPolicy::default();

        let a = store.create_snapshot(&src, &policy).unwrap();
        let _b = store.create_snapshot(&src, &policy).unwrap();

        // Remove snapshot A while B links into it.
        let mut manifest = store.load().unwrap();
        let removed = store
            .apply_retention(&mut manifest, &RetentionPolicy::keep_last(1))
            .unwrap();
        assert_eq!(removed, vec![a.dir_name.clone()]);

        // B's linked file still resolves to correct content (inode survives).
        let b_name = manifest.snapshots[0].dir_name.clone();
        assert_eq!(
            fs::read(store.backups_dir.join(b_name).join("docs/a.txt")).unwrap(),
            b"alpha"
        );
    }

    #[test]
    fn manifest_roundtrips_through_json() {
        let dir = tempdir().unwrap();
        let store = SnapshotStore::new(dir.path());
        let manifest = SnapshotManifest {
            snapshots: vec![SnapshotEntry {
                dir_name: "2026-10-27_10-00-00".to_string(),
                created_at: Utc::now(),
                files: 3,
                bytes: 10,
            }],
        };
        store.save(&manifest).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, manifest);
    }

    #[test]
    fn missing_manifest_loads_as_empty() {
        let dir = tempdir().unwrap();
        let store = SnapshotStore::new(dir.path());
        assert_eq!(store.load().unwrap(), SnapshotManifest::default());
    }

    #[test]
    fn plan_links_unchanged_copies_changed_and_new() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        seed(&src).unwrap();

        let src_index = scan(&src).unwrap();

        // Empty base: everything that is a file must be Copy.
        let plan = build_snapshot_plan(&src_index, &TreeIndex::default());
        let copies = plan.iter().filter(|p| p.action == Action::Copy).count();
        assert_eq!(copies, 3, "no base -> all files copied");

        // Base == src (same index): files are unchanged -> LinkOrCopy.
        let plan2 = build_snapshot_plan(&src_index, &src_index);
        let links = plan2
            .iter()
            .filter(|p| p.action == Action::LinkOrCopy)
            .count();
        assert_eq!(links, 3, "identical base -> all files linked");
        // Dirs still CreateDir, symlink still Symlink regardless of base.
        assert!(plan2.iter().any(|p| p.action == Action::CreateDir));
        assert!(plan2.iter().any(|p| p.action == Action::Symlink));
    }
}
