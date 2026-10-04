# Run with Windows PowerShell 5.1 or PowerShell 7. No test framework required.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'invoke-gate.ps1')
$evidenceRoot = Join-Path ([IO.Path]::GetTempPath()) ('worktrace-gate-probe-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $evidenceRoot | Out-Null
$results = [Collections.Generic.List[object]]::new()

Invoke-Gate 'success' 'cmd.exe' @('/c', 'exit /b 0')
Invoke-Gate 'missing-after-success' 'worktrace_nonexistent_gate_command_123' @()
Invoke-Gate 'native-failure' 'cmd.exe' @('/c', 'exit /b 7')
Invoke-Gate 'missing-after-failure' 'worktrace_nonexistent_gate_command_123' @()
Invoke-Gate 'stderr-success' 'cmd.exe' @('/c', 'echo stderr-probe 1>&2 & exit /b 0')
New-Item -ItemType Directory -Path (Join-Path $evidenceRoot 'unwritable-log.log') | Out-Null
Invoke-Gate 'unwritable-log' 'cmd.exe' @('/c', 'echo evidence-probe & exit /b 0')

$expected = @(0, 1, 7, 1, 0, 1)
for ($i = 0; $i -lt $expected.Count; $i++) {
    if ($results[$i].exit_code -ne $expected[$i]) {
        throw "Incorrect exit code for $($results[$i].name): $($results[$i].exit_code)"
    }
}
foreach ($i in @(1, 3, 5)) {
    if ([string]::IsNullOrWhiteSpace($results[$i].failure_reason)) {
        throw "Missing failure reason for $($results[$i].name)"
    }
}
if (@($results | Where-Object { $_.exit_code -ne 0 }).Count -ne 4) {
    throw 'The aggregate gate must reject all four failing cases.'
}
if ((Get-Content -LiteralPath (Join-Path $evidenceRoot 'stderr-success.log') -Raw) -notmatch 'stderr-probe') {
    throw 'Successful stderr output must remain in the evidence log.'
}
Write-Host 'GATE RUNNER PROBE PASSED (6 cases)'
