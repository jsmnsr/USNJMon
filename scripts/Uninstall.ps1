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
        if ($controller.Status -ne 'Stopped') {
            Stop-Service UsnEventLog
            $controller.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))
        }
        & sc.exe delete UsnEventLog
        if ($LASTEXITCODE -ne 0) { throw 'SCM service deletion failed' }
    }
}
if ($RemoveEventLog -and $PSCmdlet.ShouldProcess('UsnJournal log and UsnJournalBridge source', 'PERMANENTLY DELETE collected events and unregister their source')) {
    if (Get-Service UsnEventLog -ErrorAction SilentlyContinue) {
        throw 'Service is still registered (possibly marked for deletion); close Services consoles and retry.'
    }
    if ([Diagnostics.EventLog]::Exists('UsnJournal')) { Remove-EventLog -LogName UsnJournal }
}
Write-Output 'Application files, configuration and checkpoints are preserved. By default the log/source/message DLL are retained so old events still render.'
Write-Output 'No journal, other log, or other service was changed. See README for explicit archival/removal and reinstall guidance.'
