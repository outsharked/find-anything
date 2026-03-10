---
description: Analyse the codebase for code quality issues — coupling, untested complexity, duplication, argument bloat, SOLID violations
---

You are performing a code quality review of the find-anything project. This is a Rust workspace (server + client + extractor crates) with a SvelteKit web UI.

## What to do

Read the key source files, then produce a **prioritised findings report**. Do not make any code changes — analysis only.

---

## Phase 1 — Gather context (run all reads in parallel where possible)

Read these files to understand the current structure:

**Server core:**
- `crates/server/src/worker.rs`
- `crates/server/src/archive.rs`
- `crates/server/src/db/mod.rs`
- `crates/server/src/routes.rs` (or `crates/server/src/routes/` directory)

**Client core:**
- `crates/client/src/scan.rs`
- `crates/client/src/watch.rs`
- `crates/client/src/batch.rs`

**Shared types:**
- `crates/common/src/api.rs`
- `crates/common/src/config.rs`
- `crates/extract-types/src/extractor_config.rs`

**Web UI:**
- `web/src/routes/+page.svelte`
- `web/src/lib/` (glob all `.ts` / `.svelte` files)

Also run `find crates -name "*.rs" | xargs wc -l | sort -rn | head -20` to identify the largest files.

---

## Phase 2 — What to look for

For each finding, record: **location** (file:line if possible), **category**, **severity** (High / Medium / Low), and a **concrete recommendation**.

### 1. Function/method argument bloat (> 4 parameters)
Long argument lists are a symptom of missing config structs or missing abstraction. Look especially in:
- Worker processing functions (phase1, phase2 callsites)
- Extractor lib.rs extract functions
- DB helper functions

**Flag:** any function with ≥ 5 parameters that could instead receive a struct.

### 2. Deep dependency chains / tight coupling
- Does any single file import from 10+ modules?
- Do route handlers reach directly into `db::` bypassing the worker? (Invariant: only worker writes to SQLite.)
- Do extractors import anything beyond `find-extract-types`?
- Does `worker.rs` directly instantiate archive logic, or go through a clean interface?

### 3. Complex logic without tests
Focus on non-trivial logic that has no corresponding `#[test]` block or integration test:
- Archive chunk routing and ZIP rewrite logic in `archive.rs`
- Inbox worker batch processing (phase 1 and phase 2)
- `scan.rs` subdirectory-rescan and archive sentinel logic
- `watch.rs` accumulator collapse rules (`Create+Modify`, `Delete+Create`, directory rename detection)
- `db.rs` functions for pruning, deduplication, FTS5 population

**For each:** describe what could go wrong with no test coverage and what a useful test would exercise.

### 4. Duplication of complex logic
Look for logic implemented more than once across the codebase that could drift:
- Chunk read paths: does `routes/search.rs`, `routes/context.rs`, and `routes/file.rs` each open ZIPs independently? Is the cache logic duplicated?
- Inline-content fallback (SQLite vs ZIP): is the branch duplicated across routes?
- Archive member path parsing (`::` separator): is it split/joined in multiple places?
- Batch submission logic: is there any duplication between `scan.rs` and `watch.rs` batch assembly?

### 5. SOLID violations
- **Single Responsibility:** Is `worker.rs` doing too many things? (SQLite writes, activity logging, rename handling, archive queuing, logging, error handling all in one file)
- **Open/Closed:** If a new file kind needs special handling, how many files need to change?
- **Dependency Inversion:** Do high-level modules (worker, routes) depend on concrete implementations rather than traits/interfaces?
- **Interface Segregation:** Are any structs/enums used as catch-alls that force callers to handle fields they don't care about?

### 6. Error handling quality
- Functions that swallow errors with `unwrap_or_default()` or `let _ =` in non-trivial paths
- Large `match` blocks on `Result`/`Option` that obscure the happy path
- Errors logged but not propagated where propagation would enable retry or caller decision

### 7. State management complexity (web UI)
In `+page.svelte`:
- How many reactive variables (`$state`, `let`, writable stores) track "the same conceptual thing"?
- Is there a clear state machine or is control flow spread across reactive blocks?
- Are any derived values recomputed in multiple places?

### 8. Oversized files
Any `.rs` file > 600 lines is a candidate for splitting. Any `.svelte` file > 400 lines similarly.
Identify what logical units could be extracted.

---

## Phase 3 — Output format

Present findings as a **prioritised list**, grouped by severity. Use this structure:

```
## High Priority

### [H1] Title — file.rs:line
**Category:** (Argument Bloat / Duplication / Missing Tests / Coupling / SOLID / Error Handling / UI Complexity)
**Problem:** Concrete description of the issue.
**Risk:** What could go wrong or is already painful.
**Recommendation:** Specific, actionable fix.

---
```

Repeat for Medium and Low priority.

End with a **Summary table**:
| # | Severity | Category | File | One-line description |
|---|----------|----------|------|---------------------|

Keep findings concrete and specific — no generic advice. Every finding must name a real file and describe actual code observed, not hypothetical issues.

If `$ARGUMENTS` is provided, treat it as a focus area (e.g. "focus on server" or "focus on tests") and scope the analysis accordingly.
