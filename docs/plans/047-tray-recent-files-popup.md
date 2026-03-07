# Tray Recent Files Popup

## Overview

Left-clicking the tray icon shows a small borderless popup window listing the
20 most recently indexed files (like Process Hacker's process summary popup).
Right-clicking continues to show the existing context menu.

Polling is demand-driven: the background poller only runs while the popup is
open, not continuously.  A single immediate poll fires on any tray click so the
menu file count is always fresh when the user opens it.

## Design Decisions

### Data source

New `GET /api/v1/recent?limit=N` server endpoint.  Queries each per-source
SQLite DB for the N most recently indexed files (`ORDER BY indexed_at DESC`),
merges across sources, re-sorts, and returns the top N.  Archive members
(`path LIKE '%::%'`) are excluded — only outer files are shown.

Using the server rather than parsing find-watch logs keeps the data accurate
and avoids Windows Event Log complexity.

### Popup window

A borderless Win32 `WS_POPUP | WS_BORDER | WS_CLIPCHILDREN` window containing
a single `WS_CHILD | WS_VISIBLE | LBS_NOSEL | WS_VSCROLL | LBS_NOINTEGRALHEIGHT`
LISTBOX child that fills the client area.  Win32 handles all drawing and
scrolling natively.

Positioned above the taskbar at the bottom-right of the work area
(`SystemParametersInfoW(SPI_GETWORKAREA, ...)`).

Auto-dismisses when the window loses activation (`WM_ACTIVATE` with
`WA_INACTIVE`).  Also dismisses on Escape (`WM_KEYDOWN`).

The `WndProc` lives in a static `unsafe extern "system"` fn; the popup `HWND`
is stored in `TrayApp` and created once (lazily on first left-click).

### Polling architecture

`poller::spawn()` returns a `PollerHandle` (wraps `Arc<AtomicBool>`).  The
background thread loops:

```
if popup_open.load() {
    query server + SCM, send AppEvent
    sleep poll_interval_ms
} else {
    sleep 100 ms
}
```

`TrayApp` holds the `PollerHandle` and calls:
- `handle.set_active(true)` when popup opens
- `handle.set_active(false)` when popup closes
- `handle.poll_once()` (sets active briefly via a separate `AtomicBool` flag)
  on right-click, so menu file counts are fresh

### Poll interval — configurable, not a magic number

The active poll interval defaults to **1 second** and is read from
`client.toml` under a new `[tray]` section:

```toml
[tray]
# poll_interval_ms = 1000   # Refresh interval while popup is open (ms)
```

`ClientConfig` gains a `tray: TrayConfig` field; `TrayConfig` has
`poll_interval_ms: u64` with a default of `1000`.  The tray app passes this
value to `poller::spawn()` at startup so there are no magic numbers in the
poller code.

The idle-check interval (100 ms) is an internal implementation detail and does
not need to be configurable.

### AppEvent

Add `recent_files: Vec<RecentFile>` to the existing `StatusUpdate` variant.
When the popup is closed, the poller is idle so `recent_files` will be the
last-seen slice; it gets refreshed as soon as the popup opens.

### Display format

Each listbox row: `[source]  basename  (parent dir)` e.g.
```
[home]  report.docx   Documents
[work]  main.rs       src
```
Full path stored alongside for future tooltip/click-to-open use.

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/api.rs` | Add `RecentFile` struct |
| `crates/common/src/config.rs` | Add `TrayConfig` + `tray` field to `ClientConfig` |
| `crates/server/src/db/mod.rs` | Add `recent_files(conn, limit)` |
| `crates/server/src/routes/recent.rs` | New — `get_recent` handler |
| `crates/server/src/routes/mod.rs` | Re-export `get_recent` |
| `crates/server/src/main.rs` | Register `GET /api/v1/recent` |
| `crates/windows/tray/src/poller.rs` | Demand-driven polling, configurable interval, recent query |
| `crates/windows/tray/src/main.rs` | Left-click, AppEvent variant, popup wiring |
| `crates/windows/tray/src/popup.rs` | New — Win32 popup window |
| `crates/windows/tray/Cargo.toml` | Add `Win32_System_LibraryLoader`, `Win32_Foundation` features |
| `packaging/windows/find-anything.iss` | Add commented `[tray]` block to `BuildToml()` |
| `install.sh` | Add commented `[tray]` block to default `client.toml` heredoc |

## Testing

1. Build Windows binary (`mise build-wsl-cross` or native).
2. Left-click tray: popup appears instantly at bottom-right.  After ~100ms +
   network latency the file list populates with the 20 most recently indexed
   paths.  Subsequent updates arrive every 1 s.
3. Click anywhere outside the popup: it dismisses.
4. Escape key dismisses the popup.
5. Left-click again: popup re-opens (toggles closed if already open).
6. Right-click: existing menu appears; file count is fresh (single poll fired).
7. With popup open, trigger a scan; within ~1 s the list updates.
8. No server reachable: popup shows "Connecting…" placeholder row.
9. With server unreachable, no excessive CPU (thread sleeps 100 ms between
   checks when popup is closed).
10. Set `poll_interval_ms = 3000` in `client.toml`; verify popup updates every
    3 s instead of 1 s.
