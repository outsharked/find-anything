# find-anything protocol handler installer
# Installs find-handler.exe and registers the findanything:// URL scheme.
# No administrator rights required — keys are written under HKCU.
#
# Usage (run from PowerShell):
#   irm https://github.com/jamietre/find-anything/releases/latest/download/install-handler.ps1 | iex
#
# To uninstall:
#   irm https://github.com/jamietre/find-anything/releases/latest/download/install-handler.ps1 | iex -Command { & { . ([ScriptBlock]::Create($input | Out-String)); Uninstall-Handler } }
# Or manually: Remove-Item -Recurse "$env:LOCALAPPDATA\FindAnythingHandler"; Remove-Item -Recurse "HKCU:\Software\Classes\findanything"

$ErrorActionPreference = 'Stop'

$InstallDir  = Join-Path $env:LOCALAPPDATA 'FindAnythingHandler'
$ExePath     = Join-Path $InstallDir 'find-handler.exe'
$DownloadUrl = 'https://github.com/jamietre/find-anything/releases/latest/download/find-handler.exe'
$RegBase     = 'HKCU:\Software\Classes\findanything'

function Install-Handler {
    Write-Host 'Installing find-anything protocol handler...'

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

    Write-Host "  Downloading find-handler.exe..."
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $ExePath -UseBasicParsing

    Write-Host '  Registering findanything:// protocol (HKCU)...'
    New-Item -Path $RegBase -Force | Out-Null
    Set-ItemProperty -Path $RegBase -Name '(Default)' -Value 'URL:Find Anything Protocol'
    New-ItemProperty -Path $RegBase -Name 'URL Protocol' -Value '' -PropertyType String -Force | Out-Null
    $CmdKey = "$RegBase\shell\open\command"
    New-Item -Path $CmdKey -Force | Out-Null
    Set-ItemProperty -Path $CmdKey -Name '(Default)' -Value "`"$ExePath`" `"%1`""

    Write-Host ''
    Write-Host 'Done. The findanything:// protocol handler is registered.'
    Write-Host "  Binary:   $ExePath"
    Write-Host "  Registry: $RegBase"
}

function Uninstall-Handler {
    Write-Host 'Uninstalling find-anything protocol handler...'
    if (Test-Path $RegBase) {
        Remove-Item -Recurse -Force -Path $RegBase
        Write-Host '  Registry keys removed.'
    }
    if (Test-Path $InstallDir) {
        Remove-Item -Recurse -Force -Path $InstallDir
        Write-Host "  Removed $InstallDir"
    }
    Write-Host 'Done.'
}

Install-Handler
