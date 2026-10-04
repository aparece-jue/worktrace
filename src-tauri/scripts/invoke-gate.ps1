# Shared runner: check-pre-p3.ps1 and its regression probe use this same function.
# The caller owns $evidenceRoot and $results; this function records one result.
function Invoke-Gate {
    param([string]$Name, [string]$Command, [string[]]$Arguments)
    $logPath = Join-Path $evidenceRoot "$Name.log"
    $code = 1
    $failure = $null
    $previousPreference = $ErrorActionPreference
    # Native stderr is not a failure by itself (including Windows PowerShell 5.1).
    $PSNativeCommandUseErrorActionPreference = $false
    # Native executables update the global automatic variable. A local shadow
    # would hide their new value and make even successful checks look missing.
    $global:LASTEXITCODE = $null
    try {
        $resolved = Get-Command -Name $Command -CommandType Application, ExternalScript -ErrorAction Stop |
            Select-Object -First 1
        $ErrorActionPreference = 'Continue'
        & $resolved @Arguments 2>&1 | Tee-Object -FilePath $logPath -ErrorAction Stop
        if ($null -eq $global:LASTEXITCODE) {
            throw 'The check did not report a native exit code.'
        }
        $code = $global:LASTEXITCODE
    } catch {
        # A launch, pipeline or evidence-write exception is always a failure.
        # Never reuse an exit code from an earlier check, even if it was zero.
        $code = 1
        $failure = $_.Exception.Message
        try {
            $_ | Out-String | Add-Content -LiteralPath $logPath -ErrorAction Stop
        } catch {
            Write-Warning "Unable to write check evidence: $logPath"
        }
    } finally {
        $ErrorActionPreference = $previousPreference
    }
    $results.Add([pscustomobject]@{
        name = $Name; exit_code = $code; log = $logPath; failure_reason = $failure
    })
}
