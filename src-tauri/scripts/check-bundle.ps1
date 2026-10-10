# Release bundle gate (R-04, plan Task 4). Run AFTER `pnpm build`.
#
# Proves two things before a release:
#   1. the release front-end bundle (repo-root dist/, produced by `pnpm build`)
#      carries no trace of the DockviewDemo demo asset nor of the dockview
#      dependency that demo pulls in;
#   2. the Rust crate still compiles in the release profile
#      (`cargo check --offline --lib --release`). Merged into this call chain on
#      purpose (Ruling P8-5, from docs/validation/p7-acceptance.md 6.4 item 16):
#      release-only breakage - #[cfg(debug_assertions)] guards, panic = "abort",
#      warnings that exist only in release - is exactly what a *release* gate is
#      for. The P8-5 estimate was "about 93 s" (a cold run); measured on this
#      machine it is 0.5-21 s once the dependencies are built. Either way it is
#      paid only before a release.
#
# Judgement rules that are easy to get wrong:
#   * A MISSING (or empty) dist/ is a FAILURE, exit 1. "Nothing to check" must
#     never come out as "nothing wrong".
#   * dist/ is resolved from the REPOSITORY ROOT (this script lives in
#     src-tauri/scripts/, so the root is two levels up), never from the current
#     directory: the bundle always lands in the repo root, not under src-tauri/,
#     and the gate may be invoked from any cwd.
#   * package.json is deliberately NOT inspected. `dockview` staying in
#     dependencies is the normal state while the layout is fixed; "the
#     dependency exists" says nothing about "the code got bundled". Only the
#     built artifacts can answer that question.
#   * This script does not build: it judges what is on disk, so run
#     `pnpm build` first. The package.json forwarder (`pnpm check:bundle`)
#     chains the build in front of this script on purpose: a failed build must
#     stop the chain instead of leaving a stale dist/ to be green-lit here.
#
# Extra context is printed (file count, newest write time) so a stale bundle is
# visible in the evidence, but staleness is NOT judged here: mtime comparisons
# across a checkout are not a reliable gate.
#
# ASCII-only on purpose: Windows PowerShell 5.1 reads a BOM-less UTF-8 script as
# ANSI, so any non-ASCII byte here would be mojibake.
$ErrorActionPreference = 'Stop'
trap { Write-Host ("FAIL: unexpected error: {0}" -f $_.Exception.Message); exit 1 }

# Every marker must be able to turn the gate red:
#   DockviewDemo        the demo component's own identifier;
#   dock-workspace / dock-toolbar / dock-panel
#                       the demo stylesheet's rules (a CSS-only leak counts);
#   dockview            dockview / dockview-react runtime and CSS - class names
#                       such as dv-dockview survive minification, and CSS class
#                       names are never renamed.
$markers = @('DockviewDemo', 'dock-workspace', 'dock-toolbar', 'dock-panel', 'dockview')
$snippetWidth = 64

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$distDir = Join-Path $repoRoot 'dist'

Write-Host '=== release bundle gate (R-04) ==='
Write-Host ("repo root : {0}" -f $repoRoot)
Write-Host ("bundle dir: {0}" -f $distDir)

if (-not (Test-Path -LiteralPath $distDir -PathType Container)) {
    Write-Host ("FAIL: bundle directory not found: {0}" -f $distDir)
    Write-Host 'FAIL: run pnpm build first. A missing dist/ is an error, not a pass.'
    exit 1
}

