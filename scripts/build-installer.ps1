<#
.SYNOPSIS
    Build dist\ttk-setup-<version>-x86_64-windows.exe, the one-file installer.

.DESCRIPTION
    1. builds ttk.exe in release mode,
    2. builds ttk-setup.exe with that ttk.exe embedded in it,
    3. copies the result to dist\ and prints its SHA-256.

    Double-click the result, or run it with --yes for an unattended install.
#>
[CmdletBinding()]
param(
    [string]$Target = 'x86_64-pc-windows-msvc'
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
Push-Location $Root
try {
    $version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value

    Write-Host "==> building ttk $version ($Target)" -ForegroundColor Magenta
    cargo build --locked --release --target $Target -p ttk-cli
    if ($LASTEXITCODE -ne 0) { throw 'building ttk failed' }

    $payload = Join-Path $Root "target\$Target\release\ttk.exe"
    $env:TTK_INSTALLER_PAYLOAD = $payload

    Write-Host "==> building the installer around $payload" -ForegroundColor Magenta
    cargo build --locked --release --target $Target -p ttk-installer
    if ($LASTEXITCODE -ne 0) { throw 'building the installer failed' }

    New-Item -ItemType Directory -Force -Path dist | Out-Null
    $out = Join-Path $Root "dist\ttk-setup-$version-x86_64-windows.exe"
    Copy-Item "target\$Target\release\ttk-setup.exe" $out -Force

    $hash = (Get-FileHash -Algorithm SHA256 $out).Hash.ToLower()
    $size = '{0:N1} MiB' -f ((Get-Item $out).Length / 1MB)
    Write-Host ''
    Write-Host "  $out" -ForegroundColor Green
    Write-Host "  $size  sha256 $hash" -ForegroundColor DarkGray
} finally {
    Remove-Item Env:\TTK_INSTALLER_PAYLOAD -ErrorAction SilentlyContinue
    Pop-Location
}
