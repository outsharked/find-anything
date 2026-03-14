# Plan 021: Windows Client Support

## Context

find-anything currently targets Linux and macOS only. The server already runs anywhere
(including Docker), and the client binaries (`find-scan`, `find-watch`) are conceptually
cross-platform since `notify` v6 supports `ReadDirectoryChangesW` and `walkdir` supports
Windows. This plan adds:

1. **Windows build pipeline** — x86_64-pc-windows-msvc via `windows-latest` CI runner
2. **`find-watch` as a Windows Service** — self-installing via `windows-service` crate;
   no external tools required
3. **`find-tray`** — Windows-only system tray app using `tray-icon` crate;
   auto-starts at login, manages the service, shows index status, triggers scans
4. **PowerShell install helper** + documentation

---

## Architecture

```
Windows machine:
  find-scan.exe          on-demand scan (existing binary, compiles as-is)
  find-watch.exe         file watcher (add service subcommands)
    ├── find-watch                  normal mode (existing)
    ├── find-watch install          register service + tray at login (new, admin)
    ├── find-watch uninstall        remove service + tray (new, admin)
    └── find-watch service-run      called by SCM only (new, hidden command)
  find-tray.exe          system tray UI (new Windows-only binary)
    ├── Shows tray icon + right-click menu
    ├── Polls Windows SCM for service status
    ├── Polls server HTTP API for file counts
    ├── Triggers find-scan.exe for on-demand scans
    └── Starts/stops the find-watch service
  find-extract-*.exe     extractors (existing binaries, compile as-is)
```

---

## Component Details

### 1. Windows Build Pipeline

Add a Windows job to `.github/workflows/release.yml`:

```yaml
- os: windows-latest
  target: x86_64-pc-windows-msvc
  artifact: windows-x86_64
  ext: .exe
```

**Notes:**
- Native MSVC build (not MinGW cross-compile) — required for `windows-service` crate
- Package as `find-anything-{version}-windows-x86_64.zip` (not tarball)
- Include all existing 8 binaries + `find-tray.exe` = 9 binaries total
- `reqwest` already uses `rustls-tls` — no OpenSSL dependency on Windows
- `aarch64-pc-windows-msvc` deferred (x86_64 emulation works on ARM64 Windows)

Also add a Windows build check to `.github/workflows/ci.yml`:
```yaml
- os: windows-latest, run: cargo build --workspace
```
Catches Windows compilation failures early without the full release matrix.

---

### 2. `find-watch` Windows Service Support

**New library crate:** `crates/windows/service/` — contains all Windows service logic

**`crates/windows/service/Cargo.toml`:**
```toml
[package]
name = "find-windows-service"
version = "0.2.1"
edition = "2021"

[lib]
name = "find_windows_service"
path = "src/lib.rs"

[dependencies]
find-common = { path = "../../common" }
windows-service = "0.8"
anyhow = { workspace = true }
tokio = { workspace = true }
```

**`crates/windows/service/src/lib.rs`** exports:
- `pub fn service_main(args: Vec<OsString>)` — called by the SCM macro; sets up tokio
  runtime, registers stop handler (→ cancellation token), reports `ServiceState::Running`,
  calls `watch::run_watch()`, on Stop: `ServiceState::Stopped`
- `pub fn install_service(config_path: &Path, service_name: &str) -> anyhow::Result<()>`
  — creates service via `ServiceManager` (`OWN_PROCESS`, `AutoStart`),
  binary path: `current_exe service-run --config {abs_path}`,
  description: "Find Anything file watcher — keeps the index current",
  also writes `find-tray.exe --config {abs_path}` to
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` key `FindAnythingTray`
- `pub fn uninstall_service(service_name: &str) -> anyhow::Result<()>` — stops + deletes
  service, removes Run registry key

**`crates/client/Cargo.toml`** — add Windows-only dep:
```toml
[target.'cfg(windows)'.dependencies]
find-windows-service = { path = "../windows/service" }
```

**Modified:** `crates/client/src/watch_main.rs`

The `define_windows_service!` macro must live in the binary crate (it emits a public
FFI symbol), so one line stays here:
```rust
#[cfg(windows)]
windows_service::define_windows_service!(ffi_service_main, find_windows_service::service_main);
```

Add clap subcommands (Windows-only via `#[cfg(windows)]`):

```
find-watch [--config path]          # existing behavior — unchanged
find-watch install [--config path] [--service-name name]
find-watch uninstall [--service-name name]
find-watch service-run              # hidden; called by SCM only
```

Subcommand dispatch calls into `find_windows_service::install_service()`,
`find_windows_service::uninstall_service()`, and
`service_dispatcher::start(SERVICE_NAME, ffi_service_main)` respectively.

**Service name:** configurable, default `"FindAnythingWatcher"`.

---

### 3. `find-tray` Binary (new Windows-only crate)

**Location:** `crates/windows/tray/`

**Cargo.toml deps:**
```toml
[dependencies]
find-common = { path = "../../common" }
tray-icon = "0.21"
windows-service = "0.8"
reqwest = { version = "0.12", features = ["json", "rustls-tls", "blocking"] }
toml = "0.8"
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
```

**Entry point:** `src/main.rs`

Startup:
1. Parse `--config path` (written to Run registry by `find-watch install`)
2. Read `ClientConfig` to get server URL + token
3. Find `find-scan.exe` path (same directory as `find-tray.exe`)
4. Create `winit::EventLoop`
5. Build tray icon + muda menu
6. Spawn background poller thread (std thread, sends via `EventLoopProxy`)
7. Run event loop on main thread; handle `TrayIconEvent` + `MenuEvent`

