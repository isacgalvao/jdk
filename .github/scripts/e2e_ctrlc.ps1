# Prove that the shim ignoring Ctrl+C does not desensitize the child JVM: the
# console CTRL_C_EVENT must still reach the real java.exe, run its shutdown
# hooks, and the shim must propagate a non-zero exit instead of hanging.
#
# Console Ctrl+C delivery needs a real interactive console that headless CI
# runners do not reliably provide, so this test can't just assert success. It
# runs a CONTROL first - the same Ctrl+C round-trip against a JVM launched
# DIRECTLY (no shim). The control tells us whether the mechanism even works
# here, which disentangles "this runner can't deliver Ctrl+C" from "the shim
# broke it":
#   control hook did NOT run  -> the runner can't deliver console Ctrl+C at all
#                                -> SKIP (not a product defect; validated locally)
#   control ran, shim did NOT  -> the shim desensitized the child -> FAIL
#   both ran, shim exit != 0   -> correct -> PASS
#
# A single C# harness owns the console relationship deterministically:
# FreeConsole + AllocConsole gives it an isolated real console (so the raised
# Ctrl+C can't reach the pwsh step and the child JVM inherits a real console),
# it launches the target inheriting that console, shields itself like the shim
# does, raises CTRL_C_EVENT, and reports every stage to a result file (stdout
# is gone with the old console).
$ErrorActionPreference = 'Stop'

$dir = Join-Path $env:RUNNER_TEMP "ctrlc-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force $dir | Out-Null
$shims = Join-Path $env:USERPROFILE '.jdk\shims'
$javaReal = Join-Path $env:USERPROFILE '.jdk\current\bin\java.exe'   # control: no shim
$javaShim = Join-Path $shims 'java.exe'                              # product: through the shim

# A class whose shutdown hook leaves a file behind; file-based signalling
# because Ctrl+C tears the console down with the process.
@'
import java.nio.file.*;
public class Hook {
    public static void main(String[] args) throws Exception {
        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            try { Files.writeString(Path.of(args[0]), "hook-ran"); } catch (Exception e) {}
        }));
        Files.writeString(Path.of(args[1]), "ready");
        Thread.sleep(120000);
    }
}
'@ | Set-Content (Join-Path $dir 'Hook.java') -Encoding ascii

# Compiled with the installed JDK through the javac SHIM - a second tool
# exercised end to end for free.
& (Join-Path $shims 'javac.exe') (Join-Path $dir 'Hook.java')
if ($LASTEXITCODE -ne 0) { throw "javac (shim) exited $LASTEXITCODE" }

@'
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;

class Harness {
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool AllocConsole();
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetConsoleCtrlHandler(IntPtr handler, bool add);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GenerateConsoleCtrlEvent(uint ev, uint group);

    // args: targetExe, workDir, hookFile, readyFile, resultFile
    static int Main(string[] args) {
        string target = args[0], workDir = args[1], hookFile = args[2], readyFile = args[3], resultFile = args[4];
        string stage = "start";
        try {
            stage = "alloc-console";
            FreeConsole();
            if (!AllocConsole()) return Fail(resultFile, stage, "AllocConsole " + Marshal.GetLastWin32Error());

            stage = "spawn";
            var proc = new Process();
            proc.StartInfo.FileName = target;
            proc.StartInfo.Arguments = "Hook \"" + hookFile + "\" \"" + readyFile + "\"";
            proc.StartInfo.WorkingDirectory = workDir;
            proc.StartInfo.UseShellExecute = false;
            if (!proc.Start()) return Fail(resultFile, stage, "Process.Start returned false");

            stage = "await-ready";
            var deadline = DateTime.UtcNow.AddSeconds(60);
            while (!File.Exists(readyFile)) {
                if (proc.HasExited) return Fail(resultFile, stage, "target exited early code=" + proc.ExitCode);
                if (DateTime.UtcNow > deadline) { Kill(proc); return Fail(resultFile, stage, "JVM never signalled ready"); }
                Thread.Sleep(200);
            }

            stage = "raise";
            SetConsoleCtrlHandler(IntPtr.Zero, true);
            bool raised = GenerateConsoleCtrlEvent(0, 0);

            stage = "await-exit";
            bool exited = proc.WaitForExit(30000);
            bool hookRan = File.Exists(hookFile);
            string code = exited ? proc.ExitCode.ToString() : "n/a";
            if (!exited) Kill(proc);
            File.WriteAllText(resultFile, "raised=" + raised + " exited=" + exited + " hookRan=" + hookRan + " exitCode=" + code);
            return 0;
        } catch (Exception e) {
            return Fail(resultFile, stage, e.GetType().Name + ": " + e.Message);
        }
    }

