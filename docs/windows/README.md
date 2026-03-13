# find-anything for Windows

## System Requirements

- Windows 10 version 1903 (May 2019 Update) or later
- Windows Server 2019 or later
- x86_64 architecture (64-bit Intel/AMD)
- Administrator privileges for service installation

## Quick Start

### 1. Download and Extract

Download the latest Windows release from [GitHub Releases](https://github.com/findanything/find-anything/releases):

- Look for `find-anything-vX.X.X-windows-x86_64.zip`
- Extract to a permanent location (e.g., `C:\Program Files\find-anything` or `%LOCALAPPDATA%\find-anything`)

### 2. Configure

Edit `client.toml` to point to your find-anything server:

```toml
[server]
url   = "http://localhost:8080"
token = "your-secret-token"

[[sources]]
name  = "home"
path = "C:\\Users\\YourName"

[[sources]]
name  = "projects"
path = "C:"
include = "code/**", "project/**"

[scan]
max_content_size_mb = 10
exclude = ["**/.git/**", "**/node_modules/**", "**/target/**"]

[watch]
debounce_ms = 500
```

### 3. Install the Service

Run **as Administrator**:

```powershell
.\find-watch.exe install --config client.toml
```

This will:

- Register the `FindAnythingWatcher` Windows service
- Configure it to start automatically on boot
- Register `find-tray.exe` to start at login (system tray app)

### 4. Start the Service

```powershell
sc start FindAnythingWatcher
```

Or use the Windows Services MMC snap-in (`services.msc`).

### 5. Run Initial Scan

The watcher only processes file changes. Run a full scan first:

```powershell
.\find-scan.exe --config client.toml
```

This will index all configured sources. Progress is logged to the console.

---

## Service Management

### Start/Stop/Restart

```powershell
# Start
sc start FindAnythingWatcher

# Stop
sc stop FindAnythingWatcher

# Restart
sc stop FindAnythingWatcher
sc start FindAnythingWatcher

# Check status
sc query FindAnythingWatcher
```

Or use PowerShell cmdlets:

```powershell
Start-Service FindAnythingWatcher
Stop-Service FindAnythingWatcher
Restart-Service FindAnythingWatcher
Get-Service FindAnythingWatcher
```

### View Service Logs

The service logs to the Windows Event Log. View via Event Viewer or PowerShell:

```powershell
Get-EventLog -LogName Application -Source FindAnythingWatcher -Newest 50
```

Or use the newer `Get-WinEvent`:

```powershell
Get-WinEvent -FilterHashtable @{LogName='Application'; ProviderName='FindAnythingWatcher'} -MaxEvents 50
```

### Uninstall the Service

Run **as Administrator**:

```powershell
.\find-watch.exe uninstall
```

This will:

- Stop the service
- Remove it from the Windows Service Control Manager
- Remove the tray app from auto-start

---

## System Tray App (`find-tray.exe`)

The tray app provides a quick interface for managing the watcher:

- **Watcher status** — shows if the service is running/stopped
- **File count** — displays total indexed files across all sources
- **Run Full Scan** — triggers `find-scan.exe` on demand
- **Start/Stop Watcher** — controls the Windows service
- **Open Config File** — opens `client.toml` in the default editor
- **Quit Tray** — exits the tray app (service keeps running)

The tray app auto-starts at login (registered in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`). To disable auto-start:

```powershell
Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "FindAnythingTray"
```

---

## Binaries Included

| Binary                     | Purpose                                |
| -------------------------- | -------------------------------------- |
| `find-scan.exe`            | One-time full scan                     |
| `find-watch.exe`           | File watcher service                   |
| `find.exe`                 | Command-line search client             |
| `find-server.exe`          | Server (typically run on Linux/macOS)  |
| `find-tray.exe`            | System tray app (Windows-only)         |
| `find-extract-text.exe`    | Text content extractor                 |
| `find-extract-pdf.exe`     | PDF text extractor                     |
| `find-extract-media.exe`   | Image EXIF, audio tags, video metadata |
| `find-extract-archive.exe` | ZIP/TAR/7Z archive extractor           |
| `find-extract-html.exe`    | HTML content extractor                 |
| `find-extract-office.exe`  | Office document extractor              |
| `find-extract-epub.exe`    | EPUB ebook extractor                   |

---

## Automated Installation

For scripted deployments, use the included PowerShell installer:

```powershell
# Download and run (requires admin)
powershell -ExecutionPolicy Bypass -File install-windows.ps1
```

This will:

1. Download the latest release from GitHub
2. Extract to `%LOCALAPPDATA%\find-anything`
3. Create a config template
4. Open the config in Notepad for editing
5. Install the service

---

## Troubleshooting

### Service won't start

**Check the Event Log:**

```powershell
Get-EventLog -LogName Application -Source FindAnythingWatcher -Newest 10 | Format-List
```

**Common issues:**

- **Config file not found:** Ensure the path passed to `install --config` is absolute, or the config is in the same directory as `find-watch.exe`
- **Server unreachable:** Verify `server.url` in `client.toml` points to a running server
- **Invalid token:** Check `server.token` matches the server's configured token
- **Permissions:** The service runs as `LocalSystem` by default; ensure it has access to the paths configured in `sources[].paths`

### Watcher not detecting file changes

**Verify the service is running:**

```powershell
Get-Service FindAnythingWatcher
```

**Check the debounce setting:** The watcher waits `watch.debounce_ms` (default 500ms) after a file change before processing. Rapid edits are batched.

**Exclusion patterns:** Check `scan.exclude` in `client.toml` — the watcher respects the same exclusions as `find-scan`.

**File system limits:** On Windows, `ReadDirectoryChangesW` may drop events if the change queue overflows (e.g., mass file copies). This is a Windows API limitation.

### Extractor binaries not found

The watcher spawns extractor binaries (`find-extract-*.exe`) as subprocesses. Ensure they're:

1. In the same directory as `find-watch.exe`, OR
2. On the system `PATH`

You can override the extractor directory in `client.toml`:

```toml
[watch]
extractor_dir = "C:\\path\\to\\extractors"
```

### High CPU usage

The watcher is event-driven and should be idle when files aren't changing. If CPU is high:

- Check for a runaway file change loop (e.g., indexing a directory that the server writes to)
- Verify exclusion patterns cover large build directories (`**/target/**`, `**/node_modules/**`)

---

## Differences from Linux/macOS

- **Service model:** Uses Windows SCM instead of systemd
- **Tray app:** Windows-only; Linux/macOS don't have an equivalent (use `find-watch` directly or systemd user units)
- **Path separators:** Use backslashes `\` in config paths (e.g., `path = "C:\\Users\\Name"`)
- **Event log:** Windows uses Event Viewer instead of journalctl
- **Permissions:** Run installers and service commands as Administrator; the service itself runs as `LocalSystem` (can be changed via `sc config`)

---

## Uninstallation

### Option 1: Automated

```powershell
powershell -ExecutionPolicy Bypass -File uninstall-windows.ps1
```

### Option 2: Manual

1. Uninstall the service:
   ```powershell
   .\find-watch.exe uninstall
   ```
2. Remove the install directory:
   ```powershell
   Remove-Item "$env:LOCALAPPDATA\find-anything" -Recurse -Force
   ```

---

## Architecture Notes

- **File watcher:** Uses `notify` crate v6 with `ReadDirectoryChangesW` backend (native Windows API)
- **Service integration:** Uses `windows-service` crate v0.8 (FFI wrapper around Windows Service Control Manager)
- **Tray app:** Uses `tray-icon` crate v0.21 (`muda` for menus, `winit` for event loop)
- **Extraction:** Spawns `find-extract-*.exe` subprocesses (same binaries as Linux/macOS)

---

For more details, see the [main documentation](../../README.md) and [plan 021](../plans/021-windows-client.md).
