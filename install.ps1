<#
.SYNOPSIS
Installs jdk, the Java version manager for Windows.

.DESCRIPTION
Downloads the release zip for this machine's architecture, verifies its
SHA-256 checksum, extracts jdk.exe and jdk-shim.exe to an isolated staging
directory and runs `jdk setup --yes` — which does all the real work once:
copies jdk.exe into the store, materializes the shims, sets the persistent
JAVA_HOME, prepends the store's bin and shims directories to the user PATH
and broadcasts WM_SETTINGCHANGE so new consoles see it without a logoff.
The installer never duplicates any of that environment logic.

The checksum comes from the release's SHA256SUMS, verified against the
signing key embedded below (the same key `jdk update` has compiled in), so
the expected hash does not come from whoever served the zip.

Two things fall back to the per-file `.sha256` sidecar, with a loud warning,
and both are facts about this machine or this release's age rather than
choices the release host can make: `ssh-keygen` missing (every Windows 10
1809 and later ships it, but it can be absent or removed), and a release
older than v0.6.0, which publishes no SHA256SUMS at all. Everything else
aborts — a signature that does not verify, a SHA256SUMS with no line for
this zip, and a SHA256SUMS published without its signature, which means the
anchor was stripped.

jdk.exe itself honors JDK_ROOT (store location) and JDK_ENV_KEY (disposable
registry subkey for hermetic testing) from the environment, so this script
needs no parameters for either.

.PARAMETER Version
Release version to install, e.g. 0.1.0 or v0.1.0. Default: latest release.

.PARAMETER ZipPath
Hermetic override: install from a local release zip instead of downloading.
The checksum is still enforced — from a `<zip>.sha256` file next to it, or
from -Sha256.

.PARAMETER Sha256
Expected SHA-256 of the -ZipPath zip (hex, case-insensitive). Overrides the
`<zip>.sha256` sidecar file.

.PARAMETER DefineOnly
Hermetic override: define the functions and constants, then return without
downloading or changing anything. Exists so `.github/scripts/test_install_anchor.ps1`
can dot-source this file and drive Get-SignedHash against a loopback server.

.EXAMPLE
irm https://github.com/isacgalvao/jdk/releases/latest/download/install.ps1 | iex
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$ZipPath,
    [string]$Sha256,
    [switch]$DefineOnly
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 renders a byte-by-byte progress bar that slows
# Invoke-WebRequest downloads by an order of magnitude.
$ProgressPreference = 'SilentlyContinue'

$repo = 'isacgalvao/jdk'

# The release signing key, pinned here exactly as jdk-core pins it. Rotating
# it means rotating both, plus the copy in README.md and RELEASING.md — the
# procedure is in RELEASING.md.
$signingKey = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKufZs1YJGeiBZsIVZSpxMIR/1hmAu1/biqqKJzgDxu7'
$signingPrincipal = 'release@jdk'
$signingNamespace = 'jdk-release'

function Get-Arch {
    # WMI processor architecture: 9 = AMD64, 5 = ARM, 12 = ARM64 as
    # reported by Surface Pro X class machines.
    try {
        $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture
        switch ($cpu) {
            9 { return 'x64' }
            5 { return 'arm64' }
            12 { return 'arm64' }
        }
    } catch {
        # WMI unavailable — fall through to the environment.
    }
    # PROCESSOR_ARCHITEW6432 first: an emulated 32/64-bit process sees the
    # real machine architecture only there.
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { return 'x64' }
        'ARM64' { return 'arm64' }
    }
    throw "Unsupported processor architecture: $arch"
}

function Get-LatestTag {
    $release = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repo/releases/latest"
    return $release.tag_name
}

function Get-ExpectedHash([string]$Zip) {
    if ($Sha256) {
        return $Sha256.Trim().ToLowerInvariant()
    }
    $sidecar = "$Zip.sha256"
    if (-not (Test-Path $sidecar)) {
        throw "No checksum for $Zip : expected $sidecar next to it (or pass -Sha256)"
    }
    # `<hash>  <file>` (sha256sum format); the hash is the first token.
    $line = Get-Content $sidecar | Where-Object { $_.Trim() } | Select-Object -First 1
    if (-not $line) {
        throw "$sidecar is empty"
    }
    return ($line.Trim() -split '\s+')[0].ToLowerInvariant()
}

