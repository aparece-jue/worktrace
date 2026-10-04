# Layering gate (P1 执行与完成门槛).
#
# Rules (01 section 2 / plan index section 9):
#   commands must not touch storage::, rusqlite or Connection
#   domain   must not touch rusqlite, std::fs, platform::, storage::, commands::
#            or services::
#   storage  must not touch platform::, commands:: or services::
#   services must not touch std::time/SystemTime/Instant::now, nor commands::
#
# Matching is CASE-SENSITIVE (rg is by default; Select-String is not, hence
# -CaseSensitive below): `AppError::Storage` is a variant name, not the `storage`
# layer. A case-insensitive bare-word rule reported it as a leak.
#
# Since the P7 Task 1 fix round all four match the bare module WORD, not the
# `module::` spelling: `use crate::{storage as db};` and `use crate::storage as _;`
# are the same upward edge but contain no `storage::` substring. See the rule site.
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
# P7 Task 1 fix round tightened that pattern to the bare module WORD (word
# boundaries): the `crate::`-prefixed form missed the synonymous spellings the same
# review named -- `use crate::{services::x};` and `use crate::services as _;` have no
# `crate::services::` substring at all. The other four rules already match the bare
# module name; this one now matches `services`/`storage`/`commands` wherever they
# appear as words, so every spelling is covered. Verified to have zero false
# positives in src/platform: every hit there sits in a comment (stripped), and
# identifiers such as `storage_path` are not matched because `_` is a word character.
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
            Select-String -Pattern $Pattern -CaseSensitive |
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

    # `main.rs` is not mirrored into the WSL workspace, so ONE missing file is expected
    # when this script runs against the mirror -- it gets a warning. ZERO is not
    # expected: the rule would then scan nothing and still print "clean", so a renamed
    # or moved entry point would silently stop being checked (a false clean). Fail loudly.
    $present = @($Files | Where-Object { Test-Path $_ })
    if ($present.Count -eq 0) {
        throw ("no entry point to scan (all missing: {0})" -f ($Files -join ', '))
    }
    if ($present.Count -lt $Files.Count) {
        $missing = @($Files | Where-Object { -not (Test-Path $_) })
        Write-Host ("  (${Label}: scanning {0}; missing {1} -- only the mirror lacks main.rs)" -f `
            ($present -join ', '), ($missing -join ', '))
    }

    $lines = @()
    foreach ($f in $present) {
        if (Get-Command rg -ErrorAction SilentlyContinue) {
            $raw = & rg -n --no-heading $Pattern $f
            if ($LASTEXITCODE -gt 1) { throw "search failed: rg exit $LASTEXITCODE" }
            $lines += $raw
        } else {
            $lines += Get-ChildItem -File $f |
                Select-String -Pattern $Pattern -CaseSensitive |
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
# P7 Task 1 fix round 1 (review I5): all four rules below used the `module::` form,
# which misses the synonymous import spellings -- `use crate::{storage as db};` and
# `use crate::storage as _;` contain no `storage::` substring at all. They now match
# the bare module WORD (word boundaries, so `storage_path` is not a hit), the same
# shape the platform rule already uses. Each of the four was reverse-verified with
# both spellings injected: LEAK + exit 1 (see the P7 Task 1 fix round report).
$ok = (Test-LayerLeak 'src/commands' '\b(storage|rusqlite|Connection)\b' 'commands') -and $ok
$ok = (Test-LayerLeak 'src/domain'  '\b(rusqlite|platform|storage|commands|services)\b|std::fs' 'domain')  -and $ok
$ok = (Test-LayerLeak 'src/storage' '\b(platform|commands|services)\b'  'storage') -and $ok

# services must not read the system clock directly -- time has to come through
# platform::clock::Clock so FakeClock can drive it. Without this check a single
# `Instant::now()` in a service silently makes the whole unit-testable seam leak.
#
# services must not touch commands:: either: that edge reverses the
# `commands -> services` direction (P4 Task 2; see the header comment).
# Both rules share this one check so the three output lines keep their shape.
$ok = (Test-LayerLeak 'src/services' 'std::time|SystemTime|Instant::now|\bcommands\b' 'services') -and $ok

# platform must not reach up into any sibling layer (P7 Task 0, review I4; the
# pattern was widened in the P7 Task 1 fix round -- see the header comment).
$ok = (Test-LayerLeak 'src/platform' '\b(services|storage|commands)\b' 'platform') -and $ok

# One startup entry, machine-checked: lib.rs / main.rs wire the app, they do not
# open the database, migrate it or build an application_run themselves. That order
# lives in services/bootstrap.rs only (P7 Task 0 requirement 1; the next task is
# exactly the one that rewrites lib.rs, so the rule has to exist before it does).
# Missing files: one is normal (the mirror has no main.rs), zero is an error.
$ok = (Test-EntryPointLeak @('src/lib.rs', 'src/main.rs') 'Db::open|migrate\(|run_repo::' 'entry points') -and $ok

if (-not $ok) {
    Write-Host 'LAYER CHECK FAILED'
    exit 1
}
Write-Host 'LAYER CHECK PASSED'
