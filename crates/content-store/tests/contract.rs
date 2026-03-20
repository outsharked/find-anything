//! Shared `ContentStore` contract tests.
//!
//! Every `ContentStore` implementation must pass this full suite.  The
//! `contract_tests!` macro stamps out a `mod` for each concrete type so each
//! case appears in test output as e.g.
//!   `contract::sqlite_store::get_lines_roundtrip`

use std::collections::HashSet;
use std::sync::Arc;
use find_content_store::{ContentKey, ContentStore, MultiContentStore, SqliteContentStore};
use tempfile::TempDir;

// ── Per-store setup helpers ───────────────────────────────────────────────────

fn make_sqlite_store() -> (SqliteContentStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = SqliteContentStore::open(dir.path(), None, None, None).unwrap();
    (store, dir)
}

fn make_sqlite_store_compressed() -> (SqliteContentStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = SqliteContentStore::open(dir.path(), None, None, Some(true)).unwrap();
    (store, dir)
}

fn k(s: &str) -> ContentKey {
    ContentKey::new(s)
}

// 64-hex-char keys used across tests
const K1: &str = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
const K2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const K3: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

// ── Contract test functions ───────────────────────────────────────────────────
// Each takes a `&dyn ContentStore` so they work with any implementation.

fn tc_put_then_contains(store: &dyn ContentStore) {
    let key = k(K1);
    assert!(!store.contains(&key).unwrap());
    store.put(&key, "line0\nline1\nline2").unwrap();
    assert!(store.contains(&key).unwrap());
}

fn tc_put_idempotent(store: &dyn ContentStore) {
    let key = k(K1);
    assert!(store.put(&key, "hello").unwrap(), "first put should return true");
    assert!(!store.put(&key, "hello").unwrap(), "second put should return false");
}

fn tc_get_lines_roundtrip(store: &dyn ContentStore) {
    let key = k(K1);
    let blob = "alpha\nbeta\ngamma\ndelta";
    store.put(&key, blob).unwrap();

    let lines = store.get_lines(&key, 0, 3).unwrap().unwrap();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], (0, "alpha".to_string()));
    assert_eq!(lines[1], (1, "beta".to_string()));
    assert_eq!(lines[2], (2, "gamma".to_string()));
    assert_eq!(lines[3], (3, "delta".to_string()));
}

fn tc_get_lines_sub_range(store: &dyn ContentStore) {
    let key = k(K1);
    store.put(&key, "a\nb\nc\nd\ne").unwrap();

    let lines = store.get_lines(&key, 1, 3).unwrap().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], (1, "b".to_string()));
    assert_eq!(lines[1], (2, "c".to_string()));
    assert_eq!(lines[2], (3, "d".to_string()));
}

fn tc_get_lines_key_not_found(store: &dyn ContentStore) {
    let key = k(K1);
    assert!(store.get_lines(&key, 0, 5).unwrap().is_none());
}

fn tc_delete_removes_blob(store: &dyn ContentStore) {
    let key = k(K1);
    store.put(&key, "some content").unwrap();
    assert!(store.contains(&key).unwrap());
    store.delete(&key).unwrap();
    assert!(!store.contains(&key).unwrap());
    assert!(store.get_lines(&key, 0, 0).unwrap().is_none());
}

fn tc_compact_removes_orphaned_blobs(store: &dyn ContentStore) {
    let k_live   = k(K1);
    let k_orphan = k(K2);
    store.put(&k_live,   "live content").unwrap();
    store.put(&k_orphan, "orphaned content").unwrap();

    let live: HashSet<ContentKey> = [k_live.clone()].into_iter().collect();
    store.compact(&live, false).unwrap();

    assert!(store.contains(&k_live).unwrap());
    assert!(!store.contains(&k_orphan).unwrap());
}

fn tc_compact_dry_run_does_not_remove(store: &dyn ContentStore) {
    let key = k(K1);
    store.put(&key, "content").unwrap();

    let live: HashSet<ContentKey> = HashSet::new(); // key is orphan
    store.compact(&live, true).unwrap();

    assert!(store.contains(&key).unwrap(), "dry_run must not delete anything");
}

