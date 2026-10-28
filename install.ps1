# USBackup installer — downloads the latest release binary from GitHub.
#
# Usage:
#   irm https://raw.githubusercontent.com/YoannDev90/USBackup/master/install.ps1 | iex
#
#   # or with options (save the script first, then run it):
#   .\install.ps1 -Prefix $env:LOCALAPPDATA\Programs\USBackup -Version 1.2.3
#
# Options:
#   -Prefix DIR      Install directory (default: %LOCALAPPDATA%\Programs\USBackup)
#   -Version X.Y.Z   Install a specific version instead of latest
#   -NoService       Skip the scheduled-task prompt
#   -Force           Overwrite an existing binary without asking
#   -Help            Show this help

[CmdletBinding()]
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\USBackup'),
    [string]$Version = '',
    [switch]$NoService,
    [switch]$Force,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 defaults to TLS 1.0, which GitHub rejects.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = 'YoannDev90/USBackup'
$BinName = 'USBackup.exe'
$Api = "https://api.github.com/repos/$Repo/releases"

function Info { param([string]$Msg) Write-Host "  + $Msg" -ForegroundColor Green }
function Warn { param([string]$Msg) Write-Host "  ! $Msg" -ForegroundColor Yellow }
function Die  { param([string]$Msg) Write-Host "  x $Msg" -ForegroundColor Red; exit 1 }

if ($Help) {
    # The leading comment block doubles as the usage text.
    $lines = @()
    if ($MyInvocation.MyCommand.Path) {
        foreach ($line in (Get-Content -Path $MyInvocation.MyCommand.Path)) {
            if ($line -notmatch '^\s*#') { break }
            $lines += $line -replace '^\s*#\s?', ''
        }
    }
    if (-not $lines) {
        $lines = @('Usage: install.ps1 [-Prefix DIR] [-Version X.Y.Z] [-NoService] [-Force]')
    }
    $lines | ForEach-Object { Write-Host $_ }
    exit 0
}

# ---------------------------------------------------------------- preflight
Write-Host 'USBackup installer'
Write-Host ''

switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { $Arch = 'x64' }
    'ARM64' { $Arch = 'arm64' }
    default { Die "Unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}

$Asset = "usbackup-windows-$Arch.exe"
$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "usbackup-install-$PID"
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    # ---------------------------------------------------------------- resolve version
    if ($Version) {
        $ReleaseUrl = "$Api/tags/v$Version"
    } else {
        $ReleaseUrl = "$Api/latest"
    }
    Write-Host "-> Resolving release from $ReleaseUrl ..."
    $headers = @{
        Accept     = 'application/vnd.github+json'
        'User-Agent' = 'usbackup-installer'
    }
    try {
        $release = Invoke-RestMethod -Uri $ReleaseUrl -Headers $headers
    } catch {
        Die 'Could not fetch release info. Does this version exist?'
    }

    $resolved = "$($release.tag_name)".TrimStart('v')
    $assetInfo = $release.assets | Where-Object { $_.name -eq $Asset } | Select-Object -First 1
    if (-not $assetInfo) {
        Die "No release asset found for '$Asset' (version $resolved)."
    }
    Info "Version $resolved - $Asset"

    # ---------------------------------------------------------------- download
    Write-Host "-> Downloading $Asset ..."
    $Downloaded = Join-Path $TmpDir $Asset
    Invoke-WebRequest -Uri $assetInfo.browser_download_url -OutFile $Downloaded -UseBasicParsing
    Info 'Binary downloaded'

    # ---------------------------------------------------------------- checksum
    Write-Host '-> Verifying SHA256 checksum ...'
    $ChecksumsUrl = "$($assetInfo.browser_download_url -replace '/[^/]+$', '')/SHA256SUMS"
    try {
        $ChecksumsFile = Join-Path $TmpDir 'SHA256SUMS'
        Invoke-WebRequest -Uri $ChecksumsUrl -OutFile $ChecksumsFile -UseBasicParsing -ErrorAction Stop
        $pattern = '[ /]' + [regex]::Escape($Asset) + '\s*$'
        $entry = Select-String -Path $ChecksumsFile -Pattern $pattern | Select-Object -First 1
        if ($entry) {
            $expected = ("$($entry.Line)" -split '\s+')[0].ToLowerInvariant()
            $actual = (Get-FileHash -Path $Downloaded -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actual -ne $expected) {
                Die 'Checksum verification FAILED - refusing to install.'
            }
            Info 'Checksum OK'
        } else {
            Warn "No checksum entry for $Asset in SHA256SUMS; skipping verification."
        }
    } catch {
        if ("$_" -match 'refusing to install') { throw }
        Warn 'SHA256SUMS not found in this release; skipping verification.'
    }

    # ---------------------------------------------------------------- install binary
    New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
    $Dest = Join-Path $Prefix $BinName

    if ((Test-Path $Dest) -and -not $Force) {
        if (-not [Console]::IsInputRedirected) {
            $ans = Read-Host "$Dest already exists. Overwrite? [y/N]"
            if ($ans -notmatch '^[Yy]') { Die 'Aborted.' }
        } else {
            Die "$Dest exists. Re-run with -Force to overwrite."
        }
    }

    Copy-Item -Path $Downloaded -Destination $Dest -Force
    Info "Installed to $Dest"

    # PATH hint
    $pathDirs = $env:PATH -split [System.IO.Path]::PathSeparator
    if ($pathDirs -notcontains $Prefix) {
        Warn "$Prefix is not in your PATH. Add it for the current session:"
        Write-Host "      `$env:PATH += '$([System.IO.Path]::PathSeparator)$Prefix'"
    }

    # ---------------------------------------------------------------- scheduled task
    $InstallService = 'no'
    if ($NoService) {
        $InstallService = 'no'
    } elseif ([Console]::IsInputRedirected) {
        Info "Tip: run '$Dest --install-service' to enable the background agent."
    } else {
        $ans = Read-Host 'Install and enable the scheduled task? [Y/n]'
        if ($ans -notmatch '^[Nn]') { $InstallService = 'yes' }
    }

    if ($InstallService -eq 'yes') {
        Write-Host '-> Installing scheduled task ...'
        & $Dest --install-service
        if ($LASTEXITCODE -eq 0) {
            Info 'Service installed. It starts automatically at logon.'
        } else {
            Warn "Service installation failed; you can retry with: $Dest --install-service"
        }
    }

    Write-Host ''
    Info "Done! Run '$BinName --help' to get started."
} finally {
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
