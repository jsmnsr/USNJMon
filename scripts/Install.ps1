#requires -Version 5.1
#requires -RunAsAdministrator
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [Parameter(Mandatory)] [string] $BinaryDirectory,
    [Parameter(Mandatory)] [string] $ConfigPath,
    [ValidateRange(64, 4095)] [int] $MaximumSizeMB = 256
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($PSEdition -ne 'Desktop') { throw 'Run this script in Windows PowerShell 5.1 (powershell.exe).' }
if (-not [Environment]::Is64BitProcess) { throw 'Use 64-bit Windows PowerShell.' }

$install = Join-Path $env:ProgramFiles 'UsnEventLog'
$data = Join-Path $env:ProgramData 'UsnEventLog'
$state = Join-Path $data 'state'
$exe = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usn-eventlog.exe'
$dll = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usn-eventlog-messages.dll'
$config = (Resolve-Path -LiteralPath $ConfigPath).Path
if (-not (Test-Path -LiteralPath $exe -PathType Leaf) -or -not (Test-Path -LiteralPath $dll -PathType Leaf)) {
    throw 'Both release artifacts must exist in BinaryDirectory.'
}
& $exe check-config $config
if ($LASTEXITCODE -ne 0) { throw 'Configuration validation failed' }
& $exe verify-resources $dll
if ($LASTEXITCODE -ne 0) { throw 'Message resource validation failed' }
if ((Get-Service -Name UsnEventLog -ErrorAction SilentlyContinue) -or
    [Diagnostics.EventLog]::SourceExists('UsnJournalBridge') -or
    [Diagnostics.EventLog]::Exists('UsnJournal') -or
    (Test-Path -LiteralPath $install) -or (Test-Path -LiteralPath $data)) {
    throw 'Fresh installation only: service, source, log and application directories must not already exist. See README for upgrades and retained uninstall data.'
}

function New-PrivateDirectory([string] $Path, [bool] $UsersRead) {
    $security = New-Object Security.AccessControl.DirectorySecurity
    $security.SetAccessRuleProtection($true, $false)
    $security.SetOwner((New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-544'))
    foreach ($sid in @('S-1-5-18', 'S-1-5-32-544')) {
        $rule = New-Object Security.AccessControl.FileSystemAccessRule(
            (New-Object Security.Principal.SecurityIdentifier $sid), 'FullControl',
            'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $security.AddAccessRule($rule)
    }
    if ($UsersRead) {
        $rule = New-Object Security.AccessControl.FileSystemAccessRule(
            (New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-545'), 'ReadAndExecute',
            'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $security.AddAccessRule($rule)
    }
    # Apply the ACL at creation, before any privileged data or executable is written.
    [IO.Directory]::CreateDirectory($Path, $security) | Out-Null
}

if ($PSCmdlet.ShouldProcess('UsnEventLog', 'Create private directories, register custom log, install stopped LocalSystem service')) {
    try {
        New-PrivateDirectory $install $true
        New-PrivateDirectory $data $false
        New-PrivateDirectory $state $false
        Copy-Item -LiteralPath $exe -Destination (Join-Path $install 'usn-eventlog.exe')
        Copy-Item -LiteralPath $dll -Destination (Join-Path $install 'usn-eventlog-messages.dll')
        Copy-Item -LiteralPath $config -Destination (Join-Path $data 'config.json')
        $installedDll = Join-Path $install 'usn-eventlog-messages.dll'
        New-EventLog -LogName UsnJournal -Source UsnJournalBridge -MessageResourceFile $installedDll
        $source = 'HKLM:\SYSTEM\CurrentControlSet\Services\EventLog\UsnJournal\UsnJournalBridge'
        New-ItemProperty -LiteralPath $source -Name EventMessageFile -Value $installedDll -PropertyType String -Force | Out-Null
        $log = 'HKLM:\SYSTEM\CurrentControlSet\Services\EventLog\UsnJournal'
        New-ItemProperty -LiteralPath $log -Name CustomSD -PropertyType String -Value 'O:BAG:SYD:(A;;0x7;;;SY)(A;;0x7;;;BA)(A;;0x1;;;S-1-5-32-573)' -Force | Out-Null
        Limit-EventLog -LogName UsnJournal -MaximumSize ($MaximumSizeMB * 1MB) -OverflowAction OverwriteAsNeeded
        $command = '"{0}" service "{1}" "{2}"' -f (Join-Path $install 'usn-eventlog.exe'), (Join-Path $data 'config.json'), $state
        New-Service -Name UsnEventLog -DisplayName 'NTFS USN Event Log Bridge' -BinaryPathName $command `
            -StartupType Manual -DependsOn EventLog -Description 'Read-only retained NTFS USN journal collection with durable checkpoints.' | Out-Null
        Write-Output 'Installed but NOT started. Review log retention and configuration before Start-Service UsnEventLog.'
        Write-Output 'After acceptance testing, optionally use: Set-Service UsnEventLog -StartupType Automatic'
    } catch {
        Write-Error "Installation failed; partial application-owned artifacts may remain. No service was started. Inspect the exact UsnEventLog paths and UsnJournal log before retrying. $($_.Exception.Message)"
        throw
    }
}
