# 034 — 7z OOM Crash During Archive Extraction

## Problem

The client crashes with a memory allocation failure when indexing certain `.7z` archives:

```
INFO find_scan::scan: extracting archive backups/Computers/JamieDraftkingsPc-2025-03/documents-pictures.7z (28661 indexed so far)
memory allocation of 126086249 bytes failed
```

The crash is a Rust allocator panic (~120 MB failed allocation), not a graceful error. On
Linux with memory overcommit disabled (WSL2, NAS boxes, containers), `malloc` can return
`NULL`, which Rust converts to an abort/panic with this message. The process terminates,
losing all progress for the remainder of the scan.

---

## Environment

The crash occurs on a Synology NAS running an `armv7-unknown-linux-gnueabihf` (32-bit
ARM) build of find-client. The machine is severely memory-constrained:

```
              total        used        free      shared  buff/cache   available
Mem:          500Mi       146Mi        12Mi       8.0Mi       341Mi       314Mi
Swap:         2.0Gi       266Mi       1.7Gi
```

At baseline the system has only **314 MB available** (including reclaimable cache).
After the scan process has been running for 28,661 files its own heap is meaningfully
larger, so the headroom at the point of the crash is less than the 314 MB baseline
figure. Attempting to allocate a contiguous **120 MB** buffer when under this kind of
pressure will fail.

---

## Root Cause

### Memory pressure on a 500 MB RAM device

The LZMA decoder inside `sevenz_rust2` allocates a dictionary buffer when decoding a
solid block. This buffer is sized to the **LZMA dictionary parameter** stored in the
stream header — not the block's unpack size. The encoder chooses the dictionary size
independently of how much data is actually in the block. A 100 MB block can be encoded
with a 256 MB dictionary, requiring a 256 MB allocation before any file content is seen.

In this crash the requested allocation was **~120 MB** (`126086249` bytes). On a
500 MB system with a long-running scan process this is enough to exhaust available
physical and virtual memory.

### The default threshold (256 MB) is meaningless on this hardware

The code in `sevenz_streaming` (`crates/extractors/archive/src/lib.rs`) pre-computes
oversized blocks using `b.get_unpack_size()` and skips any block exceeding
`cfg.max_7z_solid_block_mb` (default: **256 MB**). On a 500 MB machine this default
blocks nothing useful — it would only trigger for a block larger than half the device's
entire RAM. The 120 MB block that caused this crash passed the check easily.

Additional weaknesses:

1. **`get_unpack_size()` ≠ decoder memory.** The function reports the sum of uncompressed
   member sizes in the block — not the LZMA dictionary size. The dictionary can be
   larger, so even a block that passes the guard may still require more memory than the
   system can provide.

2. **`get_unpack_size()` returns 0 for some blocks.** Solid archives with no
   `SubStreamsInfo` field in the header report 0 here. A zero-size block always passes
   the guard and proceeds to decode with an unknown (potentially large) allocation.

3. **32-bit ARM address space fragmentation.** After thousands of indexing operations the
   32-bit heap is fragmented; contiguous regions shrink over time independent of total
   free memory.

### Why it kills the process

When the LZMA decoder inside `sevenz_rust2` calls `Vec::with_capacity(dict_size)` and
the allocator returns null, Rust calls `handle_alloc_error`. The default implementation
prints `"memory allocation of N bytes failed"` to stderr and then **aborts** the process.
This is not a panic — `catch_unwind` cannot intercept it. The only defence is to avoid
attempting the allocation in the first place.

### Future option: `set_alloc_error_hook` (nightly, tracking issue [#51245](https://github.com/rust-lang/rust/issues/51245))

Rust nightly exposes `std::alloc::set_alloc_error_hook`, which allows replacing the
default abort behaviour with a custom handler. The hook is explicitly permitted to panic:

```rust
#![feature(alloc_error_hook)]
std::alloc::set_alloc_error_hook(|layout| {
    panic!("memory allocation of {} bytes failed", layout.size());
});
```

With this hook installed, OOM inside `sevenz_rust2` would panic instead of abort, and
`std::panic::catch_unwind` around `block_dec.for_each_entries()` **would** catch it.
This would eliminate our dependence on the pre-flight memory check being accurate (the
check would become an optimisation rather than a hard requirement), and would handle the
`get_unpack_size() == 0` and dictionary-exceeds-unpack-size blind spots automatically.

