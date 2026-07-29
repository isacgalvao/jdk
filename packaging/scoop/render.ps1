<#
.SYNOPSIS
Renders jdk.json for one release, to be published as an asset of it.

.DESCRIPTION
The release job calls this with the hashes it has just computed, so the
manifest names the exact bytes of the zips it ships beside - it never
downloads anything to find them out - and it is itself covered by the
release's signed SHA256SUMS.

That is also why there is no scoop bucket: the manifest travels with the
release, `scoop install <url>` records the URL it came from, and `scoop
update` re-reads it. A `latest/download` URL always serves the newest one.

.PARAMETER Arm64Sha256
Empty when the release carries no arm64 zip - the aarch64 build is
best-effort. The arm64 entries are then dropped rather than left with a hole,
and scoop tells an arm64 user the app does not support their architecture.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$X64Sha256,
    [string]$Arm64Sha256,
    [Parameter(Mandatory)][string]$OutFile
)

$ErrorActionPreference = 'Stop'

# scoop versions carry no `v`, and the tag does - taking either means the tag
# can be pasted in without producing a .../download/vv0.6.0/... URL that only
# shows up as a 404 much later.
$Version = $Version -replace '^v', ''

$manifest = Get-Content (Join-Path $PSScriptRoot 'jdk.json') -Raw | ConvertFrom-Json
$manifest.version = $Version
$manifest.architecture.'64bit'.url = $manifest.architecture.'64bit'.url.Replace('__VERSION__', $Version)
$manifest.architecture.'64bit'.hash = $X64Sha256.ToLowerInvariant()
if ($Arm64Sha256) {
    $manifest.architecture.arm64.url = $manifest.architecture.arm64.url.Replace('__VERSION__', $Version)
    $manifest.architecture.arm64.hash = $Arm64Sha256.ToLowerInvariant()
} else {
    $manifest.architecture.PSObject.Properties.Remove('arm64')
    $manifest.autoupdate.architecture.PSObject.Properties.Remove('arm64')
}

$json = $manifest | ConvertTo-Json -Depth 10
# The placeholders are the only thing standing between a template and a
# manifest: one left behind is a hash nobody checked, or a URL that 404s.
if ($json -match '__[A-Z0-9_]+__') {
    throw "the rendered scoop manifest still holds the placeholder $($Matches[0])"
}
[IO.File]::WriteAllText($OutFile, "$json`n")
Write-Host "wrote $OutFile"
