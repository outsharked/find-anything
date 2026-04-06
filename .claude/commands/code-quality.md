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
- `crates/server/src/worker/` (glob all .rs files — was split from a monolith)
- `crates/server/src/db/` (glob all .rs files)
- `crates/server/src/routes/` (glob all .rs files)
- `crates/server/src/normalize.rs`

**Client core:**
- `crates/client/src/scan.rs`
- `crates/client/src/watch.rs`
- `crates/client/src/batch.rs`

**Shared types:**
- `crates/common/src/api.rs`
- `crates/common/src/config.rs`
- `crates/extract-types/src/extractor_config.rs`
- `crates/extractors/dispatch/src/lib.rs`

**Web UI:**
- `web/src/routes/+page.svelte`
- `web/src/lib/` (glob all `.ts` / `.svelte` files)

Also run `find crates -name "*.rs" | xargs wc -l | sort -rn | head -20` to identify the largest files.

---

## Phase 2 — What to look for

For each finding, record: **location** (file:line if possible), **category**, **severity** (High / Medium / Low), and a **concrete recommendation**.

### 1. Function/method argument bloat (> 4 parameters)
Long argument lists are a symptom of missing config structs or missing abstraction.

**Flag:** any function with ≥ 5 parameters that could instead receive a struct. Pay special attention to:
- Boolean parameters — `fn foo(skip: bool)` makes call sites unreadable. Flag any `bool` param that could be an enum variant or a separate method.
- Parameters that always travel together — two or more params that appear at every call site together belong in a struct.

### 2. Deep dependency chains / tight coupling
- Does any single file import from 10+ modules?
- Do route handlers reach directly into `db::` bypassing the worker? (Invariant: only worker writes to SQLite.)
- Do extractors import anything beyond `find-extract-types`?
- Is dispatch order in `crates/extractors/dispatch/src/lib.rs` a load-bearing invariant with no type enforcement?

### 3. Panic risk in production code
- Bare `unwrap()` or `expect()` calls **outside** `#[test]` or `#[cfg(test)]` blocks. Each is a latent panic.
- `unwrap_or_default()` on non-trivial types where the default silently masks an error.
- Index operations (`slice[i]`) without bounds checks in hot paths.

### 4. Complex logic without tests
Focus on non-trivial logic that has no corresponding `#[test]` block or integration test:
- Worker phase 1 and phase 2 processing
- `scan.rs` subdirectory-rescan and archive sentinel logic
- `watch.rs` accumulator collapse rules (`Create+Modify`, `Delete+Create`, directory rename detection)
- `db/` functions for pruning, deduplication, FTS5 population

**For each:** describe what could go wrong with no test coverage and what a useful test would exercise.

### 5. Duplication of complex logic
Look for logic implemented more than once across the codebase that could drift:
- Content read paths across multiple routes — do they share a helper or duplicate open/seek/decode logic?
- Archive member path parsing (`::` separator): is it split/joined in multiple places?
- Batch submission logic: is there duplication between `scan.rs` and `watch.rs`?
- Magic constants (numeric literals, string sentinels) that appear in multiple files instead of being imported from a single definition.

### 6. Implicit ordering dependencies not enforced by types
Look for sequencing logic that is maintained only by convention or comments, not enforced by the type system:
- "Must do X before Y" relationships where doing them in the wrong order compiles fine but corrupts state.
- Multi-phase pipelines where phase identity is tracked by convention (e.g. naming) rather than typestate or separate types.
- Shared mutable state that is valid only within a certain window.

### 7. SOLID violations
- **Single Responsibility:** Does any module handle I/O, business logic, and error reporting all at once?
- **Open/Closed:** If a new file kind needs special handling, how many files need to change?
- **Dependency Inversion:** Do high-level modules depend on concrete implementations rather than traits/interfaces?
- **Interface Segregation:** Are any structs/enums used as catch-alls that force callers to handle fields they don't care about?

### 8. Error handling quality
- Functions that swallow errors with `let _ =` in non-trivial paths
- Large `match` blocks on `Result`/`Option` that obscure the happy path
- Errors logged but not propagated where propagation would enable retry or caller decision

### 9. Concurrency and blocking
- `spawn_blocking` closures that clone large data structures or `Arc<dyn Trait>` by value instead of passing a reference — signals a design issue and wastes memory.
- Blocking I/O (file reads, DB queries) called directly in async context without `spawn_blocking`.
- Shared state protected by `Mutex` where the critical section is larger than necessary.

### 10. State management complexity (web UI)
In `+page.svelte`:
- How many reactive variables track "the same conceptual thing"? Group them and flag redundancy.
- Is there a clear state machine or is control flow spread across reactive blocks?
- Are derived values recomputed in multiple places rather than declared once?

### 11. Oversized files
Any `.rs` file > 600 lines is a candidate for splitting. Any `.svelte` file > 400 lines similarly.
Identify what logical units could be extracted and where they would live.

### 12. Evidence verification gate
Before recording any finding, verify it with direct evidence from the code you read. Do not report a problem you did not actually observe. If you are uncertain whether something is an issue, say so explicitly and describe what you saw.

---

## Phase 3 — Output format

Present findings as a **prioritised list**, grouped by severity. Use this structure:

```
## High Priority

### [H1] Title — file.rs:line
**Category:** (Argument Bloat / Boolean Param / Panic Risk / Duplication / Missing Tests / Coupling / Implicit Ordering / SOLID / Error Handling / Concurrency / UI Complexity / Oversized File)
**Problem:** Concrete description of the issue, with the actual code or pattern observed.
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
