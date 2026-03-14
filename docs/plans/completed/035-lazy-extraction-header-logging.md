# 035 — Lazy Extraction Header Logging

## Problem

When a third-party extractor library (e.g. `lopdf`, `sevenz_rust2`) emits a `warn!` or
`error!` during file extraction, the message appears in the log with no context about
which file caused it:

```
2026-02-28T10:12:58.077171Z WARN lopdf::object: out of memory / memory allocation of 76492800 bytes failed
```

The user has to correlate the timestamp against other `INFO` lines to figure out which
file was being processed. This is especially awkward when many files are being indexed
in sequence and the offending file is buried hundreds of lines back.

---

## Desired Behaviour

When any `WARN` or higher log message fires **during the extraction of a file**, the
output should read:

```
2026-02-28T10:12:58.077171Z INFO find_scan::scan: Processing /nas/backups/report.pdf
2026-02-28T10:12:58.077171Z WARN lopdf::object: out of memory / memory allocation of 76492800 bytes failed
```

The header line (`Processing …`) is emitted **at most once per file** — before the
first diagnostic — and is suppressed entirely for files that produce no warnings. Files
that are indexed cleanly produce no extra output.

---

## Design

### Why a custom tracing `Layer`?

The log messages from third-party crates (`lopdf`, `sevenz_rust2`) arrive via the
`tracing-log` compatibility bridge as ordinary tracing `Event`s. There is no way to
intercept them before they reach the formatter without inserting a custom `Layer` into
the subscriber stack. A `Layer` sees every event before (or after) any other layer and
can emit extra output unconditionally, making it the right insertion point for the
lazy header.

### Redundancy avoidance: target-based filtering

Our own `warn!` calls in `scan.rs` already include the file path in the message body
(e.g. `warn!("extract {}: {}", abs_path.display(), e)`), so emitting a header before
them would be redundant. Third-party crate logs (`lopdf::object`, `sevenz_rust2`, etc.)
do **not** include path context — those are the events the header is for.

The `Layer` checks `event.metadata().target()`. If the target starts with `find_`,
the event comes from our own code and the path is already present — the header is
suppressed. For all other targets (third-party crates), the header fires.

```rust
// Our own warn! calls already include the path — skip the header.
if event.metadata().target().starts_with("find_") {
    return;
}
```

### Thread-local "pending header" state

The extraction path in `scan.rs` is synchronous (both the non-archive
`dispatch_from_path` call and the `spawn_blocking` body for archives). This makes a
**thread-local** the simplest coordination mechanism:

```rust
thread_local! {
    /// Set to Some(path) while a file is being extracted on this thread.
    /// `emitted` flips to true after the header has been written once.
    static PENDING: RefCell<Option<PendingHeader>> = RefCell::new(None);
}

struct PendingHeader {
    path: String,
    emitted: bool,
}
```

The layer's `on_event` inspects `PENDING` on the current thread:

1. If `None` → not in an extraction context; do nothing extra.
2. If `Some(pending)` and `!pending.emitted` → print the header, set `pending.emitted = true`.
3. If `Some(pending)` and `pending.emitted` → header already shown; pass through silently.

### Integration in `scan.rs`

```rust
// Before calling the extractor:
lazy_header::set_pending(abs_path.to_string_lossy().as_ref());

let lines = extract::extract(abs_path, &cfg);  // any warn! fires inside here

// After extraction (success or error):
lazy_header::clear_pending();
```

For the archive path (`spawn_blocking`), set and clear inside the closure:

```rust
let abs_clone = (*abs_path).clone();
let extract_task = tokio::task::spawn_blocking(move || {
    lazy_header::set_pending(abs_clone.to_string_lossy().as_ref());
    let result = find_extract_archive::extract_streaming(&abs_clone, &cfg_clone, &mut |batch| {
        let _ = tx.blocking_send(batch);
    });
    lazy_header::clear_pending();
    result
});
```

### Output formatting

The header is written directly to `stderr` (bypassing the formatter's timestamp and
target fields) to keep it visually distinct from normal tracing output, or it can be
emitted via `eprintln!` as a pre-formatted string. Alternatively, emit it through the
tracing infrastructure using `tracing::info!(target: "find_scan::scan", "Processing {path}")` — this naturally inherits the formatter's timestamp and level prefix, giving a
consistent look at the cost of slight extra complexity in the Layer (calling back into
the subscriber can cause re-entrancy).