function Assert-Checksum([string]$Zip, [string]$Expected) {
    if (-not $Expected) {
        $Expected = Get-ExpectedHash $Zip
    }
    $actual = (Get-FileHash -Algorithm SHA256 -Path $Zip).Hash.ToLowerInvariant()
    if ($actual -ne $Expected) {
        throw "Checksum mismatch for $Zip - expected $Expected, got $actual. Aborting: the download may be corrupt or tampered with."
    }
    Write-Host "Checksum OK ($actual)"
}

# The hash the release's SIGNED SHA256SUMS records for $Name, or $null when
# there is nothing to check against — which only ever means something about
# THIS machine or THIS release's age, never something the release host chose:
# a pre-0.6.0 release (no SHA256SUMS at all), or no ssh-keygen here. The
# caller then falls back to the per-file sidecar, which only proves the
# download was not corrupted in transit.
#
# A release that publishes SHA256SUMS but not its signature is the one case
# that aborts instead: from 0.6.0 the pipeline writes both, so a missing
# signature beside a present SHA256SUMS is an anchor that was removed. Letting
# that fall back would hand the choice of "verified or not" to whoever serves
# the files. A signature that is present and fails to verify aborts too.
function Get-SignedHash([string]$AssetBase, [string]$Name, [string]$WorkDir) {
    $sums = Join-Path $WorkDir 'SHA256SUMS'
    $sig = "$sums.sig"
    # Two separate try blocks, deliberately: a single one cannot tell "this
    # release is older than signing" from "the signature was stripped", and
    # would answer both with the fallback.
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$AssetBase/SHA256SUMS" -OutFile $sums
    } catch {
        $status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
        if ($status -eq 404) {
            Write-Warning "This release predates signed checksums (added in v0.6.0). Falling back to the per-file .sha256 sidecar, which the release host also serves - it detects a corrupt download, not a substituted one."
            return $null
        }
        throw
    }
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$AssetBase/SHA256SUMS.sig" -OutFile $sig
    } catch {
        $status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
        if ($status -eq 404) {
            # No next step pointing at -ZipPath: that switch is exactly the way
            # around this check, and handing it to someone who just met a
            # stripped signature would be handing them the bypass.
            throw "This release publishes SHA256SUMS but no SHA256SUMS.sig - the signature was stripped. Aborting. Check the release page for SHA256SUMS.sig: if it is listed there, these files did not come from it; if it is not, the release is incomplete - report it and try again later."
        }
        throw
    }

    $sshKeygen = Get-Command ssh-keygen -ErrorAction SilentlyContinue
    if (-not $sshKeygen) {
        Write-Warning "ssh-keygen was not found on PATH, so the release signature CANNOT be checked here. Falling back to the per-file .sha256 sidecar, which the release host also serves - it detects a corrupt download, not a substituted one. Install OpenSSH Client (Settings > Optional features) and re-run to verify, or check SHA256SUMS.sig by hand on another machine."
        return $null
    }

    # An allowed_signers file naming one principal, one namespace, one key.
    # WriteAllText because ssh-keygen wants LF and no BOM.
    $allowed = Join-Path $WorkDir 'allowed_signers'
    [IO.File]::WriteAllText($allowed, "$signingPrincipal namespaces=""$signingNamespace"" $signingKey`n")
    # Start-Process, not a pipeline: ssh-keygen reads the signed message from
    # stdin, and piping through PowerShell would re-encode it and append a
    # newline - the signature is over the file's exact bytes.
    # Paths are quoted individually: Start-Process joins the argument list
    # with spaces and quotes nothing, and %TEMP% can sit under a user name
    # that has a space in it.
    $stdout = Join-Path $WorkDir 'verify.out'
    $stderr = Join-Path $WorkDir 'verify.err'
    $verify = Start-Process -FilePath $sshKeygen.Source -Wait -PassThru -NoNewWindow `
        -ArgumentList @(
            '-Y', 'verify',
            '-f', """$allowed""",
            '-I', $signingPrincipal,
            '-n', $signingNamespace,
            '-s', """$sig"""
        ) `
        -RedirectStandardInput $sums -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if ($verify.ExitCode -ne 0) {
        $detail = (Get-Content $stderr -Raw -ErrorAction SilentlyContinue)
        throw "The release signature over SHA256SUMS does not verify ($($detail -replace '\s+', ' ')). Aborting: these are not the checksums this project published."
    }
    Write-Host "Signature OK (SHA256SUMS signed by $signingPrincipal)"

    # `<hash>  <name>`; the name must be this asset exactly, so a line for
    # another release's zip can never answer for this one. -ceq and not -eq:
    # PowerShell compares case-insensitively by default, while the updater's
    # `sum_for` (jdk-core/src/release.rs) is exact — two anchors over the same
    # file disagreeing about which line covers an asset is not a difference
    # either of them should have.
    foreach ($line in Get-Content $sums) {
        $fields = $line.Trim() -split '\s+'
        if ($fields.Count -eq 2 -and $fields[1] -ceq $Name) {
            return $fields[0].ToLowerInvariant()
        }
    }
    throw "The signed SHA256SUMS of this release does not cover $Name. Aborting: the download cannot be tied to the release that was signed."
}

