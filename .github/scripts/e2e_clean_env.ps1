# GitHub runners ship a machine-scope (HKLM) JAVA_HOME pointing at a
# preinstalled toolcache JDK. A clean user machine has none, so `jdk doctor`
# rightly flags the HKLM-vs-per-user conflict and exits non-zero. Clearing it
# is what lets a runner stand in for a normal machine; the runner is
# disposable and runs elevated.
#
# Every e2e job that reaches `jdk doctor` runs this first.
$ErrorActionPreference = 'Stop'
# `reg delete` exits 1 when the value is not there, which is a fine outcome
# here and not an error. PowerShell 7.4 turns a native non-zero exit into a
# terminating error on its own, so the preference is turned off rather than
# the exit code merely ignored.
$PSNativeCommandUseErrorActionPreference = $false

reg delete "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v JAVA_HOME /f 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Host 'cleared machine-scope JAVA_HOME'
} else {
    Write-Host 'no machine-scope JAVA_HOME to clear'
}
exit 0
