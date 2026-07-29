<#
.SYNOPSIS
`jdk update` end to end, on the installation the real installer just made.

.DESCRIPTION
Two legs, proving two different things.

1. THE REAL ANCHOR. `jdk update` against this project's GitHub releases with
   the signing key COMPILED INTO the binary. Every case in
   crates/jdk/tests/update.rs replaces that key through JDK_RELEASE_PUBKEY -
   it has no choice, a loopback server cannot hold the release private key -
   so the pinned key verifying a real SHA256SUMS.sig is exercised in no test
   anywhere. The installed copy is the latest release, so the honest answer
   is "already up to date"; reaching it means the /latest redirect resolved
   and landed back on the same host, the real signature verified against the
   pinned key, and the two versions were compared.

2. THE REAL SWAP. A release v9.9.9 served from 127.0.0.1, signed with a
   throwaway key. JDK_RELEASE_PUBKEY is honored only alongside JDK_RELEASES
   and only for loopback (jdk-core/src/release.rs), so nothing here can
   re-anchor anything but a test.

   The payload is the published v0.5.0 zip, served under the v9.9.9 name: a
   real jdk.exe that runs and announces a version this machine did not have.
   `jdk --version` answering 0.5.0 afterwards is what proves the bytes now in
   the store are the ones the fake release served. Comparing hashes against a
   payload this script itself produced would not - a swap that never happened
   satisfies that comparison just as well when the payload is a copy of what
   was already there.

   The swap mechanics, the refusals and the leftover sweep are already
   covered by crates/jdk/tests/update.rs, against marker payloads that never
   execute. What is left for here is the store copy the REAL installer placed
   replacing itself WHILE RUNNING, and the result still being a jdk.exe that
   works.

Leaves the store on v0.5.0. Nothing may follow this script in a job.

