#requires -Version 5.1
#requires -RunAsAdministrator
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [Parameter(Mandatory)] [string] $BinaryDirectory,
    [string] $ConfigPath,
    [switch] $Upgrade,
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
$dll = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usn-eventlog-provider.dll'
$manifest = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usn-eventlog.man'
$legacyDll = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usn-eventlog-messages.dll'
foreach ($artifact in @($exe, $dll, $manifest, $legacyDll)) {
    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) { throw "Missing artifact: $artifact" }
}
if ($Upgrade) {
    if ($ConfigPath) { throw '-Upgrade preserves installed configuration; do not pass -ConfigPath.' }
    $config = Join-Path $data 'config.json'
} else {
    if (-not $ConfigPath) { throw '-ConfigPath is required for a fresh installation.' }
    $config = (Resolve-Path -LiteralPath $ConfigPath).Path
}
& $exe check-config $config
if ($LASTEXITCODE -ne 0) { throw 'Configuration validation failed' }
& $exe verify-resources $dll
if ($LASTEXITCODE -ne 0) { throw 'Message resource validation failed' }
$command = '"{0}" service "{1}" "{2}"' -f (Join-Path $install 'usn-eventlog.exe'), (Join-Path $data 'config.json'), $state
$service = Get-CimInstance Win32_Service -Filter "Name='UsnEventLog'"
$channelKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WINEVT\Channels\UsnEventLog/Operational'
$previousChannel = $null
if (Test-Path -LiteralPath $channelKey) {
    $channel = Get-WinEvent -ListLog 'UsnEventLog/Operational'
    try {
        $previousChannel = [pscustomobject]@{
            IsEnabled = $channel.IsEnabled
            LogMode = $channel.LogMode
            SecurityDescriptor = $channel.SecurityDescriptor
            MaximumSizeInBytes = $channel.MaximumSizeInBytes
        }
    } finally { $channel.Dispose() }
}
if ($Upgrade) {
    if (-not $service -or $service.PathName -ne $command -or $service.State -ne 'Stopped') {
        throw 'Upgrade requires this installation''s service to exist and be stopped.'
    }
    foreach ($path in @($install, $data, $state, $config)) {
        $item = Get-Item -LiteralPath $path
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing reparse-point installation path: $path" }
    }
    if (Get-CimInstance Win32_Process -Filter "Name='usn-eventlog.exe'" |
        Where-Object { $_.ExecutablePath -eq (Join-Path $install 'usn-eventlog.exe') }) {
        throw 'Stop foreground collection before upgrading.'
    }
} elseif ($service -or (Test-Path -LiteralPath $install) -or (Test-Path -LiteralPath $data) -or
    (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WINEVT\Publishers\{4fbeac1d-5fe8-44c1-ab4e-a06a645bf2d8}') -or
    $null -ne $previousChannel) {
    throw 'Application installation already exists. Stop collection and use -Upgrade, or inspect retained uninstall files.'
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

if ($PSCmdlet.ShouldProcess('UsnEventLog', 'Install/update native provider and stopped service; preserve legacy history and checkpoints')) {
    $stateLock = $null
    try {
        if (-not $Upgrade) {
            New-PrivateDirectory $install $true
            New-PrivateDirectory $data $false
            New-PrivateDirectory $state $false
            Copy-Item -LiteralPath $config -Destination (Join-Path $data 'config.json')
        }
        $stateLock = [IO.File]::Open((Join-Path $state 'collector.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
        foreach ($name in @('usn-eventlog.exe', 'usn-eventlog-provider.dll', 'usn-eventlog.man')) {
            $destination = Join-Path $install $name
            if (Test-Path -LiteralPath $destination) {
                if ((Get-Item -LiteralPath $destination).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                    throw "Refusing reparse-point artifact: $destination"
                }
                $source = Join-Path $BinaryDirectory $name
                if ((Get-FileHash -LiteralPath $source).Hash -eq (Get-FileHash -LiteralPath $destination).Hash) {
                    continue
                }
                Copy-Item -LiteralPath $destination -Destination "$destination.previous" -Force
            }
            Copy-Item -LiteralPath (Join-Path $BinaryDirectory $name) -Destination $destination -Force
        }
        if (-not $Upgrade) {
            Copy-Item -LiteralPath $legacyDll -Destination (Join-Path $install 'usn-eventlog-messages.dll')
        }
        $installedDll = Join-Path $install 'usn-eventlog-provider.dll'
        & wevtutil.exe im (Join-Path $install 'usn-eventlog.man') "/rf:$installedDll" "/mf:$installedDll"
        if ($LASTEXITCODE -ne 0) { throw 'Native provider registration failed' }
        if ($null -ne $previousChannel) {
            $updatedChannel = Get-WinEvent -ListLog 'UsnEventLog/Operational'
            try {
                $updatedChannel.IsEnabled = $previousChannel.IsEnabled
                $updatedChannel.LogMode = $previousChannel.LogMode
                $updatedChannel.SecurityDescriptor = $previousChannel.SecurityDescriptor
                $updatedChannel.MaximumSizeInBytes = if ($PSBoundParameters.ContainsKey('MaximumSizeMB')) {
                    $MaximumSizeMB * 1MB
                } else { $previousChannel.MaximumSizeInBytes }
                $updatedChannel.SaveChanges()
            } finally { $updatedChannel.Dispose() }
        } else {
            & wevtutil.exe sl 'UsnEventLog/Operational' /e:true "/ms:$($MaximumSizeMB * 1MB)"
            if ($LASTEXITCODE -ne 0) { throw 'Native channel configuration failed' }
        }
        if (-not $Upgrade) {
            New-Service -Name UsnEventLog -DisplayName 'NTFS USN Event Log Bridge' -BinaryPathName $command `
                -StartupType Manual -DependsOn EventLog -Description 'Read-only retained NTFS USN journal collection with durable checkpoints.' | Out-Null
        }
        Write-Output 'Installed but NOT started. New events go to UsnEventLog/Operational. Existing UsnJournal events and checkpoints are untouched.'
        Write-Output 'After acceptance testing, optionally use: Set-Service UsnEventLog -StartupType Automatic'
    } catch {
        Write-Error "Installation failed; partial application-owned artifacts may remain. No service was started. Inspect UsnEventLog paths and .previous backups before retrying. $($_.Exception.Message)"
        throw
    } finally {
        if ($null -ne $stateLock) { $stateLock.Dispose() }
    }
}
