use std::collections::HashSet;

use crate::key::ContentKey;

/// Statistics returned by a `compact` call.
pub struct CompactResult {
    pub units_scanned:   usize,
    pub units_rewritten: usize,
    pub units_deleted:   usize,
    pub chunks_removed:  usize,
    pub bytes_freed:     u64,
}

/// Content-addressable blob storage abstraction.
pub trait ContentStore: Send + Sync {
    /// Store a blob of text keyed by `key`.
    ///
    /// The blob is all lines of a file joined with `'\n'`; line positions are
    /// 0-based (position 0 = first element).
    ///
    /// Idempotent: returns `Ok(false)` if the key already exists (no re-write).
    fn put(&self, key: &ContentKey, blob: &str) -> anyhow::Result<bool>;

    /// Replace the stored blob for `key`, even if one already exists.
    ///
    /// Deletes any existing blob then stores the new one.  Returns `Ok(true)`
    /// if the blob was written, `Ok(false)` if the blob was empty and nothing
    /// was stored.
    fn put_overwrite(&self, key: &ContentKey, blob: &str) -> anyhow::Result<bool> {
        self.delete(key)?;
        self.put(key, blob)
    }

    /// Remove all stored data for `key`. No-op if the key does not exist.
    fn delete(&self, key: &ContentKey) -> anyhow::Result<()>;

    /// Return lines in the range `lo..=hi` (inclusive, 0-based positions).
    ///
    /// Returns `None` if the key is not found.
    /// Returns `Some(vec)` where each element is `(pos, content)` for lines
    /// that exist within `[lo, hi]`.
    fn get_lines(
        &self,
        key: &ContentKey,
        lo: usize,
        hi: usize,
    ) -> anyhow::Result<Option<Vec<(usize, String)>>>;

    /// Return `true` if a complete blob is stored for `key`.
    fn contains(&self, key: &ContentKey) -> anyhow::Result<bool>;

    /// Remove blobs not in `live_keys` and compact ZIP archives.
    fn compact(
        &self,
        live_keys: &HashSet<ContentKey>,
        dry_run: bool,
    ) -> anyhow::Result<CompactResult>;

    /// Optional stats hook for monitoring: (storage-unit count, bytes on disk).
    /// Default impl returns `None`.
    fn storage_stats(&self) -> Option<(u64 /* units */, u64 /* bytes */)> {
        None
    }
}
