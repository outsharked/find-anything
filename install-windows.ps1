# install-windows.ps1
# Downloads the latest find-anything Windows release and installs the client stack:
#   - find-watch    as a Windows service
#   - find-tray     in the autostart registry key
#   - All binaries  added to the user PATH
#
# The server (find-server) always runs on Linux/Docker. This script is client-only.
#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"
$InstallDir  = "$env:LOCALAPPDATA\find-anything"
$DataDir     = "$InstallDir\data"
$ClientConfig = "$InstallDir\client.toml"
$ServiceName = "FindAnythingWatcher"

Write-Host "find-anything Windows installer (client)" -ForegroundColor Cyan
Write-Host ""

# ── Download release ──────────────────────────────────────────────────────────

$Release = Invoke-RestMethod "https://api.github.com/repos/jamietre/find-anything/releases/latest"
$Asset   = $Release.assets | Where-Object { $_.name -like "*windows-x86_64*.zip" } | Select-Object -First 1
if (-not $Asset) { throw "Could not find Windows release asset in latest release" }

$ZipPath = "$env:TEMP\find-anything-windows.zip"
Write-Host "Downloading $($Asset.name)..." -ForegroundColor Yellow
Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $ZipPath

Write-Host "Extracting to $InstallDir..."
$ExtractTemp = "$env:TEMP\find-anything-extract"
if (Test-Path $ExtractTemp) { Remove-Item $ExtractTemp -Recurse -Force }
Expand-Archive -Path $ZipPath -DestinationPath $ExtractTemp

# Stop existing service before overwriting binaries
Write-Host "Stopping existing service (if any)..."
Stop-Service -Name $ServiceName -ErrorAction SilentlyContinue

if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force }
New-Item -ItemType Directory -Path $InstallDir | Out-Null
New-Item -ItemType Directory -Path $DataDir   | Out-Null

$ExtractedDir = Get-ChildItem $ExtractTemp | Select-Object -First 1
Move-Item "$($ExtractedDir.FullName)\*" $InstallDir
Remove-Item $ExtractTemp -Recurse -Force
Remove-Item $ZipPath -Force

# ── Prompt for configuration ───────────────────────────────────────────────────

Write-Host ""
Write-Host "Server configuration" -ForegroundColor Cyan
Write-Host "  The find-anything server runs on Linux/Docker."
Write-Host "  Enter the URL and token from your server's server.toml."
Write-Host ""

$ServerUrl = Read-Host "Server URL [http://localhost:8765]"
if (-not $ServerUrl) { $ServerUrl = "http://localhost:8765" }

$TokenSecure = Read-Host "Bearer token" -AsSecureString
$Token = [System.Runtime.InteropServices.Marshal]::PtrToStringAuto(
    [System.Runtime.InteropServices.Marshal]::SecureStringToBSTR($TokenSecure)
)
if (-not $Token) {
    Write-Host "Token cannot be empty." -ForegroundColor Red
    exit 1
}

$DefaultDir  = $env:USERPROFILE
$WatchDir    = Read-Host "Directory to watch [$DefaultDir]"
if (-not $WatchDir) { $WatchDir = $DefaultDir }

# ── Write client config ────────────────────────────────────────────────────────

Write-Host ""
Write-Host "Writing client.toml..."

$WatchDirToml = $WatchDir -replace '\\', '\\'

@"
[server]
url   = "$ServerUrl"
token = "$Token"

[[sources]]
name  = "home"
paths = ["$WatchDirToml"]
"@ | Set-Content $ClientConfig -Encoding UTF8

# ── Register find-watch as a Windows service ──────────────────────────────────

Write-Host "Installing find-watch Windows service..."
& "$InstallDir\find-watch.exe" install --config $ClientConfig

# ── Add find-tray to autostart ────────────────────────────────────────────────

Write-Host "Adding find-tray to startup..."
$RunKey = "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
Set-ItemProperty -Path $RunKey -Name "FindAnythingTray" -Value "`"$InstallDir\find-tray.exe`""

# ── Add install directory to user PATH ────────────────────────────────────────

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
    Write-Host "Added $InstallDir to PATH."
}

# ── Start the service ─────────────────────────────────────────────────────────

Write-Host ""
Write-Host "Starting find-watch service..."
Start-Service -Name $ServiceName -ErrorAction SilentlyContinue

if (Test-Path "$InstallDir\find-tray.exe") {
    Write-Host "Starting find-tray..."
    Start-Process "$InstallDir\find-tray.exe"
}

# ── Summary ───────────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "Installation complete!" -ForegroundColor Green
Write-Host ""
Write-Host "  Server:       $ServerUrl"
Write-Host "  Watching:     $WatchDir"
Write-Host ""
Write-Host "  Binaries:     $InstallDir"
Write-Host "  Client cfg:   $ClientConfig"
Write-Host "  Data:         $DataDir"
Write-Host ""
Write-Host "Re-run a full scan:  find-scan.exe --config $ClientConfig --full"
Write-Host "Stop the watcher:    sc stop $ServiceName"