# Everything above is definitions; everything below installs. Dot-sourcing
# with -DefineOnly stops here, which is what lets the anchor test drive
# Get-SignedHash without this script touching the network or this machine.
if ($DefineOnly) { return }

# GitHub releases require TLS 1.2, which 5.1-era defaults may not enable.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$arch = Get-Arch
$tempRoot = Join-Path $env:TEMP ("jdk-install-" + [guid]::NewGuid().ToString())
$downloadDir = Join-Path $tempRoot 'download'
$extractDir = Join-Path $tempRoot 'extract'
New-Item -ItemType Directory -Force -Path $downloadDir, $extractDir | Out-Null

try {
    # Set on the download path only: the hash the release's signed SHA256SUMS
    # records for this zip. Empty on the -ZipPath path, and when there is no
    # signature to check — Assert-Checksum then falls back to the sidecar.
    $signedHash = ''
    if ($ZipPath) {
        $zip = (Resolve-Path $ZipPath).Path
        Write-Host "Installing from local zip: $zip"
    } else {
        if ($Version) {
            # \z and not $: in .NET, $ also matches just before a trailing
            # newline, so a -Version carrying one would pass into a URL.
            if ($Version -notmatch '^v?\d+(\.\d+)*\z') {
                throw "Invalid -Version '$Version': expected something like 0.1.0"
            }
            $tag = $Version
            if (-not $tag.StartsWith('v')) { $tag = "v$tag" }
        } else {
            $tag = Get-LatestTag
        }
        $zipName = "jdk-$tag-windows-$arch.zip"
        $assetBase = "https://github.com/$repo/releases/download/$tag"
        $zip = Join-Path $downloadDir $zipName
        Write-Host "Downloading $zipName ($tag, $arch)..."
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$assetBase/$zipName" -OutFile $zip
        } catch {
            $status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
            if ($status -eq 404) {
                throw "No $zipName in release $tag. arm64 builds are best-effort and may be absent from a release - try x64, or a newer release."
            }
            throw
        }
        $signedHash = Get-SignedHash $assetBase $zipName $downloadDir
        if (-not $signedHash) {
            # Only now: with a signed hash in hand the per-file sidecar is a
            # download nobody reads, and it is served by the same host as the
            # zip anyway - it proves the transfer, never the release.
            Invoke-WebRequest -UseBasicParsing -Uri "$assetBase/$zipName.sha256" -OutFile "$zip.sha256"
        }
    }

    Assert-Checksum $zip $signedHash

    Expand-Archive -Path $zip -DestinationPath $extractDir -Force
    $jdkExe = Get-ChildItem -Path $extractDir -Recurse -Filter 'jdk.exe' | Select-Object -First 1
    $shimExe = Get-ChildItem -Path $extractDir -Recurse -Filter 'jdk-shim.exe' | Select-Object -First 1
    if (-not $jdkExe -or -not $shimExe) {
        throw "The zip does not contain jdk.exe and jdk-shim.exe - not a jdk release archive?"
    }
    if ($jdkExe.DirectoryName -ne $shimExe.DirectoryName) {
        throw "jdk.exe and jdk-shim.exe are not side by side in the archive - jdk setup needs them together"
    }

    # setup copies the running jdk.exe into `<store>\bin`, materializes the
    # shims from the sibling jdk-shim.exe, writes JAVA_HOME, prepends bin
    # and shims to the user PATH (once each) and broadcasts the change.
    & $jdkExe.FullName setup --yes
    if ($LASTEXITCODE -ne 0) {
        throw "jdk setup exited with code $LASTEXITCODE"
    }

    Write-Host ''
    Write-Host 'jdk is installed. Next steps, in a NEW terminal (it picks up the PATH):'
    Write-Host '  jdk install temurin@21    # install a JDK (auto-set as global)'
    Write-Host '  jdk doctor                # verify the whole setup'
} finally {
    Remove-Item -Recurse -Force $tempRoot -ErrorAction SilentlyContinue
}
