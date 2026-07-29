# Hermetic test of install.ps1's release-checksum anchor (SEC-03): which
# checksum the installer trusts, and when it refuses instead of falling back.
#
# install.ps1 is dot-sourced with -DefineOnly, so its functions run against a
# loopback server serving files from a temp directory - no real network, no
# release, nothing installed. The signing key is a throwaway pair generated
# here and assigned over $signingKey, because what is under test is the
# verification logic, not the value of the pinned key (that one is held to the
# five-places table in RELEASING.md, and jdk-core parses it in its own tests).
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

function Assert([bool]$Condition, [string]$What) {
    if (-not $Condition) { throw "FAIL: $What" }
    Write-Host "ok: $What"
}

function Assert-Throws([scriptblock]$Action, [string]$Pattern, [string]$What) {
    $message = $null
    try { & $Action | Out-Null } catch { $message = $_.Exception.Message }
    if ($null -eq $message) { throw "FAIL: $What returned instead of throwing" }
    if ($message -notlike "*$Pattern*") {
        throw "FAIL: $What threw '$message', expected to mention '$Pattern'"
    }
    Write-Host "ok: $What"
}

# One request per connection, files straight off disk, 404 for anything the
# case did not lay down - which is how "this release has no SHA256SUMS.sig"
# is expressed.
$serve = {
    param([System.Net.Sockets.TcpListener]$Listener, [string]$Root)
    while ($true) {
        try { $client = $Listener.AcceptTcpClient() } catch { break }
        try {
            $stream = $client.GetStream()
            $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::ASCII)
            $requestLine = $reader.ReadLine()
            while (-not [string]::IsNullOrEmpty($reader.ReadLine())) { }

            $path = ($requestLine -split ' ')[1]
            $file = Join-Path $Root $path.TrimStart('/')
            if (Test-Path -PathType Leaf $file) {
                $status = '200 OK'
                $body = [System.IO.File]::ReadAllBytes($file)
            } else {
                $status = '404 Not Found'
                $body = [System.Text.Encoding]::ASCII.GetBytes('not found')
            }
            $head = "HTTP/1.1 $status`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n"
            $headBytes = [System.Text.Encoding]::ASCII.GetBytes($head)
            $stream.Write($headBytes, 0, $headBytes.Length)
            $stream.Write($body, 0, $body.Length)
            $stream.Flush()
        } catch {
            # A client that hung up mid-response is not a server fault.
        } finally {
            $client.Close()
        }
    }
}

$root = Join-Path $env:TEMP ("jdk-anchor-test-" + [guid]::NewGuid().ToString())
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$server = $null

