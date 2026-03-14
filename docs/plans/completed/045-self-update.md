# Self-Update

## Overview

Allow `find-server` to update itself to the latest GitHub release from within
the web UI. A button in the About panel checks for a newer version and, if one
is available, downloads the matching binary, replaces the current executable
atomically, and exits cleanly so systemd restarts the service on the new binary.

The feature is only offered when the server detects it is running under systemd
(via the `INVOCATION_ID` environment variable). On unsupported configurations
the UI shows an informational message rather than the button.

---

## Design Decisions

### Systemd detection

Check `INVOCATION_ID` at startup — systemd sets this on every service
invocation. Store the boolean in `AppState`. The update routes gate on it
server-side (return `400` if not under systemd) and the frontend only shows the
button when `check` reports `restart_supported: true`.

This means the feature is a no-op on direct invocation (dev, macOS, non-service
Linux) without any special casing in the install docs.

### Restart mechanism

After successfully replacing the binary, spawn a background task that sleeps
300 ms (enough for the HTTP response to flush) then calls
`std::process::exit(0)`. Systemd sees a clean exit and restarts the service,
picking up the new binary from disk.

`exec()` is deliberately avoided — it has subtle interactions with open file
descriptors and signal masks that are unnecessary given that systemd handles
the restart.

### Binary replacement

1. Download the new binary to a temp file in the same directory as the current
   executable (same filesystem → `rename()` is atomic).
2. `chmod +x` the temp file.
3. `rename(tmp, current_exe)` — atomic on Linux. The running process keeps its
   original inode open; the new binary occupies the path immediately.
4. Exit.

`std::env::current_exe()` returns the binary path. On systems where this is a
symlink, follow it to the real path before replacing.

### Platform / asset selection

Release assets are named with a predictable suffix:

| Platform | Asset suffix |
|----------|-------------|
| x86_64 Linux | `x86_64-linux` |
| ARMv7 Linux | `armv7-linux` |

At startup, map `std::env::consts::ARCH` + `std::env::consts::OS` to the
expected suffix. If no matching asset is found in the release, the update
response marks the update as unavailable with a human-readable reason.

### Version check caching

The GitHub API is called at most once per hour. The result (latest version,
asset URLs, timestamp) is stored in `AppState` behind a `tokio::sync::RwLock`.
Subsequent calls within the window return the cached value. This stays well
within GitHub's 60 req/hour unauthenticated limit.

### No checksum verification (initial implementation)

The current release workflow does not publish a `SHA256SUMS` file. The download
is over HTTPS to `github.com` which provides transport security. Adding checksum
verification is a follow-up task if desired.

---

## Implementation

### New API types (`crates/common/src/api.rs`)

```rust
pub struct UpdateCheckResponse {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
    pub restart_supported: bool,
    /// None when restart_supported is true; human-readable reason otherwise.
    pub restart_unsupported_reason: Option<String>,
}

pub struct UpdateApplyResponse {
    pub ok: bool,
    pub message: String,
}
```

### New routes

```
GET  /api/v1/admin/update/check   → UpdateCheckResponse
POST /api/v1/admin/update/apply   → UpdateApplyResponse (202, then exits)
```

Both require bearer auth (same `check_auth` guard as other admin routes).

### AppState additions

```rust
pub under_systemd: bool,
pub update_cache: tokio::sync::RwLock<Option<CachedUpdateCheck>>,
```

```rust
struct CachedUpdateCheck {
    checked_at: std::time::Instant,
    latest_version: String,
    asset_url: Option<String>,   // None if no matching asset for this platform
}
```

### `check` handler (`routes/admin.rs`)

1. If cache is fresh (< 1 hour), use it. Otherwise call GitHub API.
2. Parse `tag_name` and `assets[].browser_download_url` from the response.
3. Find the asset whose name contains the platform suffix for this binary.
4. Return `UpdateCheckResponse`.

Use `reqwest` (already a transitive dependency via `find-common` or server
crates; add explicitly if needed).

### `apply` handler (`routes/admin.rs`)

1. Guard: return `400` if `!state.under_systemd`.
2. Call `check` logic to get the asset URL (or use cache).
3. Guard: return `400` if no update available or no matching asset.
4. Download asset to `{current_exe_dir}/find-server.new`.
5. `chmod +x`.
6. `rename` over `current_exe`.
7. Spawn background task: `sleep(300ms)` → `std::process::exit(0)`.
8. Return `202 { ok: true, message: "Update applied. Restarting…" }`.

### `AppState` construction (`main.rs`)

```rust
let under_systemd = std::env::var("INVOCATION_ID").is_ok();
```

### Frontend (`web/src/lib/About.svelte`)

On mount:
1. Call `GET /api/v1/admin/update/check`.
2. If `!restart_supported`: show small muted note "Self-update requires systemd".
3. If `update_available`: show "Update to vX.Y.Z" button.
4. If up to date: show "Up to date" with the latest version.

On button click:
1. Disable button, show spinner.
2. `POST /api/v1/admin/update/apply`.
3. Show "Restarting…" message.
4. Poll `GET /api/v1/settings` every 2 s until it responds.
5. Reload the page.

New API functions in `api.ts`:
```ts
getUpdateCheck(): Promise<UpdateCheckResponse>
applyUpdate(): Promise<UpdateApplyResponse>
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/api.rs` | `UpdateCheckResponse`, `UpdateApplyResponse` types |
| `crates/server/src/main.rs` | `under_systemd` + `update_cache` in `AppState`; detect systemd at startup |
| `crates/server/src/routes/admin.rs` | `update_check` and `update_apply` handlers |
| `crates/server/src/routes/mod.rs` | Re-export new handlers |
| `crates/server/src/main.rs` | Register new routes |
| `web/src/lib/api.ts` | `UpdateCheckResponse` type + `getUpdateCheck`, `applyUpdate` functions |
| `web/src/lib/About.svelte` | Update check UI, button, restart polling |
| `Cargo.toml` / `Cargo.lock` | Add `reqwest` if not already present as a direct dep |

---

## Testing

- **Without systemd:** Run binary directly → `check` returns `restart_supported: false`, button absent from UI.
- **With systemd:** Set `INVOCATION_ID=test` in environment → `check` returns `restart_supported: true`. (Full end-to-end requires a real service unit.)
- **No matching asset:** Mock a release with no `x86_64-linux` asset → `check` returns `update_available: false` with a reason.
- **Already up to date:** Current version = latest → `update_available: false`.
- **Download failure:** Simulate a bad URL → `apply` returns `500`, binary unchanged.

---

## Breaking Changes

None. New routes are additive. `AppState` gains fields but construction is
internal. The UI change is additive (new section in About panel).

---

## Follow-up

- Publish `SHA256SUMS` in the release workflow and verify the download before replacing the binary.
- Show a changelog excerpt (pull from the GitHub release body) in the UI before confirming the update.
- Support `Restart=on-failure` vs `Restart=always` detection (minor — both work for a clean exit).
