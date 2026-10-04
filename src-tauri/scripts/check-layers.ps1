# Layering gate (P1 执行与完成门槛).
#
# Rules (01 section 2 / plan index section 9):
#   commands must not touch storage::, rusqlite or Connection
#   domain   must not touch rusqlite, std::fs, platform::, storage::, commands::
#            or services::
#   storage  must not touch platform::, commands:: or services::
#   services must not touch std::time/SystemTime/Instant::now, nor commands::
#
# The services -> commands edge was added in P4 Task 2: the edge is the reverse of
# the `commands -> services` direction. It slipped through once because nothing
# checked it -- `WriteEnvelope` used to live in `commands::` and a service imported
# it. The type now lives at the crate root; this rule keeps it that way.
#
# P4 Task 6 added commands:: and services:: to the domain rule. Both are reverse
# edges for the same reason: domain sits at the bottom of
# `commands -> services -> {storage, domain, platform}`, so a `use crate::services::..`
# or `use crate::commands::..` inside src/domain is an upward dependency -- and
# nothing checked it until now.
#
# P4 final fix wave added services:: to the storage rule (M6): storage is a sibling
# of domain under services, so `use crate::services::..` inside src/storage is an
# upward edge exactly like services -> commands. It was the one reverse edge left
# unguarded when the domain rule learned both words.
#
# P7 Task 0 fix round 1 added the platform rule (I4): platform sits at the bottom
# with domain, so `crate::services::` / `crate::storage::` / `crate::commands::`
# inside src/platform are all upward edges. Nothing checked them before, and the
# very next task (P7 Task 4, the tray) is the one that will want to "reuse the same
# commands" -- i.e. the exact place where platform -> services would appear.
# Doc links inside `//!` comments name services::bootstrap on purpose and are
# stripped like every other comment line.
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

function Test-EntryPointLeak {
    param(
        [string[]]$Files,
        [string]$Pattern,
        [string]$Label
    )

    $lines = @()
    foreach ($f in $Files) {
        # main.rs is not mirrored into the workspace, so a missing file is fine.
        if (-not (Test-Path $f)) { continue }
        if (Get-Command rg -ErrorAction SilentlyContinue) {
            $raw = & rg -n --no-heading $Pattern $f
            if ($LASTEXITCODE -gt 1) { throw "search failed: rg exit $LASTEXITCODE" }
            $lines += $raw
        } else {
            $lines += Get-ChildItem -File $f |
                Select-String -Pattern $Pattern |
                ForEach-Object { "$($_.Path):$($_.LineNumber):$($_.Line)" }
        }
    }

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
$ok = (Test-LayerLeak 'src/commands' 'storage::|rusqlite|\bConnection\b' 'commands') -and $ok
$ok = (Test-LayerLeak 'src/domain'  'rusqlite|std::fs|platform::|storage::|commands::|services::' 'domain')  -and $ok
$ok = (Test-LayerLeak 'src/storage' 'platform::|commands::|services::'  'storage') -and $ok

# services must not read the system clock directly -- time has to come through
# platform::clock::Clock so FakeClock can drive it. Without this check a single
# `Instant::now()` in a service silently makes the whole unit-testable seam leak.
#
# services must not touch commands:: either: that edge reverses the
# `commands -> services` direction (P4 Task 2; see the header comment).
# Both rules share this one check so the three output lines keep their shape.
$ok = (Test-LayerLeak 'src/services' 'std::time|SystemTime|Instant::now|commands::' 'services') -and $ok

# platform must not reach up into any sibling layer (P7 Task 0, review I4).
$ok = (Test-LayerLeak 'src/platform' 'crate::services::|crate::storage::|crate::commands::' 'platform') -and $ok

# One startup entry, machine-checked: lib.rs / main.rs wire the app, they do not
# open the database, migrate it or build an application_run themselves. That order
# lives in services/bootstrap.rs only (P7 Task 0 requirement 1; the next task is
# exactly the one that rewrites lib.rs, so the rule has to exist before it does).
$ok = (Test-EntryPointLeak @('src/lib.rs', 'src/main.rs') 'Db::open|migrate\(|run_repo::' 'entry points') -and $ok

if (-not $ok) {
    Write-Host 'LAYER CHECK FAILED'
    exit 1
}
Write-Host 'LAYER CHECK PASSED'
