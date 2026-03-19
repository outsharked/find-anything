mod key;
mod multi_store;
mod sqlite_store;
mod store;
pub mod zip_store;

pub use key::ContentKey;
pub use multi_store::MultiContentStore;
pub use sqlite_store::SqliteContentStore;
pub use store::{CompactResult, ContentStore};
pub use zip_store::ZipContentStore;

/// Temporarily exported internals used by the `find-server` transition shim.
/// Removed when step 15 is complete.
#[doc(hidden)]
pub mod _internal {
    pub use crate::zip_store::shared::SharedArchiveState;
    pub use crate::zip_store::archive::{ArchiveManager, ChunkRef};
    pub use crate::zip_store::chunk::{chunk_lines, Chunk, ChunkRange, ChunkResult};
}