.PARAMETER Jdk
The store copy to update: `<store>\bin\jdk.exe`, the only one `jdk update`
agrees to replace.
#>
param(
    [Parameter(Mandatory)] [string]$Jdk
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# The published release whose binaries stand in for a newer build. Any real
# release older than the installed one would do; this one is pinned so the
# version asserted below is a fact and not a lookup.
$payloadTag = 'v0.5.0'
$payloadVersion = '0.5.0'
# Higher than any release, so `decide` chooses to update without --force.
$fakeVersion = '9.9.9'

function Assert([bool]$Condition, [string]$What) {
    if (-not $Condition) { throw "FAIL: $What" }
    Write-Host "ok: $What"
}

$bin = Split-Path -Parent $Jdk
$root = Split-Path -Parent $bin

# ---------------------------------------------------------------- leg 1 ---
Write-Host '--- leg 1: the real release host, verified with the pinned key ---'
$out = (& $Jdk update 2>&1 | Out-String)
Write-Host $out
Assert ($LASTEXITCODE -eq 0) 'jdk update against the real release host exits 0'
Assert ($out -match 'already up to date') 'the installed copy is the latest release'

# ---------------------------------------------------------------- leg 2 ---
Write-Host ''
Write-Host "--- leg 2: a signed fake v$fakeVersion on loopback, payload $payloadTag ---"

# x64 only, and said out loud: the asset name the updater asks for carries
# the architecture, and a silent mismatch would look like a missing release.
$arch = $env:PROCESSOR_ARCHITEW6432
if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
if ($arch -ne 'AMD64') {
    throw "this leg is written for x64 and this machine reports $arch"
}
$asset = "jdk-v$fakeVersion-windows-x64.zip"

$work = Join-Path $env:RUNNER_TEMP ("jdk-e2e-update-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $work | Out-Null
$listener = $null
$server = $null
try {
    # The payload, checked against the sidecar the release publishes for it.
    # The updater will verify it again against the SHA256SUMS signed below -
    # this check is here so a truncated download fails while it still has an
    # obvious name, rather than as a signature mismatch three steps later.
    $payloadAsset = "jdk-$payloadTag-windows-x64.zip"
    $payloadBase = "https://github.com/$env:GITHUB_REPOSITORY/releases/download/$payloadTag"
    $zip = Join-Path $work $asset
    Invoke-WebRequest -UseBasicParsing -Uri "$payloadBase/$payloadAsset" -OutFile $zip
    # To a file and back, not through .Content: GitHub serves the sidecar as
    # application/octet-stream, and PowerShell 7 hands back a byte array for
    # anything it does not read as text.
    $sidecar = Join-Path $work 'payload.sha256'
    Invoke-WebRequest -UseBasicParsing -Uri "$payloadBase/$payloadAsset.sha256" -OutFile $sidecar
    $expected = (((Get-Content $sidecar -Raw).Trim() -split '\s+')[0]).ToLowerInvariant()
    $hash = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
    Assert ($hash -eq $expected) "the $payloadTag payload matches its published sidecar (expected $expected, got $hash)"

    # A throwaway signer standing in for the release key. What is under test
    # is the updater's verification, not the value of the pinned key - that
    # one is held to the table in RELEASING.md and parsed in jdk-core's tests.
    $keyFile = Join-Path $work 'signer'
    ssh-keygen -t ed25519 -N '' -C 'jdk e2e update' -f $keyFile -q
    if ($LASTEXITCODE -ne 0) { throw "ssh-keygen could not generate a test key (exit $LASTEXITCODE)" }
    $pubkey = ((Get-Content "$keyFile.pub" -Raw).Trim() -split '\s+')[0..1] -join ' '

    # WriteAllText for LF and no BOM: the signature is over these exact bytes.
    $sums = Join-Path $work 'SHA256SUMS'
    [IO.File]::WriteAllText($sums, "$hash  $asset`n")
    ssh-keygen -Y sign -f $keyFile -n jdk-release $sums 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "ssh-keygen could not sign the fixture (exit $LASTEXITCODE)" }

    # A port nobody holds, then handed straight to the HttpListener. The
    # listener needs the number up front and will not take a zero.
    $probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $probe.Start()
    $port = $probe.LocalEndpoint.Port
    $probe.Stop()
    $base = "http://127.0.0.1:$port"

    $routes = @{
        '/latest'                                  = @{ Location = "$base/tag/v$fakeVersion" }
        "/tag/v$fakeVersion"                       = @{ Body = [Text.Encoding]::ASCII.GetBytes('release page') }
        "/download/v$fakeVersion/SHA256SUMS"       = @{ Body = [IO.File]::ReadAllBytes($sums) }
        "/download/v$fakeVersion/SHA256SUMS.sig"   = @{ Body = [IO.File]::ReadAllBytes("$sums.sig") }
        "/download/v$fakeVersion/$asset"           = @{ Body = [IO.File]::ReadAllBytes($zip) }
    }

    # HttpListener rather than the raw socket loop test_install_anchor.ps1
    # uses: this one has to answer a redirect and stream a 4 MiB body with an
    # honest Content-Length, and both come free here.
    $listener = [System.Net.HttpListener]::new()
    $listener.Prefixes.Add("$base/")
    $listener.Start()
    $serve = {
        param([System.Net.HttpListener]$Listener, [hashtable]$Routes)
        while ($Listener.IsListening) {
            try { $context = $Listener.GetContext() } catch { break }
            try {
                $route = $Routes[$context.Request.Url.AbsolutePath]
                $reply = $context.Response
                if ($null -eq $route) {
                    $reply.StatusCode = 404
                    $body = [Text.Encoding]::ASCII.GetBytes('not found')
                } elseif ($route.Location) {
                    $reply.StatusCode = 302
                    $reply.RedirectLocation = $route.Location
                    $body = [byte[]]::new(0)
                } else {
                    $reply.StatusCode = 200
                    $body = $route.Body
                }
                $reply.ContentLength64 = $body.Length
                $reply.OutputStream.Write($body, 0, $body.Length)
                $reply.OutputStream.Close()
            } catch {
                # A client that hung up mid-response is not a server fault.
            }
        }
    }
    $server = [PowerShell]::Create()
    $null = $server.AddScript($serve).AddArgument($listener).AddArgument($routes)
    $null = $server.BeginInvoke()

    $before = Get-FileHash -Algorithm SHA256 -Path $Jdk
    $env:JDK_RELEASES = $base
    $env:JDK_RELEASE_PUBKEY = $pubkey
    try {
        $out = (& $Jdk update 2>&1 | Out-String)
    } finally {
        Remove-Item Env:JDK_RELEASES, Env:JDK_RELEASE_PUBKEY -ErrorAction SilentlyContinue
    }
    Write-Host $out
    Assert ($LASTEXITCODE -eq 0) 'jdk update against the fake release exits 0'
    # Matched without the arrow the message prints between the versions: what
    # is being checked is which versions it named, not the console encoding
    # the runner happened to decode them with.
    Assert ($out -match "updated .*$([regex]::Escape($fakeVersion))") "it reports updating to $fakeVersion"

    # The proof. This jdk.exe was 0.6.0 a moment ago, is the same path, was
    # running when it was replaced, and now answers as the payload.
    $reported = (& $Jdk --version | Out-String).Trim()
    Assert ($LASTEXITCODE -eq 0) 'the swapped-in jdk.exe runs'
    Assert ($reported -eq "jdk $payloadVersion") "it is the served payload (jdk --version says '$reported')"
    Assert ((Get-FileHash -Algorithm SHA256 -Path $Jdk).Hash -ne $before.Hash) 'the store copy changed on disk'

    Assert (Test-Path (Join-Path $bin 'jdk.exe.old')) 'the replaced binary was kept aside as jdk.exe.old'
    # The shims are rewritten from the bundle's jdk-shim.exe, which the update
    # parks next to jdk.exe. Equal bytes is what says they came from the NEW
    # template rather than surviving from the old one.
    $template = [IO.File]::ReadAllBytes((Join-Path $bin 'jdk-shim.exe'))
    $shim = [IO.File]::ReadAllBytes((Join-Path $root 'shims\java.exe'))
    Assert (@(Compare-Object $template $shim -SyncWindow 0).Count -eq 0) 'the shims were rewritten from the new template'

    Write-Host ''
    Write-Host "the store now holds jdk $payloadVersion - nothing may run after this script"
} finally {
    if ($listener) { $listener.Stop() }
    if ($server) { $server.Dispose() }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
