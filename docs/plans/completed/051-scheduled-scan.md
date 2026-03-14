# 051 — Scheduled `find-scan` in `find-watch`

## Overview

`find-watch` misses directory renames entirely (the OS emits a single directory-level
event which is silently skipped). A periodic full `find-scan` run catches these and
any other gaps. The scheduler lives inside `find-watch` so no new process or system
cron job is needed.

## Design Decisions

**Where the scheduler lives**
`find-watch` is already long-running and holds the client config. Adding a background
task there is the simplest option. `find-server` does not have `client.toml` and is
the wrong place. A separate daemon adds unnecessary overhead.

**No automatic startup scan**
By default the first scheduled scan fires after the first full interval. This avoids
a potentially expensive scan on every service restart. Pass `--scan-now` to trigger
one immediately at startup (useful for one-off invocations and testing).

**Binary resolution**
`std::env::current_exe()` returns the path to `find-watch`. Replace the filename
with `find-scan` (+ `.exe` on Windows). This is reliable even when the install
directory is not on `PATH` — which is common for systemd services and Windows services.

**Overlap prevention**
Track the running child as `Option<tokio::process::Child>`. Before each scheduled
tick, call `.try_wait()`. If the child is still running, log a warning and skip that
tick instead of spawning a second concurrent scan.

**Missed ticks**
Use `tokio::time::MissedTickBehavior::Skip` so a system sleep/suspend that spans
multiple intervals fires exactly one scan on wake-up, not a burst.

**Config**
Add `scan_interval_hours: f64` to `[watch]` in `client.toml`.
- Default: `24.0` (once per day)
- Set to `0.0` to disable scheduled scanning entirely
- Fractional values supported (e.g. `0.5` = every 30 minutes; handy for testing)

**Stdio**
Inherit parent stdout/stderr so `find-scan` log output flows through the same stream
(journald, Windows Event Log, terminal) without any extra plumbing.

**Config path threading**
`run_watch` gains a `WatchOptions` struct (per project convention for >1 extra param)
containing `config_path: String` and `scan_now: bool`. The scheduler passes
`--config <config_path>` to the child so it uses the same source definitions.

## Implementation

### `crates/common/src/config.rs`

Add to `WatchConfig`:
```rust
/// Run find-scan on this interval. Set to 0.0 to disable.
#[serde(default = "default_scan_interval_hours")]
pub scan_interval_hours: f64,
```
Default: `24.0`.
Add `fn default_scan_interval_hours() -> f64 { client_defaults().watch.scan_interval_hours }`.
Update `defaults_client.toml` with the new key.

### `crates/client/src/watch.rs`

Add `WatchOptions { config_path: String, scan_now: bool }`.
Change `run_watch` signature to `run_watch(config: &ClientConfig, opts: &WatchOptions)`.
Spawn `start_scan_scheduler(config, opts)` as a background task before the event loop.

`start_scan_scheduler`:
1. If `scan_now`, spawn `find-scan --config <path>` immediately and await it.
2. Enter interval loop (`MissedTickBehavior::Skip`, period = `scan_interval_hours` hours).
3. Each tick: check `child.try_wait()`; if still running, log warning and skip.
4. Otherwise spawn child, store handle, continue (don't await — let the event loop run).
5. On the next tick, check again and await or skip accordingly.

Binary resolution helper:
```rust
fn find_scan_binary() -> PathBuf {
    let name = if cfg!(windows) { "find-scan.exe" } else { "find-scan" };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}
```

### `crates/client/src/watch_main.rs`

Add `--scan-now` / `-S` flag to `Args`.
Build `WatchOptions` from args and pass to `run_watch`.
Update Windows `service_entry` to pass `WatchOptions { config_path, scan_now: false }`.

## Files Changed

- `crates/common/src/config.rs` — add `scan_interval_hours` to `WatchConfig`
- `crates/client/src/watch.rs` — `WatchOptions`, `start_scan_scheduler`, update `run_watch`
- `crates/client/src/watch_main.rs` — `--scan-now` flag, pass `WatchOptions`
- `defaults_client.toml` — document new key

## Testing

- `find-watch --scan-now` should log a `find-scan` invocation immediately at startup.
- With `scan_interval_hours = 0.0167` (≈1 min), verify the scheduler fires and
  doesn't overlap if the scan takes longer than the interval.
- Rename a directory while `find-watch` is running; verify old paths are eventually
  removed and new paths appear after the next scheduled scan.

## Breaking Changes

None. `scan_interval_hours` has a default; existing configs continue to work unchanged.
