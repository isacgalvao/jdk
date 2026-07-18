<#
.SYNOPSIS
Installs jdk, the Windows-first Java version manager.

.DESCRIPTION
Downloads the release zip for this machine's architecture, verifies its
SHA-256 checksum, extracts jdk.exe and jdk-shim.exe to an isolated staging
directory and runs `jdk setup --yes` — which does all the real work once:
copies jdk.exe into the store, materializes the shims, sets the persistent
JAVA_HOME, prepends the store's bin and shims directories to the user PATH
and broadcasts WM_SETTINGCHANGE so new consoles see it without a logoff.
The installer never duplicates any of that environment logic.

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

.EXAMPLE
irm https://raw.githubusercontent.com/isacgalvao/jdk/master/install.ps1 | iex
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$ZipPath,
    [string]$Sha256
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 renders a byte-by-byte progress bar that slows
# Invoke-WebRequest downloads by an order of magnitude.
$ProgressPreference = 'SilentlyContinue'

$repo = 'isacgalvao/jdk'

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

function Assert-Checksum([string]$Zip) {
    $expected = Get-ExpectedHash $Zip
    $actual = (Get-FileHash -Algorithm SHA256 -Path $Zip).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "Checksum mismatch for $Zip - expected $expected, got $actual. Aborting: the download may be corrupt or tampered with."
    }
    Write-Host "Checksum OK ($actual)"
}

# GitHub releases require TLS 1.2, which 5.1-era defaults may not enable.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$arch = Get-Arch
$tempRoot = Join-Path $env:TEMP ("jdk-install-" + [guid]::NewGuid().ToString())
$downloadDir = Join-Path $tempRoot 'download'
$extractDir = Join-Path $tempRoot 'extract'
New-Item -ItemType Directory -Force -Path $downloadDir, $extractDir | Out-Null

try {
    if ($ZipPath) {
        $zip = (Resolve-Path $ZipPath).Path
        Write-Host "Installing from local zip: $zip"
    } else {
        if ($Version) {
            if ($Version -notmatch '^v?\d+(\.\d+)*$') {
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
        Invoke-WebRequest -UseBasicParsing -Uri "$assetBase/$zipName.sha256" -OutFile "$zip.sha256"
    }

    Assert-Checksum $zip

    # Future extension (plan decision 12): verify GitHub build provenance
    # here (`gh attestation verify`, warn-only when gh is absent) once the
    # release pipeline attests its artifacts.

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
