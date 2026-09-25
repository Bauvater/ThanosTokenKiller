<#
.SYNOPSIS
    ThanosTokenKiller installer for Windows.

.DESCRIPTION
    irm https://raw.githubusercontent.com/Bauvater/ThanosTokenKiller/main/install.ps1 | iex

    What it does, in order:

      1. downloads the release archive for x86_64 Windows,
      2. downloads the checksum file and refuses to continue if they disagree,
      3. unpacks ttk.exe into %LOCALAPPDATA%\Programs\ttk,
      4. hands over to `ttk setup`, which does the rest and explains itself.

    Nothing is installed machine-wide, nothing needs an elevated prompt, and
    nothing is written outside that one directory until `ttk setup` asks you.

.PARAMETER Version
    Install a specific release, e.g. -Version v0.1.0. Defaults to the latest.

.PARAMETER InstallDir
    Somewhere other than %LOCALAPPDATA%\Programs\ttk.

.PARAMETER NoSetup
    Just install the binary; skip the walkthrough.
#>
[CmdletBinding()]
param(
    [string]$Version = $env:TTK_VERSION,
    [string]$InstallDir = $env:TTK_INSTALL_DIR,
    [switch]$NoSetup
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # the progress bar makes downloads slower

$Repo = 'Bauvater/ThanosTokenKiller'
if (-not $InstallDir) {
    $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\ttk'
}

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------
$UseColour = $Host.UI.SupportsVirtualTerminal -and -not $env:NO_COLOR
function Paint($code, $text) {
    if ($UseColour) { "$([char]27)[${code}m${text}$([char]27)[0m" } else { $text }
}
function Step($text) { Write-Host "$(Paint '1;95' '==>') $(Paint '1' $text)" }
function Info($text) { Write-Host "    $(Paint '90' $text)" }
function Ok($text)   { Write-Host "    $(Paint '32' '+') $text" }
function Warn($text) { Write-Host "    $(Paint '33' '!') $text" }
function Die($text) {
    Write-Host "$(Paint '31' 'x') $text"
    exit 1
}

Write-Host ''
Write-Host "  $(Paint '1;95' 'ThanosTokenKiller') $(Paint '90' '- the only Claude Code token killer you will ever need')"
Write-Host ''

# ---------------------------------------------------------------------------
# Which build?
# ---------------------------------------------------------------------------
if ([System.Environment]::Is64BitOperatingSystem -eq $false) {
    Die "there is no 32-bit build. Build from source instead:`n   git clone https://github.com/$Repo"
}
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -eq 'ARM64') {
    Die @"
there is no ARM64 build yet. The project only ships binaries it tests on CI.
   Build from source instead:
     git clone https://github.com/$Repo
     cd ThanosTokenKiller
     cargo build --release
     .\target\release\ttk.exe setup
"@
}
$Target = 'x86_64-pc-windows-msvc'

if (-not $Version) {
    try {
        # The redirect from /releases/latest carries the tag, so this needs no
        # API token and no JSON parsing.
        $response = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" `
            -MaximumRedirection 0 -ErrorAction SilentlyContinue
        $location = $response.Headers.Location
        if ($location -is [array]) { $location = $location[0] }
        $Version = ($location -split '/')[-1]
    } catch {
        $Version = ($_.Exception.Response.Headers.Location.ToString() -split '/')[-1]
    }
}
if (-not $Version -or -not $Version.StartsWith('v')) {
    Die 'could not work out the latest version. Pin one with -Version v0.1.0.'
}

$Bare = $Version.TrimStart('v')
$Archive = "ttk-$Bare-$Target.zip"
$Base = "https://github.com/$Repo/releases/download/$Version"

Step "downloading ttk $Version"
Info $Target

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) "ttk-install-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null
try {
    $ArchivePath = Join-Path $Tmp $Archive
    try {
        Invoke-WebRequest -Uri "$Base/$Archive" -OutFile $ArchivePath
    } catch {
        Die "cannot download $Base/$Archive`n   If that release exists, check your network; otherwise pin one with -Version."
    }

    # -----------------------------------------------------------------------
    Step 'verifying the download'
    $SumsPath = Join-Path $Tmp 'SHA256SUMS'
    $verified = $false
    try {
        Invoke-WebRequest -Uri "$Base/SHA256SUMS" -OutFile $SumsPath
        $line = Get-Content $SumsPath | Where-Object { $_ -match [regex]::Escape($Archive) }
        if ($line) {
            $expected = ($line -split '\s+')[0]
            $actual = (Get-FileHash -Algorithm SHA256 -Path $ArchivePath).Hash.ToLower()
            if ($expected.ToLower() -ne $actual) {
                Die "checksum mismatch. Do not run this binary.`n   expected $expected`n   got      $actual"
            }
            Ok 'sha256 matches the published checksum'
            $verified = $true
        }
    } catch {
        # Falls through to the warning below.
    }
    if (-not $verified) {
        Warn "could not verify the download against a published checksum"
    }

    # -----------------------------------------------------------------------
    Step 'installing'
    Expand-Archive -Path $ArchivePath -DestinationPath $Tmp -Force
    $Bin = Get-ChildItem -Path $Tmp -Filter 'ttk.exe' -Recurse | Select-Object -First 1
    if (-not $Bin) { Die 'the archive did not contain ttk.exe' }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $Dest = Join-Path $InstallDir 'ttk.exe'
    # A running ttk.exe cannot be overwritten, but it can be renamed out of the
    # way — which is how an upgrade works while a shell still has it open.
    if (Test-Path $Dest) {
        $Old = Join-Path $InstallDir "ttk.exe.old"
        if (Test-Path $Old) { Remove-Item $Old -Force -ErrorAction SilentlyContinue }
        try { Move-Item $Dest $Old -Force } catch { }
    }
    Copy-Item $Bin.FullName $Dest -Force
    Ok $Dest

    $installed = & $Dest --version 2>$null
    if ($LASTEXITCODE -ne 0) { Die 'the installed binary does not run on this machine' }
    Ok $installed
} finally {
    Remove-Item $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
if ($NoSetup -or $env:TTK_NO_SETUP) {
    Write-Host ''
    Info "run ``$InstallDir\ttk.exe setup`` when you are ready."
    exit 0
}

Write-Host ''
Step 'setting up'
Info 'ttk takes it from here - every step asks first.'
Write-Host ''
& (Join-Path $InstallDir 'ttk.exe') setup
exit $LASTEXITCODE