**Watch [#51245](https://github.com/rust-lang/rust/issues/51245) for stabilisation.** If
this lands on stable, we should:
1. Set the hook in `find-client`'s `main()` before the scan loop begins.
2. Reinstate `catch_unwind` around `block_dec.for_each_entries()` in `sevenz_streaming`.
3. Treat the pre-flight check as an optimisation (skip large blocks early to avoid
   unnecessary swap pressure) rather than a hard safety requirement.
4. Remove the `get_unpack_size() == 0` hard-skip added in attempt 2 (filenames could
   be indexed from content again).

---

## History of Attempted Fixes

### Attempt 1 — Block-level size guard + `BlockDecoder` refactor

**Introduced:** somewhere around v0.2.x

**What changed:**
- Replaced the original `sevenz_rust2::decompress_with_extract()` call (which processed
  the whole archive at once) with a manual `BlockDecoder` loop.
- Pre-parsed the archive header with `sevenz_rust2::Archive::read()` to inspect
  `block.get_unpack_size()` for each solid block.
- Added `max_7z_solid_block_mb` config option (default 256 MB).
- Blocks exceeding the limit are skipped; only their filenames are emitted.
- Non-oversized blocks proceed through `block_dec.for_each_entries()`.

**Why it didn't fully fix it:**
The guard uses `get_unpack_size()` which, as explained above, is not the decoder's
actual memory demand. A 120 MB block passes the 256 MB guard and still causes an OOM
abort when the LZMA dictionary is allocated.

---

## Proposed Fix

### Option A — Lower the default threshold

Change `max_7z_solid_block_mb` from 256 to something like **32 MB** in
`crates/common/src/defaults_client.toml`. On a 500 MB RAM device with a long-running
scan process, only small allocations are reliably satisfiable. 32 MB is a reasonable
ceiling: large enough to decode most real-world 7z blocks, small enough to succeed on
constrained hardware late in a long scan.

**Pro:** One line change, immediately deployable. Catches the majority of cases.
**Con:** Still doesn't handle blocks where `get_unpack_size()` is 0 or the dictionary
size exceeds unpack size. The "right" value varies by machine — users with more RAM
may want to raise it.

### Option B — `catch_unwind` around block extraction *(recommended)*

Wrap the `block_dec.for_each_entries()` call in `std::panic::catch_unwind`. On OOM
panic, emit a `skip_reason` for all files in that block (filename-only indexing) and
continue with the next block:

```rust
use std::panic::AssertUnwindSafe;

let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
    block_dec.for_each_entries(&mut |entry, reader| {
        sevenz_process_entry(...)
    })
}));

match result {
    Ok(Ok(())) => {}
    Ok(Err(e)) => { warn!("7z block {block_index}: extraction error: {e:#}"); }
    Err(_panic) => {
        warn!(
            "7z '{}': block {} caused a panic (likely OOM); \
             {} file(s) will be indexed by filename only",
            path.display(), block_index, files_in_block.len()
        );
        // Emit filename-only entries for all files in this block.
        for entry in &files_in_block { callback(...) }
    }
}
```

This requires pre-computing the list of `ArchiveEntry` objects for each block from the
parsed header (so we know which filenames to emit if extraction panics), which is
available from `archive.files` + `archive.stream_map.file_block_index`.

**Pro:** Fully prevents the crash regardless of why the allocation fails.
**Con:** `catch_unwind` does not catch all panics if the `panic=abort` profile is used
in release builds. Must verify the release profile uses `panic=unwind`. Also adds
complexity and means OOM is silently converted to a degraded result — acceptable because
we already do this for oversized blocks.

### Option C — Read LZMA dictionary size from stream header

Parse the LZMA properties byte from the block's `Coder` to extract the actual dictionary
size and use that instead of `get_unpack_size()`. The dictionary size is stored in bytes
2–5 of the LZMA properties (`props[1..5]` as little-endian u32).

`sevenz_rust2::Archive` exposes `blocks[i].coders` which contain the codec ID and
properties. If codec is LZMA/LZMA2, extract the dictionary and use that as the memory
bound.

**Pro:** Addresses the root cause without `catch_unwind`.
**Con:** Requires parsing internal sevenz_rust2 structures; ties us to their internal
layout. The LZMA2 property encoding is different from LZMA. More code, more fragile.

---

## Recommended Approach

**Do both A and B:**

1. Lower `max_7z_solid_block_mb` default from **256 → 64 MB** in
   `crates/common/src/defaults_client.toml`. This catches the majority of cases via the
   existing guard without code changes.

2. Add `catch_unwind` around `block_dec.for_each_entries()` as a safety net for blocks
   that slip through (zero-reported size, dictionary > unpack, edge cases).

3. Verify the release profile does not use `panic = "abort"` in the root `Cargo.toml`
   (it currently doesn't — only `panic = "unwind"` default is in use).

---

## Files to Change

| File | Change |
|------|--------|
| `crates/common/src/defaults_client.toml` | Lower `max_7z_solid_block_mb` from 256 → 64 |
| `crates/extractors/archive/src/lib.rs` | Wrap `block_dec.for_each_entries()` in `catch_unwind`; emit filename-only for panicking blocks |
| `examples/server.toml` / `config/client.toml` | Update any hardcoded defaults in example configs if present |

---

## Testing

1. Create a `.7z` solid archive with a dictionary size larger than the system's available
   free RAM (or with `max_7z_solid_block_mb` temporarily set very low):
   ```sh
   7z a -t7z -m0=lzma -mmt=on -mx=9 -ms=on test-solid.7z large_dir/
   ```
2. Set `max_7z_solid_block_mb = 1` in `config/client.toml` and run `find-scan`.
3. Verify: the scan does **not** crash; the archive's filenames appear in search results;
   an indexing error is recorded for the oversized block.
4. For the `catch_unwind` path: temporarily lower the threshold just enough that the
   `get_unpack_size()` guard doesn't fire, confirm the panic is caught and logged.
