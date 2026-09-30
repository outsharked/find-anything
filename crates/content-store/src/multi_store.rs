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
