<#
.SYNOPSIS
What an install channel has to deliver, whichever one placed the files.

.DESCRIPTION
winget, scoop and a hand-extracted release zip all end in the same place: a
jdk.exe that reports the released version, and from which `jdk setup` builds
a store that `jdk doctor` passes. Those are the claims the README makes for
all three, so they are checked in one place rather than three.

`jdk setup` is run here, not by the channel: winget has no post-install hook
and scoop's fire on `scoop update` as well, so this is the step the README
tells the reader to run afterwards. Failing it would mean the channel put
down something setup cannot work from - a jdk.exe with no jdk-shim.exe next
to it, most of all, which is the one packaging mistake no manifest linter
catches.

No JDK is installed here. That the released binary can install a real Temurin
is the `installer` job's claim, over the same bytes, and repeating it per
channel would only buy the same download five times.

.PARAMETER Jdk
The jdk.exe the channel placed - winget's Links shim, scoop's shim, or the
one extracted from the zip.

.PARAMETER Version
The version this release is expected to announce, `0.6.0`.
#>
param(
    [Parameter(Mandatory)] [string]$Jdk,
    [Parameter(Mandatory)] [string]$Version
)
$ErrorActionPreference = 'Stop'

function Assert([bool]$Condition, [string]$What) {
    if (-not $Condition) { throw "FAIL: $What" }
    Write-Host "ok: $What"
}

Assert (Test-Path $Jdk) "the channel placed $Jdk"

# `jdk 0.6.0` - the version compiled into the binary, so this is the one
# assertion that ties the file on disk to the release the manifest claimed.
$reported = (& $Jdk --version | Out-String).Trim()
Assert ($LASTEXITCODE -eq 0) 'jdk --version exits 0'
Assert ($reported -eq "jdk $Version") "jdk --version reports jdk $Version (got '$reported')"

& $Jdk setup --yes
Assert ($LASTEXITCODE -eq 0) 'jdk setup --yes exits 0'

# Setup's own output would say so, but these two are what every later command
# resolves through, and a channel that shipped jdk.exe without jdk-shim.exe
# produces exactly one of them.
$store = Join-Path $env:USERPROFILE '.jdk'
Assert (Test-Path (Join-Path $store 'bin\jdk.exe')) 'setup placed the store copy of jdk.exe'
Assert (Test-Path (Join-Path $store 'shims\java.exe')) 'setup materialized the shims'

& $Jdk doctor
Assert ($LASTEXITCODE -eq 0) 'jdk doctor exits 0'
