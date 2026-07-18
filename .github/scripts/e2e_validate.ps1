# E2E validations after `jdk setup` + `jdk install temurin@21` on a real
# runner: the persisted registry JAVA_HOME, the junction, the shim,
# `jdk which` and `jdk doctor` must all agree.
param(
    [Parameter(Mandatory)] [string]$Jdk,
    [Parameter(Mandatory)] [ValidateSet('amd64', 'aarch64')] [string]$ExpectedArch
)
$ErrorActionPreference = 'Stop'

function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "FAIL: $Message" }
    Write-Host "ok: $Message"
}

# JAVA_HOME persisted in HKCU\Environment, pointing at the immutable junction.
$javaHome = (Get-ItemProperty HKCU:\Environment -Name JAVA_HOME).JAVA_HOME
Assert (-not [string]::IsNullOrWhiteSpace($javaHome)) 'registry JAVA_HOME is non-empty'
$junction = Join-Path $env:USERPROFILE '.jdk\current'
Assert ($javaHome -eq $junction) "registry JAVA_HOME is the junction ($javaHome)"
Assert (Test-Path (Join-Path $javaHome 'bin\java.exe')) 'junction\bin\java.exe exists'

# The shim runs the real JVM of the right architecture.
$shimJava = Join-Path $env:USERPROFILE '.jdk\shims\java.exe'
Assert (Test-Path $shimJava) 'shims\java.exe exists'
$settings = & $shimJava -XshowSettings:properties -version 2>&1 | Out-String
Assert ($LASTEXITCODE -eq 0) 'shim java -version exits 0'
Write-Host $settings
Assert ($settings -match 'java\.version = 21') 'shim java is a 21'
Assert ($settings -match "os\.arch = $ExpectedArch") "shim java arch is $ExpectedArch"

# `jdk which java` agrees with what the shim just ran.
$which = (& $Jdk which java | Out-String).Trim()
Assert ($LASTEXITCODE -eq 0) 'jdk which java exits 0'
Assert (Test-Path $which) "jdk which java points at an existing file ($which)"
Assert ($which.StartsWith((Join-Path $env:USERPROFILE '.jdk'), [StringComparison]::OrdinalIgnoreCase)) 'which path lives in the store'
& $which -version 2>&1 | Out-Null
Assert ($LASTEXITCODE -eq 0) 'the which path runs'

# doctor must find nothing wrong - including the published index being
# reachable for real.
& $Jdk doctor
Assert ($LASTEXITCODE -eq 0) 'jdk doctor exits 0'
