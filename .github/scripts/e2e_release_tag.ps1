# The latest release's tag and version, into the job's environment as TAG
# (`v0.6.0`) and VERSION (`0.6.0`) — what the channel jobs render manifests
# for and download assets from.
#
# Authenticated, unlike install.ps1's own lookup: anonymous api.github.com is
# rate limited per source IP, and hosted runners share those. The token is
# read-only and buys nothing but this call.
$ErrorActionPreference = 'Stop'

if (-not $env:GH_TOKEN) { throw 'GH_TOKEN is not set for this step' }

$release = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$env:GITHUB_REPOSITORY/releases/latest" -Headers @{
    Authorization = "Bearer $env:GH_TOKEN"
    'User-Agent'  = 'jdk-e2e'
}
$tag = $release.tag_name
if ($tag -notmatch '^v\d+(\.\d+)*$') {
    throw "the latest release is tagged '$tag', which is not a v<version> this can render a manifest for"
}

Write-Host "latest release: $tag"
"TAG=$tag" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"VERSION=$($tag -replace '^v', '')" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