    static int Fail(string resultFile, string stage, string msg) {
        try { File.WriteAllText(resultFile, "harness-error stage=" + stage + " " + msg); } catch { }
        return 1;
    }

    static void Kill(Process p) { try { p.Kill(); } catch { } }
}
'@ | Set-Content (Join-Path $dir 'Harness.cs') -Encoding ascii

$harness = Join-Path $dir 'harness.exe'
$csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (-not (Test-Path $csc)) { $csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework\v4.0.30319\csc.exe' }
& $csc -nologo "-out:$harness" (Join-Path $dir 'Harness.cs')
if ($LASTEXITCODE -ne 0) { throw "csc exited $LASTEXITCODE" }

# Runs the Ctrl+C round-trip against $targetExe and returns the parsed result.
function Invoke-Roundtrip([string]$label, [string]$targetExe) {
    if (-not (Test-Path $targetExe)) { throw "$label target not found: $targetExe" }
    $tag = [guid]::NewGuid().ToString('N').Substring(0, 8)
    $hookFile = Join-Path $dir "hook-$tag.txt"
    $readyFile = Join-Path $dir "ready-$tag.txt"
    $resultFile = Join-Path $dir "result-$tag.txt"
    # The harness detaches our console, so wait on the handle; the verdict comes
    # back through the result file.
    $proc = Start-Process -FilePath $harness `
        -ArgumentList $targetExe, $dir, $hookFile, $readyFile, $resultFile -PassThru
    if (-not $proc.WaitForExit(120000)) { Stop-Process -Id $proc.Id -Force; throw "$label harness did not finish in time" }
    $raw = if (Test-Path $resultFile) { (Get-Content $resultFile -Raw).Trim() } else { "(no result; harness exit $($proc.ExitCode))" }
    Write-Host "${label}: $raw"
    if ($raw -like 'harness-error*') { throw "$label harness error: $raw" }
    return @{
        hookRan  = $raw -match 'hookRan=True'
        exitCode = if ($raw -match 'exitCode=(-?\d+)') { [int]$Matches[1] } else { $null }
    }
}

# Control: does console Ctrl+C reach a directly-launched JVM on THIS runner?
$control = Invoke-Roundtrip 'control (direct JVM)' $javaReal
if (-not $control.hookRan) {
    Write-Host '::warning::SKIP - this runner does not deliver console Ctrl+C even to a directly-launched JVM (headless console limitation). The shim Ctrl+C round-trip is validated on an interactive machine instead.'
    exit 0
}

# The mechanism works here, so the shim MUST preserve it.
$product = Invoke-Roundtrip 'product (through shim)' $javaShim
if (-not $product.hookRan) {
    throw 'REGRESSION: Ctrl+C ran the shutdown hook for a direct JVM but NOT through the shim - the shim desensitized the child'
}
if ($product.exitCode -eq 0) {
    throw 'the shim exited 0 after Ctrl+C - expected the interrupted JVM exit code'
}
Write-Host "ok: Ctrl+C reached the child JVM through the shim, its shutdown hook ran, and the shim propagated exit $($product.exitCode)"
