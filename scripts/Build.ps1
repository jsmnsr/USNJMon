#requires -Version 5.1
[CmdletBinding()]
param(
    [ValidateSet('x86_64-pc-windows-msvc', 'x86_64-pc-windows-gnu')]
    [string] $Target = 'x86_64-pc-windows-msvc'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $env:WINDOWS_SDK_BIN) {
    $sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Directory |
        Where-Object { Test-Path (Join-Path $_.FullName 'x64\mc.exe') } |
        Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
    if (-not $sdk) { throw 'Install the Windows 10/11 SDK (mc.exe and rc.exe).' }
    $env:WINDOWS_SDK_BIN = Join-Path $sdk.FullName 'x64'
}
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    & cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw 'Formatting failed' }
    & cargo test --locked --target $Target --all-targets
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
    & cargo build --locked --target $Target --release
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    & ".\target\$Target\release\usn-eventlog.exe" verify-resources ".\target\$Target\release\usn-eventlog-messages.dll"
    if ($LASTEXITCODE -ne 0) { throw 'Message formatting failed' }
    Write-Output "Artifacts: target\$Target\release\usn-eventlog.exe and usn-eventlog-messages.dll"
} finally { Pop-Location }