fn tc_empty_blob_stored_and_retrievable(store: &dyn ContentStore) {
    let key = k(K1);
    store.put(&key, "").unwrap();
    assert!(store.contains(&key).unwrap());
    // get_lines on an empty blob should return Some (key exists) with no lines.
    let lines = store.get_lines(&key, 0, 0).unwrap().unwrap();
    assert!(lines.is_empty());
}

fn tc_multi_chunk_all_lines_retrievable(store: &dyn ContentStore) {
    // 50 lines × ~10 chars = ~500 bytes; a 1 KB chunk size means at least one
    // split at some point. Both stores must reassemble the full range correctly.
    let key = k(K1);
    let lines: Vec<String> = (0..50).map(|i| format!("line {:04}", i)).collect();
    let blob = lines.join("\n");
    store.put(&key, &blob).unwrap();

    let result = store.get_lines(&key, 0, 49).unwrap().unwrap();
    assert_eq!(result.len(), 50, "all 50 lines must be returned");
    for (pos, content) in &result {
        assert_eq!(content, &format!("line {:04}", pos));
    }
}

fn tc_get_lines_boundary(store: &dyn ContentStore) {
    // Request a range that exactly matches the blob boundaries.
    let key = k(K1);
    store.put(&key, "only\ntwo").unwrap();

    let lines = store.get_lines(&key, 0, 1).unwrap().unwrap();
    assert_eq!(lines.len(), 2);

    // Request beyond the end — should return whatever lines exist, not error.
    let lines = store.get_lines(&key, 0, 999).unwrap().unwrap();
    assert_eq!(lines.len(), 2);
}

fn tc_compact_multiple_orphans(store: &dyn ContentStore) {
    // Three blobs; only the first is live.
    let k1 = k(K1);
    let k2 = k(K2);
    let k3 = k(K3);
    store.put(&k1, "live").unwrap();
    store.put(&k2, "orphan a").unwrap();
    store.put(&k3, "orphan b").unwrap();

    let live: HashSet<ContentKey> = [k1.clone()].into_iter().collect();
    store.compact(&live, false).unwrap();

    assert!(store.contains(&k1).unwrap());
    assert!(!store.contains(&k2).unwrap());
    assert!(!store.contains(&k3).unwrap());
}

// ── Macro to stamp out the suite per implementation ──────────────────────────

macro_rules! contract_tests {
    ($mod_name:ident, $make_store:expr) => {
        mod $mod_name {
            use super::*;

            fn store() -> (impl ContentStore, TempDir) { $make_store }

            #[test] fn put_then_contains()              { let (s,_t)=store(); tc_put_then_contains(&s); }
            #[test] fn put_idempotent()                 { let (s,_t)=store(); tc_put_idempotent(&s); }
            #[test] fn get_lines_roundtrip()            { let (s,_t)=store(); tc_get_lines_roundtrip(&s); }
            #[test] fn get_lines_sub_range()            { let (s,_t)=store(); tc_get_lines_sub_range(&s); }
            #[test] fn get_lines_key_not_found()        { let (s,_t)=store(); tc_get_lines_key_not_found(&s); }
            #[test] fn delete_removes_blob()            { let (s,_t)=store(); tc_delete_removes_blob(&s); }
            #[test] fn compact_removes_orphaned_blobs() { let (s,_t)=store(); tc_compact_removes_orphaned_blobs(&s); }
            #[test] fn compact_dry_run()                { let (s,_t)=store(); tc_compact_dry_run_does_not_remove(&s); }
            #[test] fn empty_blob()                     { let (s,_t)=store(); tc_empty_blob_stored_and_retrievable(&s); }
            #[test] fn multi_chunk_all_lines()          { let (s,_t)=store(); tc_multi_chunk_all_lines_retrievable(&s); }
            #[test] fn get_lines_boundary()             { let (s,_t)=store(); tc_get_lines_boundary(&s); }
            #[test] fn compact_multiple_orphans()       { let (s,_t)=store(); tc_compact_multiple_orphans(&s); }
        }
    };
}

