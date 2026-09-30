use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;

use crate::key::ContentKey;
use crate::store::{CompactResult, ContentStore};

/// A `ContentStore` that delegates to multiple inner stores simultaneously.
///
/// - **Writes** (`put`, `delete`) are forwarded to every inner store.
/// - **Reads** (`get_lines`, `contains`) try stores in order and return the
///   first hit.
/// - **Compaction** runs on every store and sums the results.
///
/// This allows populating multiple backends in a single scan run, which is
/// useful for benchmarking different configurations against identical data.
pub struct MultiContentStore {
    pub stores: Vec<Arc<dyn ContentStore>>,
}

impl ContentStore for MultiContentStore {
    fn put(&self, key: &ContentKey, blob: &str) -> Result<bool> {
        let mut stored = false;
        for s in &self.stores {
            if s.put(key, blob)? {
                stored = true;
            }
        }
        Ok(stored)
    }

    fn delete(&self, key: &ContentKey) -> Result<()> {
        for s in &self.stores {
            s.delete(key)?;
        }
        Ok(())
    }

    fn get_lines(&self, key: &ContentKey, lo: usize, hi: usize) -> Result<Option<Vec<(usize, String)>>> {
        for s in &self.stores {
            if let Some(lines) = s.get_lines(key, lo, hi)? {
                return Ok(Some(lines));
            }
        }
        Ok(None)
    }

    fn contains(&self, key: &ContentKey) -> Result<bool> {
        for s in &self.stores {
            if s.contains(key)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn missing_keys(&self, keys: &[ContentKey]) -> Result<Vec<ContentKey>> {
        let mut missing = keys.to_vec();
        for s in &self.stores {
            if missing.is_empty() {
                break;
            }
            missing = s.missing_keys(&missing)?;
        }
        Ok(missing)
    }

    fn compact(&self, live_keys: &HashSet<ContentKey>, dry_run: bool) -> Result<CompactResult> {
        let mut total = CompactResult {
            units_scanned: 0,
            units_rewritten: 0,
            units_deleted: 0,
            chunks_removed: 0,
            bytes_freed: 0,
        };
        for s in &self.stores {
            let r = s.compact(live_keys, dry_run)?;
            total.units_scanned += r.units_scanned;
            total.units_rewritten += r.units_rewritten;
            total.units_deleted += r.units_deleted;
            total.chunks_removed += r.chunks_removed;
            total.bytes_freed += r.bytes_freed;
        }
        Ok(total)
    }

    fn storage_stats(&self) -> Option<(u64, u64)> {
        // Sum across all stores.
        let mut total_count = 0u64;
        let mut total_bytes = 0u64;
        let mut any = false;
        for s in &self.stores {
            if let Some((count, bytes)) = s.storage_stats() {
                total_count += count;
                total_bytes += bytes;
                any = true;
            }
        }
        if any { Some((total_count, total_bytes)) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_store::SqliteContentStore;
    use tempfile::TempDir;

    /// A key is missing only if *every* inner store lacks it.
    #[test]
    fn missing_keys_is_intersection_of_inner_misses() {
        let (d1, d2) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let s1 = Arc::new(SqliteContentStore::open(d1.path(), None, None, None).unwrap());
        let s2 = Arc::new(SqliteContentStore::open(d2.path(), None, None, None).unwrap());
        let (a, b, c) = (
            ContentKey::new("a".repeat(64)),
            ContentKey::new("b".repeat(64)),
            ContentKey::new("c".repeat(64)),
        );
        s1.put(&a, "one").unwrap(); // only in store 1
        s2.put(&b, "two").unwrap(); // only in store 2
        let multi = MultiContentStore { stores: vec![s1, s2] };

        let mut missing = multi.missing_keys(&[a.clone(), b.clone(), c.clone()]).unwrap();
        missing.sort();
        assert_eq!(missing, vec![c.clone()]);
        for k in [&a, &b, &c] {
            assert_eq!(multi.contains(k).unwrap(), !multi.missing_keys(&[k.clone()]).unwrap().contains(k));
        }
        assert!(multi.missing_keys(&[]).unwrap().is_empty());
    }
}
