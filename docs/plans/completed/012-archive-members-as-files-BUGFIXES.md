# Plan 012: Archive Members as Files - Bug Fixes

## Bugs Identified and Fixed

### Bug #1: Archive members inherit kind="archive" from outer file ✅ FIXED

**Location:** `crates/client/src/scan.rs:186-204`

**Problem:**
When creating `IndexFile` entries for archive members, all members were assigned `kind: kind.clone()` which copied the kind from the outer archive file ("archive"), instead of detecting each member's actual kind based on its filename.

**Impact:**
- Archive members appeared as kind="archive" in Ctrl+P and tree views
- Archive members were incorrectly expandable in the tree (they have no sub-members)
- Clicking on them would attempt to expand, show no results, appearing as "nothing happens"
- FileViewer would show wrong file type indicator

**Root Cause:**
In `build_index_files()`, the `kind` variable held the outer archive's kind ("archive"), and this was cloned for all inner members.

**Fix:**
Detect each member's actual kind from its filename:
```rust
let member_kind = extract::detect_kind(std::path::Path::new(&member)).to_string();
```

This ensures:
- `report.pdf` inside `data.zip` gets kind="pdf"
- `readme.txt` inside `data.zip` gets kind="text"
- `photo.jpg` inside `data.zip` gets kind="image"
- Nested archives like `inner.tar.gz` inside `outer.zip` correctly get kind="archive"

**Verification:**
After re-indexing:
1. Archive members in Ctrl+P should show correct file type
2. PDF members should not be expandable in the tree
3. Text members should open in FileViewer correctly
4. Only nested archives (e.g., `.tar.gz` inside a `.zip`) should be expandable

---

## Documentation Updates ✅ COMPLETED

Updated `CLAUDE.md` with comprehensive notes about archive members:
- Composite path structure using `::`
- Kind detection for members
- Deletion and re-indexing semantics
- Tree browsing behavior
- Ctrl+P integration
- Archive depth limit to prevent zip bombs

---

## Re-indexing Required

After this fix, users must **re-run `find-scan`** to rebuild the index with correct `kind` values for archive members. The fix only affects new indexing operations; existing database entries retain the wrong kind until re-indexed.

---

## Follow-up Considerations

### Potential Additional UX Issues to Monitor:

1. **Empty archives:** Archives with no extractable content should still be expandable but show "No files" or similar message
2. **Deeply nested archives:** UI should handle multi-level nesting gracefully (e.g., `a.zip::b.tar::c.gz::file.txt`)
3. **Archive expansion loading state:** TreeRow should show loading indicator while fetching members
4. **Error handling:** Better user feedback when archive extraction fails or depth limit is exceeded

### Schema Consideration:

The current approach detects member kind from filename extension. For members without extensions or with misleading extensions (e.g., `data` or `readme`), kind detection falls back to "text". This is acceptable but could be improved in the future by:
- Storing detected kind during extraction (before composite paths are created)
- Using content inspection for extensionless files
- Adding a `detected_kind` field to `IndexLine` during extraction
