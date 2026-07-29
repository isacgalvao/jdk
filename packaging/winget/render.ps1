<#
.SYNOPSIS
Renders the winget manifest templates in this folder for a published release.

.DESCRIPTION
Produces the three files a winget-pkgs submission needs, with the SHA-256 of
each release zip taken from that release's SIGNED SHA256SUMS - the same anchor
install.ps1 and `jdk update` use. The hashes are not read from the zips: a
manifest is a claim about bytes winget will download later, so the claim is
made against the signature the project published, not against whatever a
download produced here.

A release with no arm64 zip - the aarch64 build is best-effort - renders an
x64-only manifest rather than failing. A release with no x64 zip is a broken
release, and does fail.

This is the FIRST submission only. Once the package exists in winget-pkgs, the
`winget` job in .github/workflows/release.yml carries each new version forward
with komac, which copies the metadata from the version already there.

.PARAMETER Tag
The release to render, `v0.6.0` or `0.6.0`.

.PARAMETER OutDir
Where to write the three files. In a winget-pkgs clone that is
manifests\i\isacgalvao\jdk\<version>\. Nothing lands there until all three
have rendered.

.EXAMPLE
.\render.ps1 -Tag v0.6.0 -OutDir ..\..\..\winget-pkgs\manifests\i\isacgalvao\jdk\0.6.0
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Tag,
    [Parameter(Mandatory)][string]$OutDir
)

$ErrorActionPreference = 'Stop'

# Before anything else assigns: install.ps1 has a `-Version` parameter, and
# dot-sourcing binds every parameter of its param block into this scope. Doing
# this first means the script's own variables are set afterwards and survive.
# What it brings in is Get-SignedHash plus $repo and the pinned signing key.
. (Join-Path $PSScriptRoot '../../install.ps1') -DefineOnly

$version = $Tag -replace '^v', ''
# A parameter added to install.ps1 that happened to share a name with one of
# these would be bound to an empty string by the dot-source above, and an
# empty version renders URLs that only fail as a 404 much later.
if (-not $version) {
    throw "no version to render: -Tag came through empty, which is what happens when install.ps1 declares a parameter of the same name"
}
$tag = "v$version"
$assetBase = "https://github.com/$repo/releases/download/$tag"

$work = Join-Path ([IO.Path]::GetTempPath()) ("jdk-winget-" + [guid]::NewGuid())
$staged = Join-Path $work 'manifest'
New-Item -ItemType Directory -Force -Path $work, $staged | Out-Null
try {
    $hashes = @{}
    foreach ($arch in 'x64', 'arm64') {
        $asset = "jdk-$tag-windows-$arch.zip"
        try {
            Invoke-WebRequest -UseBasicParsing -Method Head -Uri "$assetBase/$asset" | Out-Null
        } catch {
            # Only a 404 means the release does not carry this architecture. A
            # timeout or a 5xx would otherwise turn a release that has both
            # into an x64-only manifest - and this one is submitted to
            # winget-pkgs by hand, where it stays.
            $status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
            if ($status -ne 404) { throw }
            Write-Warning "$tag publishes no $asset, so the manifest will not offer $arch."
            continue
        }
        # $null means Get-SignedHash fell back: no ssh-keygen here, or a
        # release older than signed checksums. Both are fine for installing on
        # one machine and neither is enough to publish a hash to the world.
        $hash = Get-SignedHash $assetBase $asset $work
        if (-not $hash) {
            throw "Could not verify the signed SHA256SUMS of $tag, so the hash for $asset is unproven. Install the OpenSSH client and re-run - see the warning above for which check was skipped."
        }
        $hashes[$arch] = $hash.ToUpperInvariant()
    }
    if (-not $hashes.ContainsKey('x64')) {
        throw "$tag publishes no x64 zip - there is no manifest to render from it"
    }

    $published = (Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repo/releases/tags/$tag").published_at
    $date = ([datetime]$published).ToString('yyyy-MM-dd')

    foreach ($template in Get-ChildItem $PSScriptRoot -Filter '*.yaml') {
        # The header line names the placeholders, so it goes before the guard
        # below sees it - and it would be a lie in the rendered file anyway,
        # which is submitted as-is.
        $text = ((Get-Content $template.FullName) -notmatch '^# Template:') -join "`n"
        $text = $text.
            Replace('__VERSION__', $version).
            Replace('__SHA256_X64__', $hashes['x64']).
            Replace('__RELEASE_DATE__', $date)
        if ($hashes.ContainsKey('arm64')) {
            $text = $text.Replace('__SHA256_ARM64__', $hashes['arm64'])
        } else {
            # Drop the arm64 entry whole - its `- Architecture:` line and the
            # deeper-indented lines under it - rather than leave a hash-shaped
            # hole. Deliberately NOT paired with a Replace of the placeholder:
            # if this match ever stops working, the guard below still finds it.
            $text = $text -replace '(?m)^  - Architecture: arm64\n(?:^    .*\n)*', ''
        }
        if ($text -match '__[A-Z0-9_]+__') {
            throw "$($template.Name) still holds the placeholder $($Matches[0]) after rendering"
        }
        # WriteAllText for LF and no BOM: winget-pkgs takes either, but a file
        # that round-trips through git the same way it left here is one less
        # thing to explain in a PR.
        [IO.File]::WriteAllText((Join-Path $staged $template.Name), "$text`n")
    }

    # Only now that all three rendered. A throw halfway through used to leave a
    # partial manifest sitting in a winget-pkgs clone, which is the one place
    # where a half-written file gets committed by accident.
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    foreach ($file in Get-ChildItem $staged -File) {
        Copy-Item $file.FullName $OutDir -Force
        Write-Host "wrote $(Join-Path $OutDir $file.Name)"
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

if (Get-Command winget -ErrorAction SilentlyContinue) {
    winget validate --manifest $OutDir
} else {
    Write-Warning "winget is not on PATH, so the rendered manifest was not validated. Run 'winget validate --manifest $OutDir' on a machine that has it before submitting."
}
