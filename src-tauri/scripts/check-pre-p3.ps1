param(
    [string]$EvidenceDirectory = (Join-Path ([IO.Path]::GetTempPath()) ("worktrace-pre-p3-" + (Get-Date -Format 'yyyyMMdd-HHmmss')))
)

# 自动化门禁只证明当前核心基线；不把平台/双 WebView 实机验收标为通过。
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
New-Item -ItemType Directory -Path $EvidenceDirectory -Force | Out-Null
$evidenceRoot = (Resolve-Path -LiteralPath $EvidenceDirectory).Path
$results = [Collections.Generic.List[object]]::new()

. (Join-Path $PSScriptRoot 'invoke-gate.ps1')

Push-Location $repoRoot
try {
    $hostExe = (Get-Process -Id $PID).Path
    Invoke-Gate 'runner-regression' $hostExe @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'src-tauri/scripts/test-invoke-gate.ps1')
    Invoke-Gate 'rust-tests' 'cargo' @('test', '--offline', '--manifest-path', 'src-tauri/Cargo.toml', '-q')
    Invoke-Gate 'rust-clippy' 'cargo' @('clippy', '--offline', '--manifest-path', 'src-tauri/Cargo.toml', '--all-targets', '--', '-D', 'warnings')
    Invoke-Gate 'rust-format' 'cargo' @('fmt', '--manifest-path', 'src-tauri/Cargo.toml', '--check')
    # 既有分层脚本用 src/ 相对路径，必须在 Rust crate 目录执行。
    # 用**当前宿主解释器**跑子脚本：本机可能只有 Windows PowerShell 5.1（没有 pwsh），
    # 硬编码 'pwsh' 会在那种机器上假红（CommandNotFoundException，分层规则其实是过的）。
    $hostExe = (Get-Process -Id $PID).Path
    Push-Location (Join-Path $repoRoot 'src-tauri')
    try {
        Invoke-Gate 'layers' $hostExe @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'scripts/check-layers.ps1')
    } finally {
        Pop-Location
    }
    Invoke-Gate 'frontend-tests' 'pnpm' @('test')
    Invoke-Gate 'frontend-build' 'pnpm' @('build')
    Invoke-Gate 'diff-check' 'git' @('-c', "safe.directory=$repoRoot", 'diff', '--check')
    $head = & git -c "safe.directory=$repoRoot" rev-parse HEAD
    $status = @(& git -c "safe.directory=$repoRoot" status --short)
    $failed = @($results | Where-Object { $_.exit_code -ne 0 })
    [pscustomobject]@{
        at = (Get-Date).ToString('o'); repository = $repoRoot; head = $head
        working_tree = $status; checks = $results.ToArray(); automated_passed = ($failed.Count -eq 0)
        manual_platform_verified = $false
        carryover_contract = 'docs/validation/pre-p3-closure.md'
    } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $evidenceRoot 'result.json') -Encoding utf8
    Write-Host "Evidence: $evidenceRoot"
    if ($failed.Count -gt 0) { exit 1 }
} finally {
    Pop-Location
}