/// Returns a `MultiContentStore` wrapping two `SqliteContentStore` instances,
/// both residing in subdirectories of a single `TempDir` so the macro's
/// `(impl ContentStore, TempDir)` return type is satisfied.
fn make_multi_store() -> (MultiContentStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let sub_a = dir.path().join("a");
    let sub_b = dir.path().join("b");
    std::fs::create_dir_all(&sub_a).unwrap();
    std::fs::create_dir_all(&sub_b).unwrap();
    let s1 = SqliteContentStore::open(&sub_a, None, None, None).unwrap();
    let s2 = SqliteContentStore::open(&sub_b, None, None, None).unwrap();
    let store = MultiContentStore { stores: vec![Arc::new(s1), Arc::new(s2)] };
    (store, dir)
}

contract_tests!(sqlite_store,           make_sqlite_store());
contract_tests!(sqlite_store_compressed, make_sqlite_store_compressed());
contract_tests!(multi_store,            make_multi_store());

// ── MultiContentStore-specific behaviour ─────────────────────────────────────

#[test]
fn multi_put_writes_to_all_backends() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let sa: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_a.path(), None, None, None).unwrap());
    let sb: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_b.path(), None, None, None).unwrap());

    let m = MultiContentStore { stores: vec![Arc::clone(&sa), Arc::clone(&sb)] };
    let key = k(K1);
    m.put(&key, "hello world content").unwrap();

    assert!(sa.contains(&key).unwrap(), "primary backend should contain the key");
    assert!(sb.contains(&key).unwrap(), "secondary backend should also contain the key");
}

#[test]
fn multi_get_lines_reads_from_first_hit() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let sa: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_a.path(), None, None, None).unwrap());
    let sb: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_b.path(), None, None, None).unwrap());

    // Only put the key in the secondary store.
    let key = k(K1);
    sb.put(&key, "secondary content").unwrap();

    let m = MultiContentStore { stores: vec![Arc::clone(&sa), Arc::clone(&sb)] };

    // get_lines should fall through to sb and return the content.
    let lines = m.get_lines(&key, 0, 0).unwrap();
    assert!(lines.is_some(), "should find key in secondary store");
    assert_eq!(lines.unwrap()[0].1, "secondary content");
}

#[test]
fn multi_storage_stats_sums_backends() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let sa: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_a.path(), None, None, None).unwrap());
    let sb: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_b.path(), None, None, None).unwrap());

    sa.put(&k(K1), "content for store a").unwrap();
    sb.put(&k(K2), "content for store b").unwrap();

    let m = MultiContentStore { stores: vec![Arc::clone(&sa), Arc::clone(&sb)] };
    let stats = m.storage_stats();
    assert!(stats.is_some(), "multi store should report stats when backends have data");
    let (count, bytes) = stats.unwrap();
    assert!(count >= 2, "should count entries from both backends (got {count})");
    assert!(bytes > 0, "should sum bytes from both backends");
}

#[test]
fn multi_compact_runs_all_backends() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let sa: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_a.path(), None, None, None).unwrap());
    let sb: Arc<dyn ContentStore> = Arc::new(SqliteContentStore::open(dir_b.path(), None, None, None).unwrap());

    // Put orphaned keys in both backends.
    let orphan_a = k(K1);
    let orphan_b = k(K2);
    sa.put(&orphan_a, "orphan in a").unwrap();
    sb.put(&orphan_b, "orphan in b").unwrap();

    let m = MultiContentStore { stores: vec![Arc::clone(&sa), Arc::clone(&sb)] };

    // Compact with empty live set — both orphans should be removed.
    let result = m.compact(&HashSet::new(), false).unwrap();
    assert!(result.chunks_removed >= 2, "should remove orphans from both backends (got {})", result.chunks_removed);
    assert!(!sa.contains(&orphan_a).unwrap(), "orphan in primary should be gone");
    assert!(!sb.contains(&orphan_b).unwrap(), "orphan in secondary should be gone");
}

#[test]
fn multi_empty_stores_returns_none_for_stats() {
    let m = MultiContentStore { stores: vec![] };
    assert!(m.storage_stats().is_none(), "empty multi store should return None for stats");
    assert!(!m.contains(&k(K1)).unwrap(), "empty multi store should not contain any key");
    assert!(m.get_lines(&k(K1), 0, 0).unwrap().is_none());
}