**Tray menu structure:**

```
┌──────────────────────────────────────────┐
│ Watcher: Running                         │  (disabled, updated by poller)
│ 42,153 files across 2 sources            │  (disabled, from server API)
├──────────────────────────────────────────┤
│ Run Full Scan                            │  → spawn find-scan.exe --config ...
│ Stop Watcher    (or "Start Watcher")     │  → ServiceManager stop/start
├──────────────────────────────────────────┤
│ Open Config File                         │  → ShellExecuteW(config path)
│ Quit Tray                                │  → exit find-tray (service keeps running)
└──────────────────────────────────────────┘
```

**Background poller** (std thread, every 5 seconds):
- Windows SCM: `OpenService → QueryServiceStatus → dwCurrentState`
- Server API: `GET /api/v1/sources` → sum file counts
- Sends `AppEvent::StatusUpdate { service_state, file_count }` via `EventLoopProxy`

**Module layout:**
- `src/main.rs` — event loop, tray creation, event dispatch
- `src/menu.rs` — menu item IDs, menu construction, label updates
- `src/poller.rs` — background status polling thread
- `src/service_ctl.rs` — SCM start/stop wrapper via `windows-service`

**Tray icon:** `assets/icon.ico` (16×16 + 32×32), embedded via `include_bytes!`.
Two variants: active (filled) and stopped (grey). Swap icon based on service state.

---

### 4. PowerShell Scripts

**`install-windows.ps1`:**
- Downloads latest release ZIP from GitHub API
- Extracts to `$env:LOCALAPPDATA\find-anything`
- Opens `client.toml` template in Notepad for user to edit
- Runs `find-watch.exe install --config client.toml`

**`uninstall-windows.ps1`:**
- Runs `find-watch.exe uninstall`
- Removes install directory

---

### 5. Documentation

**`docs/windows/README.md`:**
- System requirements (Windows 10 1903+ x86_64, or Server 2019+)
- Quick start: download zip → extract → edit config → `find-watch install`
- Service management: `find-watch install/uninstall`, `sc start/stop FindAnythingWatcher`
- find-tray usage and auto-start behavior
- Troubleshooting: Windows Event Log, checking service status with `Get-Service`

---

## Files Changed / Created

| File | Change |
|------|--------|
| `.github/workflows/release.yml` | Add windows-x86_64 build job |
| `.github/workflows/ci.yml` | Add Windows `cargo build --workspace` check |
| `Cargo.toml` | Add `crates/windows/service` + `crates/windows/tray` workspace members |
| `crates/client/Cargo.toml` | Add `find-windows-service` Windows-only target dep |
| `crates/client/src/watch_main.rs` | Add `define_windows_service!` + install/uninstall/service-run subcommands |
| `crates/windows/service/Cargo.toml` | **New** — Windows service library crate |
| `crates/windows/service/src/lib.rs` | **New** — install_service, uninstall_service, service_main |
| `crates/windows/tray/Cargo.toml` | **New** |
| `crates/windows/tray/src/main.rs` | **New** — event loop + tray setup |
| `crates/windows/tray/src/menu.rs` | **New** — menu construction + label updates |
| `crates/windows/tray/src/poller.rs` | **New** — background status polling |
| `crates/windows/tray/src/service_ctl.rs` | **New** — SCM start/stop |
| `crates/windows/tray/assets/icon.ico` | **New** — tray icon (16×16 + 32×32) |
| `install-windows.ps1` | **New** |
| `uninstall-windows.ps1` | **New** |
| `docs/windows/README.md` | **New** |
| `docs/plans/021-windows-client.md` | **New** |

---

## Implementation Phasing

**Phase A — Pipeline + service (implement first):**
1. `.github/workflows/release.yml` + `ci.yml` — Windows x86_64 build job
2. `crates/windows/service/` — new library crate with all service logic
3. `crates/client/Cargo.toml` + `crates/client/src/watch_main.rs` — add dep + subcommands
4. PowerShell scripts
5. `docs/windows/README.md`
6. `docs/plans/021-windows-client.md`
7. Commit: "Add Windows build pipeline and find-watch service support (plan 021 Phase A)"

**Phase B — Tray app:**
1. `crates/windows/tray/` — full tray crate
2. Update `find-watch install` to register find-tray in Run registry
3. Add `find-tray.exe` to Windows release artifact
4. Commit: "Add find-tray system tray app for Windows (plan 021 Phase B)"

---

## Key Crate Versions

| Crate | Version | Notes |
|-------|---------|-------|
| `windows-service` | `0.8` | Maintained by Mullvad; stable API |
| `tray-icon` | `0.21` | Tauri team; muda bundled for menus |

---

## Verification

**Phase A (on Windows machine or VM):**
```powershell
cargo build --target x86_64-pc-windows-msvc -p find-client
.\find-watch.exe install --config client.toml    # must run as admin
Get-Service FindAnythingWatcher                  # should show Running/Stopped
.\find-watch.exe uninstall
```

**Phase B:**
```powershell
cargo build --target x86_64-pc-windows-msvc -p find-tray-win
.\find-tray.exe --config client.toml    # tray icon appears in system tray
# Right-click → Run Full Scan, Start/Stop Watcher, etc.
```

**CI:**
- Push to branch → ci.yml Windows check compiles successfully
- Push `v*` tag → release.yml produces `find-anything-*-windows-x86_64.zip`
