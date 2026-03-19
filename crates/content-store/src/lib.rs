pub mod bench;
mod key;
mod multi_store;
mod sqlite_store;
mod store;

pub use key::ContentKey;
pub use multi_store::MultiContentStore;
pub use sqlite_store::SqliteContentStore;
pub use store::{CompactResult, ContentStore};

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use find_common::config::BackendInstanceConfig;

/// Open a single content store backend from its config entry.
///
/// `dir` is the data directory for this backend (the caller decides whether
/// to use `data_dir` directly or a per-backend subdirectory).
pub fn open_backend(b: &BackendInstanceConfig, dir: &Path) -> Result<Arc<dyn ContentStore>> {
    Ok(Arc::new(
        SqliteContentStore::open(dir, b.chunk_size_kb, b.max_read_connections, b.compress)
            .map_err(|e| anyhow::anyhow!("opening sqlite store '{}': {e:#}", b.name))?,
    ))
}
