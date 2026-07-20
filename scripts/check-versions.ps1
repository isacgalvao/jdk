#!/usr/bin/env pwsh
# Guards the single-version invariant of the workspace. Fails when:
#   - an internal path-dependency pin does not echo the workspace version,
#   - the README MSRV badge drifts from rust-version,
#   - a crate outside the published set (jdk, jdk-core, jdk-resolve) is
#     publishable (a new internal crate that forgot publish.workspace = true).
# Run in CI and before cutting a release.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$fail = @()

# Source of truth: [workspace.package] version and rust-version.
$rootToml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
if ($rootToml -notmatch '(?m)^\s*version\s*=\s*"([^"]+)"') { throw 'Cargo.toml: no workspace version' }
$version = $Matches[1]
if ($rootToml -notmatch '(?m)^\s*rust-version\s*=\s*"([^"]+)"') { throw 'Cargo.toml: no rust-version' }
$msrv = $Matches[1]
Write-Host "workspace version = $version  |  MSRV = $msrv"

# Every internal path-dependency that pins a version must pin this one.
Get-ChildItem (Join-Path $root 'crates') -Recurse -Filter Cargo.toml | ForEach-Object {
    $rel = $_.FullName.Substring($root.Length + 1)
    foreach ($line in Get-Content $_.FullName) {
        if ($line -match 'path\s*=\s*"\.\.[^"]*"' -and $line -match 'version\s*=\s*"([^"]+)"') {
            if ($Matches[1] -ne $version) {
                $fail += "$rel pins an internal dep at $($Matches[1]), workspace is $version"
            }
        }
    }
}

# The workspace version must be documented before it can be tagged.
$changelog = Get-Content (Join-Path $root 'CHANGELOG.md') -Raw
$escaped = [regex]::Escape($version)
if ($changelog -notmatch "(?m)^## \[$escaped\]") {
    $fail += "CHANGELOG.md has no '## [$version]' section"
}

# The README MSRV badge must match rust-version.
$readme = Get-Content (Join-Path $root 'README.md') -Raw
if ($readme -match 'MSRV-([0-9][0-9.]*)') {
    if ($Matches[1] -ne $msrv) { $fail += "README MSRV badge is $($Matches[1]), rust-version is $msrv" }
} else {
    $fail += 'README has no MSRV badge'
}

# Exactly these three crates may reach crates.io.
$meta = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
$published = @($meta.packages | Where-Object { $null -eq $_.publish } | Select-Object -ExpandProperty name | Sort-Object)
$expected = @('jdk', 'jdk-core', 'jdk-resolve')
if ("$published" -ne "$expected") {
    $fail += "publishable crates are [$($published -join ', ')], expected [$($expected -join ', ')]"
}

if ($fail.Count -gt 0) {
    $fail | ForEach-Object { Write-Host "FAIL: $_" }
    exit 1
}
Write-Host 'version consistency OK'
