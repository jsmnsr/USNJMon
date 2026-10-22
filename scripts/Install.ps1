#requires -Version 5.1
#requires -RunAsAdministrator
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [Parameter(Mandatory)] [string] $BinaryDirectory,
    [string] $ConfigPath,
    [switch] $Upgrade,
    [switch] $ProviderOnly,
    [ValidateRange(64, 4095)] [int] $MaximumSizeMB = 256
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($PSEdition -ne 'Desktop') { throw 'Run this script in Windows PowerShell 5.1 (powershell.exe).' }
if (-not [Environment]::Is64BitProcess) { throw 'Use 64-bit Windows PowerShell.' }

$install = Join-Path $env:ProgramFiles 'USNJMon'
$data = Join-Path $env:ProgramData 'USNJMon'
$state = Join-Path $data 'state'
$exe = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usnjmon.exe'
$dll = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usnjmon-provider.dll'
$manifest = Join-Path (Resolve-Path -LiteralPath $BinaryDirectory).Path 'usnjmon.man'
foreach ($artifact in @($exe, $dll, $manifest)) {
    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) { throw "Missing artifact: $artifact" }
}
if ($Upgrade) {
    if ($ConfigPath) { throw '-Upgrade preserves installed configuration; do not pass -ConfigPath.' }
    $config = Join-Path $data 'config.json'
} else {
    if (-not $ConfigPath) { $ConfigPath = Join-Path $PSScriptRoot '..\config.example.json' }
    $config = (Resolve-Path -LiteralPath $ConfigPath).Path
}
& $exe check-config $config
if ($LASTEXITCODE -ne 0) { throw 'Configuration validation failed' }
& $exe verify-resources $dll
if ($LASTEXITCODE -ne 0) { throw 'Message resource validation failed' }
$command = '"{0}" service "{1}" "{2}"' -f (Join-Path $install 'usnjmon.exe'), (Join-Path $data 'config.json'), $state
$service = Get-CimInstance Win32_Service -Filter "Name='USNJMon'"
$channelKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WINEVT\Channels\USNJMon/Operational'
$previousChannel = $null
if (Test-Path -LiteralPath $channelKey) {
    $channel = Get-WinEvent -ListLog 'USNJMon/Operational'
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
    if ($service -and ($service.PathName -ne $command -or $service.State -ne 'Stopped')) {
        throw 'Upgrade requires this installation''s service to be stopped.'
    }
    foreach ($path in @($install, $data, $state, $config)) {
        $item = Get-Item -LiteralPath $path
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing reparse-point installation path: $path" }
    }
    if (Get-CimInstance Win32_Process -Filter "Name='usnjmon.exe'" |
        Where-Object { $_.ExecutablePath -eq (Join-Path $install 'usnjmon.exe') }) {
        throw 'Stop foreground collection before upgrading.'
    }
} elseif ($service -or (Test-Path -LiteralPath $install) -or (Test-Path -LiteralPath $data) -or
    (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WINEVT\Publishers\{6427dbf2-7731-48ea-9166-4a9cf0a3b5d7}') -or
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

if ($PSCmdlet.ShouldProcess('USNJMon', 'Install/update provider resources; install a stopped service unless -ProviderOnly; preserve existing history')) {
    $stateLock = $null
    try {
        if (-not $Upgrade) {
            New-PrivateDirectory $install $true
            New-PrivateDirectory $data $false
            New-PrivateDirectory $state $false
            Copy-Item -LiteralPath $config -Destination (Join-Path $data 'config.json')
        }
        $stateLock = [IO.File]::Open((Join-Path $state 'collector.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
        foreach ($name in @('usnjmon.exe', 'usnjmon-provider.dll', 'usnjmon.man')) {
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
        $installedDll = Join-Path $install 'usnjmon-provider.dll'
        & wevtutil.exe im (Join-Path $install 'usnjmon.man') "/rf:$installedDll" "/mf:$installedDll"
        if ($LASTEXITCODE -ne 0) { throw 'Native provider registration failed' }
        if ($null -ne $previousChannel) {
            $updatedChannel = Get-WinEvent -ListLog 'USNJMon/Operational'
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
            & wevtutil.exe sl 'USNJMon/Operational' /e:true "/ms:$($MaximumSizeMB * 1MB)"
            if ($LASTEXITCODE -ne 0) { throw 'Native channel configuration failed' }
        }
        if (-not $ProviderOnly -and -not $service) {
            New-Service -Name USNJMon -DisplayName 'USNJMon' -BinaryPathName $command `
                -StartupType Manual -DependsOn EventLog -Description 'Read-only retained NTFS USN journal collection with durable checkpoints.' | Out-Null
        }
        if ($ProviderOnly) {
            Write-Output 'Provider resources installed for Event Viewer descriptions. No service was created or started.'
            Write-Output 'To add a stopped service later, rerun with -Upgrade (without -ProviderOnly).'
        } else {
            Write-Output 'USNJMon installed but NOT started. Start explicitly with: Start-Service USNJMon'
            Write-Output 'For automatic startup: Set-Service USNJMon -StartupType Automatic'
        }
        Write-Output 'Existing UsnEventLog and UsnJournal installations, logs and checkpoints are untouched.'
    } catch {
        Write-Error "Installation failed; partial application-owned artifacts may remain. No service was started. Inspect USNJMon paths and .previous backups before retrying. $($_.Exception.Message)"
        throw
    } finally {
        if ($null -ne $stateLock) { $stateLock.Dispose() }
    }
}
