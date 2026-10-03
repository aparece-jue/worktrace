# Layering gate (P1 执行与完成门槛).
#
# Rules (01 section 2 / plan index section 9):
#   domain  must not touch rusqlite, std::fs, platform:: or storage::
#   storage must not touch platform:: or commands::
#
# Why a real script instead of an inline snippet in the plan:
#   A plain grep also matches the doc comments that *state* the rule, so the
#   check reported a "leak" on every module header. A gate that always fails
#   gets ignored, which is worse than having no gate. This one skips comment
#   lines before matching.
#
# ASCII-only on purpose: Windows PowerShell 5.1 reads a BOM-less UTF-8 script
# as ANSI, and non-ASCII bytes can break parsing.
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts/check-layers.ps1
#         (run from src-tauri/)

$ErrorActionPreference = 'Stop'

function Test-LayerLeak {
    param(
        [string]$Dir,
        [string]$Pattern,
        [string]$Label
    )

    if (-not (Test-Path $Dir)) { throw "scan dir missing: $Dir" }

    if (Get-Command rg -ErrorAction SilentlyContinue) {
        $raw = & rg -n --no-heading $Pattern $Dir
        if ($LASTEXITCODE -gt 1) { throw "search failed: rg exit $LASTEXITCODE" }
        $lines = $raw
    } else {
        Write-Host '  (rg absent, using Select-String)'
        $lines = Get-ChildItem -Recurse -File $Dir |
            Select-String -Pattern $Pattern |
            ForEach-Object { "$($_.Path):$($_.LineNumber):$($_.Line)" }
    }

    # Drop comment lines. `//` covers line and doc comments; a leading `*`
    # covers the body of a block comment.
    $code = $lines | Where-Object {
        $_ -notmatch ':\s*(//|\*|/\*)' -and $_ -notmatch '^\s*(//|\*)'
    }

    if ($code) {
        Write-Host "  LEAK in ${Label}:"
        $code | Select-Object -First 10 | ForEach-Object { Write-Host "    $_" }
        return $false
    }

    Write-Host "  $Label clean"
    return $true
}

$ok = $true
$ok = (Test-LayerLeak 'src/domain'  'rusqlite|std::fs|platform::|storage::' 'domain')  -and $ok
$ok = (Test-LayerLeak 'src/storage' 'platform::|commands::'                 'storage') -and $ok

if (-not $ok) {
    Write-Host 'LAYER CHECK FAILED'
    exit 1
}
Write-Host 'LAYER CHECK PASSED'