$bundleFiles = @(Get-ChildItem -LiteralPath $distDir -Recurse -File -Force)
if ($bundleFiles.Count -eq 0) {
    Write-Host ("FAIL: bundle directory is empty: {0}" -f $distDir)
    Write-Host 'FAIL: run pnpm build again. An empty dist/ is an error, not a pass.'
    exit 1
}
# Second sanity floor: dist/ that exists but holds no entry point is not a
# production bundle either, and scanning it would be another way to report
# "nothing wrong" while nothing was actually checked.
$entry = Join-Path $distDir 'index.html'
if (-not (Test-Path -LiteralPath $entry -PathType Leaf)) {
    Write-Host ("FAIL: bundle entry point not found: {0}" -f $entry)
    Write-Host 'FAIL: dist/ exists but is not a Vite production bundle; run pnpm build.'
    exit 1
}
$newest = ($bundleFiles | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1).LastWriteTimeUtc
Write-Host ("bundle    : {0} file(s), newest write {1:yyyy-MM-dd HH:mm:ss} UTC" -f $bundleFiles.Count, $newest)

Write-Host ''
Write-Host '=== 1. scan the bundle for DockviewDemo / dockview markers ==='
$hits = @()
foreach ($file in $bundleFiles) {
    $text = [IO.File]::ReadAllText($file.FullName, [Text.Encoding]::UTF8)
    # String.Replace, never Substring(Length): $repoRoot doubles as a filesystem
    # path and as a string prefix, and the two disagree under a UNC root ->
    # Substring throws (startIndex). Replace is pure string work, and if it ever
    # misses, the label degrades to an absolute path instead of failing the gate.
    $rel = $file.FullName.Replace($distDir, 'dist')
    foreach ($marker in $markers) {
        $found = [regex]::Matches($text, [regex]::Escape($marker), [Text.RegularExpressions.RegexOptions]::IgnoreCase)
        if ($found.Count -eq 0) { continue }
        $m = $found[0]
        $line = 1 + ([regex]::Matches($text.Substring(0, $m.Index), "`n")).Count
        $start = [Math]::Max(0, $m.Index - [int]($snippetWidth / 2))
        $length = [Math]::Min($snippetWidth, $text.Length - $start)
        # Non-printable bytes are flattened: the snippet goes to a console whose
        # code page is not guaranteed, and a bundle is not all text.
        $snippet = ($text.Substring($start, $length) -replace '[^\x20-\x7E]', '.')
        $hits += [pscustomobject]@{
            Marker = $marker; File = $rel; Line = $line; Count = $found.Count; Snippet = $snippet
        }
    }
}

if ($hits.Count -gt 0) {
    Write-Host ("FAIL: {0} marker hit group(s) in the release bundle" -f $hits.Count)
    foreach ($hit in $hits) {
        Write-Host ("  HIT marker='{0}' file={1}:{2} occurrences={3}" -f $hit.Marker, $hit.File, $hit.Line, $hit.Count)
        Write-Host ("      ...{0}..." -f $hit.Snippet)
    }
    Write-Host 'FAIL: DockviewDemo / dockview must not be part of the release bundle (R-04).'
    exit 1
}
Write-Host ("PASS: none of the {0} markers appears in {1} bundle file(s)" -f $markers.Count, $bundleFiles.Count)

Write-Host ''
Write-Host '=== 2. release-profile compile probe (cargo check --lib --release) ==='
$manifest = Join-Path $repoRoot 'src-tauri\Cargo.toml'
if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
    Write-Host ("FAIL: manifest not found: {0}" -f $manifest)
    exit 1
}
# Absolute --manifest-path: the probe must not depend on the caller's cwd
# either. Cargo still puts the artifacts in src-tauri/target.
$cargoArgs = @('check', '--offline', '--lib', '--release', '--manifest-path', $manifest)
Write-Host ("cargo {0}" -f ($cargoArgs -join ' '))
$started = Get-Date
& cargo @cargoArgs
$cargoExit = $LASTEXITCODE
$elapsed = [int]((Get-Date) - $started).TotalSeconds
if ($cargoExit -ne 0) {
    Write-Host ("FAIL: cargo check --offline --lib --release exited {0} after {1}s" -f $cargoExit, $elapsed)
    exit 1
}
Write-Host ("PASS: cargo check --offline --lib --release exited 0 after {0}s" -f $elapsed)

Write-Host ''
Write-Host 'PASS: release bundle gate - no DockviewDemo/dockview marker in dist/ and the release-profile check is green'
exit 0
