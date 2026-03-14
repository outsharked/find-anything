# Windows Service and Tray App Fixes

## Overview

Several critical bugs prevent the Windows service from being installed/started and
the tray app from working correctly. Additionally, all errors are silently swallowed
because the tray is a `windows_subsystem = "windows"` app with no console.

## Bugs Fixed

### Bug 1 — ISS: wrong argument order for service install (CRITICAL)

`find-anything.iss` line 75 runs:
```
find-watch.exe install --config path
```

In clap v4, parent-level args must appear before the subcommand (unless marked
`global = true`). clap rejects `install --config path` and the process exits with a
parse error. **Result: service is never registered.**

Fix: Change to `find-watch.exe --config path install`.

### Bug 2 — `--config` not global in clap (CRITICAL)

The Windows SCM binary path is `find-watch.exe service-run --config path`. When SCM
starts this process, clap sees `service-run --config path`. Since `--config` isn't
`global = true`, it can't appear after a subcommand → clap error → process exits.
**Result: service can never start.**

Fix: Add `global = true` to `--config` in `Args`.

### Bug 3 — `service_entry` can't read config path (CRITICAL)

`service_entry(args)` calls `parse_config_from_args` on the args passed by the SCM
to `ServiceMain`. These are NOT the binary-path args — they're only what's passed to
`StartService()`. When `sc start FindAnythingWatcher` is called with no extra args,
`service_entry` gets `["FindAnythingWatcher"]` and returns `None` for config.
**Result: service starts but immediately exits (can't load config).**

Fix: Store the parsed config path in a `OnceLock<PathBuf>` before calling
`service_dispatcher::start()`. `service_entry` reads from the OnceLock first, and
falls back to `parse_config_from_args` for any extra args passed via `StartService`.

### Bug 4 — Tray launched before config is written (timing)

ISS launches `find-tray.exe` in a non-`postinstall` `[Run]` entry. In Inno Setup,
non-postinstall `[Run]` entries run before `ssPostInstall`, which is where
`CurStepChanged` writes `client.toml`. So the tray launches before the config
exists, reads nothing, and silently exits.

Fix: Remove the intermediate tray `[Run]` entry. The HKCU Run registry entry handles
next-login autostart; scan-and-start.bat (postinstall) handles the immediate launch.

### Bug 5 — No user-visible errors from tray app (UX)

With `#![windows_subsystem = "windows"]`, all `tracing::warn!` output is invisible.
Config load failure causes a silent exit. Service start/stop failures are ignored.
Scan launch failures are ignored.

Fix: Add a `show_error(title, msg)` helper using `MessageBoxW`. Call it on:
- Fatal startup errors (config not found / parse error)
- Service start/stop failures
- Scan process launch failures

### Bug 6 — ISS `[Registry]` tray autostart entry missing `--config`

The ISS writes `FindAnythingTray = "find-tray.exe"` (no `--config`). This is later
overwritten correctly by `install_service` (which includes `--config`), but if
install fails the wrong entry persists.

Fix: Update the ISS `[Registry]` entry to pass `--config`.

## Files Changed

- `crates/client/src/watch_main.rs` — global --config flag, OnceLock config path
- `crates/windows/tray/src/main.rs` — MessageBoxW error surfacing
- `packaging/windows/find-anything.iss` — arg order fix, remove premature tray launch,
  fix registry entry

## Testing

1. Build Windows binaries cross-compiled from Linux.
2. In a Windows VM, run the installer. Verify:
   - Service is registered: `sc query FindAnythingWatcher`
   - Config is written: `%USERPROFILE%\.config\FindAnything\client.toml`
   - scan-and-start.bat starts service and tray successfully
   - Tray shows "Watcher: Running"
3. Use tray to stop/start the service; verify it works without admin.
4. Kill the tray; at next login it restarts automatically with the right --config.
5. Point config at a bad server URL; verify tray shows "Connecting…" but does not
   crash. Stop the service; verify error dialog appears when trying to start it while
   the service is not installed.
6. Delete client.toml and launch find-tray.exe directly; verify error MessageBox
   is shown instead of a silent crash.
