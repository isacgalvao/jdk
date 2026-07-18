# Maven must pick up the registry-persisted JAVA_HOME and report the JDK
# behind the junction - without a persisted JAVA_HOME, Maven ignores the
# version manager entirely.
$ErrorActionPreference = 'Stop'

$mavenVersion = '3.9.9'
$url = "https://archive.apache.org/dist/maven/maven-3/$mavenVersion/binaries/apache-maven-$mavenVersion-bin.zip"
$zip = Join-Path $env:RUNNER_TEMP 'maven.zip'

Invoke-WebRequest -Uri $url -OutFile $zip
$expected = ([string](Invoke-RestMethod -Uri "$url.sha512")).Trim().Split()[0].ToLowerInvariant()
$actual = (Get-FileHash $zip -Algorithm SHA512).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw "maven zip sha512 mismatch: expected $expected got $actual" }

Expand-Archive $zip -DestinationPath $env:RUNNER_TEMP -Force
$mvn = Join-Path $env:RUNNER_TEMP "apache-maven-$mavenVersion\bin\mvn.cmd"

# The JAVA_HOME of a fresh console: what setup persisted in the registry,
# not whatever this workflow process inherited.
$env:JAVA_HOME = (Get-ItemProperty HKCU:\Environment -Name JAVA_HOME).JAVA_HOME
Write-Host "JAVA_HOME from registry: $env:JAVA_HOME"

$out = & $mvn -version 2>&1 | Out-String
Write-Host $out
if ($LASTEXITCODE -ne 0) { throw "mvn -version exited $LASTEXITCODE" }
if ($out -notmatch 'Java version: 21') { throw 'maven did not report the installed Java 21' }
if ($out -notmatch '\.jdk') { throw 'maven runtime does not live under the jdk store' }
Write-Host 'ok: maven runs on the junction JDK via the registry JAVA_HOME'