**Recommended:** emit the header as a regular `tracing::info!` call from within
`on_event` using `tracing::info!(target: "find_scan::scan", "Processing {path}")`.
The Layer should guard against re-entrancy with a second thread-local bool.

---

## Complexity Assessment: **Medium-Low**

| Concern | Detail |
|---------|--------|
| New code | ~80 lines: `crates/client/src/lazy_header.rs` |
| Subscriber wiring | 2 lines in `scan_main.rs` (add layer to registry) |
| Call-site changes | ~6 lines in `scan.rs` (set/clear before/after each extraction branch) |
| Third-party logs | Handled automatically — no changes to extractor crates |
| Watch mode | `find-watch` spawns extractors as sub-processes; their stderr is already re-emitted. No changes needed. |
| Re-entrancy guard | Simple thread-local `bool` to prevent recursive header emission |
| Risk | Low — purely additive; no existing logic is modified |

The hardest part is the `Layer` implementation itself, which requires familiarity with
the `tracing_subscriber` `Layer` trait. However, the implementation is short and
self-contained; no span extension machinery is needed.

---

## Files to Change

| File | Change |
|------|--------|
| `crates/client/src/lazy_header.rs` | **New.** `set_pending()`, `clear_pending()`, `FileHeaderLayer` struct + `Layer` impl |
| `crates/client/src/scan_main.rs` | Register `FileHeaderLayer` in the subscriber stack |
| `crates/client/src/scan.rs` | `set_pending` / `clear_pending` calls around both extraction branches |

`find-watch` is **not** changed — it re-emits subprocess stderr lines through its own
tracing setup, and those lines already contain path context from the subprocess output.

---

## Example Layer Skeleton

```rust
use std::cell::RefCell;
use tracing::{Event, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

thread_local! {
    static PENDING: RefCell<Option<PendingHeader>> = RefCell::new(None);
    static IN_HEADER: RefCell<bool> = RefCell::new(false);
}

pub struct PendingHeader {
    pub path: String,
    pub emitted: bool,
}

pub fn set_pending(path: &str) {
    PENDING.with(|p| *p.borrow_mut() = Some(PendingHeader { path: path.to_owned(), emitted: false }));
}

pub fn clear_pending() {
    PENDING.with(|p| *p.borrow_mut() = None);
}

pub struct FileHeaderLayer;

impl<S: Subscriber> Layer<S> for FileHeaderLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // Only fire for WARN and above.
        if *event.metadata().level() > tracing::Level::WARN {
            return;
        }
        // Our own warn! calls already include the path — no header needed.
        if event.metadata().target().starts_with("find_") {
            return;
        }
        // Guard against re-entrancy (the header emit below would recurse).
        let already_in_header = IN_HEADER.with(|h| *h.borrow());
        if already_in_header {
            return;
        }
        PENDING.with(|p| {
            let mut pending = p.borrow_mut();
            if let Some(ref mut hdr) = *pending {
                if !hdr.emitted {
                    hdr.emitted = true;
                    IN_HEADER.with(|h| *h.borrow_mut() = true);
                    tracing::info!(target: "find_scan::scan", "Processing {}", hdr.path);
                    IN_HEADER.with(|h| *h.borrow_mut() = false);
                }
            }
        });
    }
}
```

Registration in `scan_main.rs`:

```rust
tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,find_scan=info".into()))
    .with(lazy_header::FileHeaderLayer)
    .with(tracing_subscriber::fmt::layer().with_filter(LogIgnoreFilter))
    .init();
```

---

## Testing

1. Index a PDF that causes a `lopdf` warning (or temporarily lower `max_size_kb` to
   trigger the size-limit path on a large file).
2. Confirm the log shows `INFO Processing /path/to/file.pdf` immediately before the
   `WARN` line, and only once per file.
3. Confirm that files with no warnings produce no `Processing` header.
4. Index multiple warning-producing files in sequence — confirm each gets its own
   header and they do not bleed into each other.
