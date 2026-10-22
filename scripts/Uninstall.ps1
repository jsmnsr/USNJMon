#requires -Version 5.1
#requires -RunAsAdministrator
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param([switch] $RemoveEventLog)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($PSEdition -ne 'Desktop') { throw 'Run in Windows PowerShell 5.1 (powershell.exe).' }
$install = Join-Path $env:ProgramFiles 'UsnEventLog'
$service = Get-CimInstance Win32_Service -Filter "Name='UsnEventLog'"
if ($service) {
    $expected = '"{0}" service ' -f (Join-Path $install 'usn-eventlog.exe')
    if (-not $service.PathName.StartsWith($expected, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Service binary path is not this installation; refusing to modify it.'
    }
    if ($PSCmdlet.ShouldProcess('UsnEventLog service', 'Stop and delete service registration')) {
        $controller = Get-Service UsnEventLog
        try {
            if ($controller.Status -ne 'Stopped') {
                Stop-Service UsnEventLog
                $controller.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))
            }
            & sc.exe delete UsnEventLog
            if ($LASTEXITCODE -ne 0) { throw 'SCM service deletion failed' }
        } finally { $controller.Dispose() }
    }
}
if ($RemoveEventLog -and $PSCmdlet.ShouldProcess('UsnEventLog/Operational', 'Clear native channel and unregister native provider; preserve legacy UsnJournal')) {
    if (Get-Service UsnEventLog -ErrorAction SilentlyContinue) {
        throw 'Service is still registered (possibly marked for deletion); close Services consoles and retry.'
    }
    & wevtutil.exe cl 'UsnEventLog/Operational'
    if ($LASTEXITCODE -ne 0) { throw 'Native channel clear failed; provider registration retained' }
    & wevtutil.exe um (Join-Path $install 'usn-eventlog.man')
    if ($LASTEXITCODE -ne 0) { throw 'Native provider unregistration failed' }
}
Write-Output 'Application files, configuration and checkpoints are preserved. By default native channel/provider/resources are retained so events still render.'
Write-Output 'Legacy UsnJournal, UsnJournalBridge and their message DLL are never removed by this script.'
Write-Output 'No journal, other log, or other service was changed. See README for explicit archival/removal and reinstall guidance.'
