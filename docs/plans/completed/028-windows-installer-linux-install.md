# Windows Inno Setup Installer + Linux Install Improvements

## Overview

Improves the install experience on both Windows and Linux:

- **Windows `install-windows.ps1`**: Reverted to client-only (no server setup).
  Now prompts for server URL, token, and directory to watch; runs `find-scan --full`
  before starting the service.
- **Inno Setup installer** (`packaging/windows/find-anything.iss`): GUI wizard that
  collects server URL, token, and directories; writes `client.toml`; registers the
  service; and offers a post-install full scan.
- **WinGet manifest**: Updated to list the `.exe` Inno installer as the primary
  installer type alongside the existing portable ZIP.
- **CI** (`release.yml`): Windows build job now also builds the Inno Setup installer
  and uploads it as a release asset.
- **`install.sh`**: Prompts for server URL, token, and directories; writes
  `~/.config/find-anything/client.toml`; runs `find-scan --full`; installs and
  enables the `find-watch` systemd user service (with launchd fallback on macOS).

## Design Decisions

- **Client-only on Windows**: The server always runs on Linux/Docker. The Windows
  installer only sets up the client stack (find-watch service, find-tray autostart,
  find-scan, find CLI).
- **find-server.exe included but not configured**: It's in the package because it
  compiles for Windows; power users who want a local server can configure it manually.
- **Inno Setup over NSIS**: Inno Setup has native Pascal scripting, clean built-in
  UI, and good WinGet support.
- **scan-and-start.bat**: Runs the initial scan in a visible console window so users
  can see progress; uses `pause` to keep the window open.
- **systemd user service on Linux**: Uses `~/.config/systemd/user/` so no root is
  required. Falls back to launchd on macOS, or prints manual instructions otherwise.
- **`SKIP_CONFIG=1` env var**: Allows unattended install in CI/scripted environments.

## Files Changed

- `install-windows.ps1` — client-only; prompts; runs scan before service start
- `packaging/windows/find-anything.iss` — new Inno Setup script
- `packaging/windows/scan-and-start.bat` — new post-install scan + service start helper
- `packaging/winget/JamieT.find-anything.installer.yaml` — add inno entry alongside zip
- `.github/workflows/release.yml` — add Inno Setup build + upload steps
- `install.sh` — add config prompts, find-scan run, systemd/launchd service setup

## Testing

1. **Windows (VM)**: Run installer, fill wizard pages with a real server URL/token.
   Verify: `client.toml` written, service registered, `scan-and-start.bat` opens a
   console and indexes files, service starts, find-tray in tray.
2. **Windows uninstall**: Verify service removed and autostart key cleaned.
3. **Linux**: Run `install.sh` pointing at a running find-server; verify prompts,
   config file, scan output, and `systemctl --user status find-watch`.
4. **macOS**: Run `install.sh`; verify launchd plist written and loaded.
5. **WinGet**: `winget validate packaging/winget/` passes.
6. **CI**: Push `v0.2.4` tag; verify both ZIP and installer EXE appear in the Release.

## Breaking Changes

None. The PowerShell script API is unchanged for users who were invoking it manually.
