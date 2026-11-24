# Tests install.ps1 against fake releases on disk, without the network.
# It needs Windows PowerShell 5.1, which downloads file:// URLs (PowerShell
# 7 does not):
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File .github/scripts/test-install.ps1 -Zip PATH
#
# PATH is a real release archive, lcl-VERSION-x86_64-pc-windows-msvc.zip.
# Each fake release holds its lcl.exe, so a release named for another
# version holds an lcl.exe that prints the wrong one. Each case runs
# install.ps1 as a file, with LCL_NO_MODIFY_PATH set.
param(
    [Parameter(Mandatory = $true)] [string] $Zip
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\install.ps1')).ProviderPath
$target = 'x86_64-pc-windows-msvc'
$Zip = (Resolve-Path -LiteralPath $Zip).Path
if ((Split-Path -Leaf $Zip) -notmatch "^lcl-(.+)-$target\.zip$") { throw "$Zip is not named lcl-VERSION-$target.zip" }
$real = $Matches[1]

$work = Join-Path ([IO.Path]::GetTempPath()) ('lcl-install-test-' + [Guid]::NewGuid().ToString('N'))
$rel = Join-Path $work 'releases'
$bin = Join-Path $work 'bin'
$exe = Join-Path $work "unpacked\lcl-$real-$target\lcl.exe"
Expand-Archive -LiteralPath $Zip -DestinationPath (Join-Path $work 'unpacked')
$script:cases = 0
$script:failures = 0

# A release TAG whose archive holds the real lcl.exe.
function New-Release([string] $tag) {
    $version = $tag.Substring(1)
    $dir = Join-Path $rel $tag
    $folder = Join-Path $work "stage\$tag\lcl-$version-$target"
    New-Item -ItemType Directory -Force -Path $dir, $folder | Out-Null
    Copy-Item -LiteralPath $exe -Destination $folder
    $archive = Join-Path $dir "lcl-$version-$target.zip"
    Compress-Archive -Path $folder -DestinationPath $archive
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
    Set-Content -LiteralPath (Join-Path $dir 'SHA256SUMS') -Encoding Ascii -Value "$hash  lcl-$version-$target.zip"
}

# Runs install.ps1 with the defaults below and VARS on top; returns its
# exit code and its output.
function Invoke-Install([hashtable] $vars = @{}) {
    $env:LCL_INSTALL_DIR = $bin
    $env:LCL_RELEASES_URL = ([Uri] $rel).AbsoluteUri
    $env:LCL_NO_MODIFY_PATH = '1'
    $env:LCL_VERSION = "v$real"
    foreach ($k in $vars.Keys) { Set-Item -Path "env:$k" -Value $vars[$k] }
    $ErrorActionPreference = 'Continue'
    # Each line as its own string: Out-String would wrap at the console's width.
    $out = (& powershell -NoProfile -ExecutionPolicy Bypass -File $script 2>&1 | ForEach-Object { $_.ToString() }) -join "`n"
    return @{ Code = $LASTEXITCODE; Out = $out }
}

function Test-Case([string] $name, [bool] $ok, $result) {
    $script:cases++
    if ($ok) {
        "ok: $name"
    } else {
        $script:failures++
        "FAILED: $name`n  exit $($result.Code), output:`n$($result.Out)"
    }
}

function Get-Names { if (Test-Path -LiteralPath $bin) { @(Get-ChildItem -Force -LiteralPath $bin | ForEach-Object Name) -join ',' } else { '' } }

# Installs: the run exits 0, and $bin holds only an lcl.exe that runs.
function Test-Installed([string] $name, $result) {
    $printed = try { (& (Join-Path $bin 'lcl.exe') --version | Out-String).Trim() } catch { "failed to run: $($_.Exception.Message)" }
    Test-Case $name ($result.Code -eq 0 -and $printed -eq "lcl $real" -and (Get-Names) -eq 'lcl.exe') $result
}

# Refuses: the run exits 1, says WORDS, and $bin holds what it held before.
function Test-Refused([string] $name, [string] $words, $result, [string] $before = '') {
    Test-Case $name ($result.Code -eq 1 -and $result.Out.Contains($words) -and (Get-Names) -eq $before) $result
}

function Reset-Bin { Remove-Item -LiteralPath $bin -Recurse -Force -ErrorAction SilentlyContinue }

try {
    New-Release "v$real"

    Reset-Bin
    Test-Installed 'an install from the release' (Invoke-Install)
    Reset-Bin
    Test-Installed 'LCL_VERSION without the v' (Invoke-Install @{ LCL_VERSION = $real })
    Reset-Bin
    $r = Invoke-Install @{ LCL_INSTALL_DIR = (Join-Path $bin 'deeper') }
    Test-Case 'a missing install directory is made, and the PATH hint given' (
        $r.Code -eq 0 -and (Test-Path -LiteralPath (Join-Path $bin 'deeper\lcl.exe')) -and $r.Out.Contains('is not on your PATH')) $r

    # A failed download installs nothing.
    Reset-Bin
    Test-Refused "a release that doesn't exist" "can't download" (Invoke-Install @{ LCL_VERSION = 'v0.0.1' })
    New-Release 'v8.0.1'
    Remove-Item -LiteralPath (Join-Path $rel "v8.0.1\lcl-8.0.1-$target.zip")
    Reset-Bin
    Test-Refused 'a release without the archive' "can't download" (Invoke-Install @{ LCL_VERSION = 'v8.0.1' })
    New-Release 'v8.0.2'
    Remove-Item -LiteralPath (Join-Path $rel 'v8.0.2\SHA256SUMS')
    Reset-Bin
    Test-Refused 'a release without SHA256SUMS' 'SHA256SUMS: ' (Invoke-Install @{ LCL_VERSION = 'v8.0.2' })

    # A checksum that doesn't match installs nothing.
    New-Release 'v8.0.3'
    Set-Content -LiteralPath (Join-Path $rel 'v8.0.3\SHA256SUMS') -Encoding Ascii -Value ('0' * 64 + "  lcl-8.0.3-$target.zip")
    Reset-Bin
    Test-Refused "an archive that doesn't match SHA256SUMS" 'does not match SHA256SUMS' (Invoke-Install @{ LCL_VERSION = 'v8.0.3' })
    New-Release 'v8.0.4'
    Set-Content -LiteralPath (Join-Path $rel 'v8.0.4\SHA256SUMS') -Encoding Ascii -Value ('0' * 64 + '  SKILL.md')
    Reset-Bin
    Test-Refused 'SHA256SUMS without the archive' 'has no line for' (Invoke-Install @{ LCL_VERSION = 'v8.0.4' })

    # A binary that doesn't print the release's version installs nothing.
    New-Release 'v8.0.5'
    Reset-Bin
    Test-Refused 'a binary that prints another version' "it printed: lcl $real" (Invoke-Install @{ LCL_VERSION = 'v8.0.5' })

    # Over an earlier install: a refused one leaves it, a good one replaces it.
    Reset-Bin
    New-Item -ItemType Directory -Path $bin | Out-Null
    Set-Content -LiteralPath (Join-Path $bin 'lcl.exe') -Value 'earlier'
    $r = Invoke-Install @{ LCL_VERSION = 'v8.0.3' }
    Test-Case 'a refused install leaves the earlier lcl.exe' (
        $r.Code -eq 1 -and (Get-Content -LiteralPath (Join-Path $bin 'lcl.exe')) -eq 'earlier' -and (Get-Names) -eq 'lcl.exe') $r
    Test-Installed 'an install replaces the earlier lcl.exe' (Invoke-Install)

    # An lcl.exe in the install directory that is a directory.
    Reset-Bin
    New-Item -ItemType Directory -Path (Join-Path $bin 'lcl.exe') | Out-Null
    Test-Refused 'an lcl.exe that is a directory' 'is a directory' (Invoke-Install) 'lcl.exe'
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

"$script:cases cases, $script:failures failed"
if ($script:failures -gt 0) { exit 1 }