try {
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $server = [PowerShell]::Create()
    $null = $server.AddScript($serve).AddArgument($listener).AddArgument($root)
    $null = $server.BeginInvoke()

    $assetBase = "http://127.0.0.1:$port/download/v9.9.9"
    $served = Join-Path $root 'download/v9.9.9'
    New-Item -ItemType Directory -Force -Path $served | Out-Null

    $zipName = 'jdk-v9.9.9-windows-x64.zip'
    $zipHash = 'a' * 64

    # Dot-source: brings in Get-SignedHash plus the constants it reads, and
    # returns before the install body.
    . (Join-Path $PSScriptRoot '../../install.ps1') -DefineOnly

    # A throwaway signer, standing in for the release key.
    $keyDir = Join-Path $root 'key'
    New-Item -ItemType Directory -Force -Path $keyDir | Out-Null
    $keyFile = Join-Path $keyDir 'signer'
    ssh-keygen -t ed25519 -N '' -C 'jdk anchor test' -f $keyFile -q
    if ($LASTEXITCODE -ne 0) { throw "ssh-keygen could not generate a test key (exit $LASTEXITCODE)" }
    $signingKey = ((Get-Content "$keyFile.pub" -Raw).Trim() -split '\s+')[0..1] -join ' '

    # Signs $Text as the release pipeline does and publishes both halves.
    function Publish-Sums([string]$Text, [switch]$WithoutSignature, [switch]$Tampered) {
        Get-ChildItem $served -File | Remove-Item -Force
        $local = Join-Path $root 'SHA256SUMS'
        [IO.File]::WriteAllText($local, $Text)
        # `ssh-keygen -Y sign` asks before overwriting an existing .sig and,
        # with nothing on stdin to answer with, keeps the old signature and
        # still exits 0 - so the previous case's signature would silently
        # travel into this one.
        Remove-Item "$local.sig" -Force -ErrorAction SilentlyContinue
        ssh-keygen -Y sign -f $keyFile -n jdk-release $local 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "ssh-keygen could not sign the fixture (exit $LASTEXITCODE)" }

        # One flipped character in the recorded hash, after the signature was
        # made over the honest text.
        if ($Tampered) { [IO.File]::WriteAllText($local, ($Text -replace '^.', 'b')) }
        Copy-Item $local (Join-Path $served 'SHA256SUMS')
        if (-not $WithoutSignature) {
            Copy-Item "$local.sig" (Join-Path $served 'SHA256SUMS.sig')
        }
    }

    # A fresh work directory per case: what a case downloads must never be
    # what the previous one left behind.
    function New-WorkDir([string]$Case) {
        $dir = Join-Path $root "work-$Case"
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        return $dir
    }

    $sums = "$zipHash  $zipName`ndeadbeef$('0' * 56)  install.ps1`n"

    # 1. Signed release: the hash comes from SHA256SUMS, not from the sidecar.
    Publish-Sums $sums
    $hash = Get-SignedHash $assetBase $zipName (New-WorkDir 'signed')
    Assert ($hash -eq $zipHash) "a signed SHA256SUMS answers with the hash it records for $zipName"

    # 2. The anchor was stripped: SHA256SUMS is there, its signature is not.
    #    The one case that must abort rather than fall back - otherwise
    #    "verified or not" becomes the release host's choice.
    Publish-Sums $sums -WithoutSignature
    Assert-Throws { Get-SignedHash $assetBase $zipName (New-WorkDir 'stripped') } `
        'signature was stripped' 'SHA256SUMS without its .sig aborts'

    # 3. A release older than v0.6.0 publishes neither: fall back to the
    #    per-file sidecar, with a warning.
    Get-ChildItem $served -File | Remove-Item -Force
    $fallback = Get-SignedHash $assetBase $zipName (New-WorkDir 'unsigned')
    Assert ($null -eq $fallback) 'a release with no SHA256SUMS falls back to the sidecar'

    # 4. The sums changed after they were signed.
    Publish-Sums $sums -Tampered
    Assert-Throws { Get-SignedHash $assetBase $zipName (New-WorkDir 'tampered') } `
        'does not verify' 'a SHA256SUMS edited after signing aborts'

    # 5. Signed, verifies, and says nothing about this asset.
    Publish-Sums "deadbeef$('0' * 56)  install.ps1`n"
    Assert-Throws { Get-SignedHash $assetBase $zipName (New-WorkDir 'uncovered') } `
        'does not cover' 'a signed SHA256SUMS with no line for the zip aborts'

    # 6. The line names the asset in another case. PowerShell would call that
    #    a match; the updater's `sum_for` would not, and the two anchors over
    #    one file have to agree.
    Publish-Sums "$zipHash  $($zipName.ToUpperInvariant())`n"
    Assert-Throws { Get-SignedHash $assetBase $zipName (New-WorkDir 'miscased') } `
        'does not cover' 'a line naming the asset in another case does not answer for it'

    # 7. A signature that is well-formed and made by SOMEONE ELSE. Distinct
    #    from case 4: there the bytes changed under a real signature, here the
    #    signature verifies against a key this installer does not trust. Both
    #    must abort - a release host that could swap in its own key would make
    #    the whole anchor decorative.
    Publish-Sums $sums
    $strangerDir = Join-Path $root 'stranger'
    New-Item -ItemType Directory -Force -Path $strangerDir | Out-Null
    $strangerFile = Join-Path $strangerDir 'signer'
    ssh-keygen -t ed25519 -N '' -C 'not the release key' -f $strangerFile -q
    if ($LASTEXITCODE -ne 0) { throw "ssh-keygen could not generate the stranger key (exit $LASTEXITCODE)" }
    $trusted = $signingKey
    $signingKey = ((Get-Content "$strangerFile.pub" -Raw).Trim() -split '\s+')[0..1] -join ' '
    try {
        Assert-Throws { Get-SignedHash $assetBase $zipName (New-WorkDir 'stranger') } `
            'does not verify' 'a valid signature from an untrusted key aborts'
    } finally {
        $signingKey = $trusted
    }

    # 8. No ssh-keygen on this machine. A fact about the machine, never a
    #    choice the release host can make, so it falls back with a warning
    #    rather than aborting. Get-Command is shadowed by a function in this
    #    scope, which Get-SignedHash resolves ahead of the cmdlet.
    Publish-Sums $sums
    function Get-Command { param([Parameter(ValueFromRemainingArguments)]$Rest) return $null }
    try {
        $noKeygen = Get-SignedHash $assetBase $zipName (New-WorkDir 'no-keygen')
    } finally {
        Remove-Item function:Get-Command
    }
    Assert ($null -eq $noKeygen) 'a machine without ssh-keygen falls back to the sidecar'

    Write-Host ''
    Write-Host 'install.ps1 anchor: 8 checks passed'
} finally {
    $listener.Stop()
    if ($server) { $server.Dispose() }
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}
