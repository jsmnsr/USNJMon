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
    & cargo clippy --locked --workspace --target $Target --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
    & cargo test --locked --workspace --target $Target --all-targets
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
    & cargo build --locked -p usnjmon --target $Target --release
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    & ".\target\$Target\release\usnjmon.exe" verify-resources ".\target\$Target\release\usnjmon-provider.dll"
    if ($LASTEXITCODE -ne 0) { throw 'Message formatting failed' }
    Write-Output "Artifacts: target\$Target\release\usnjmon.exe, usnjmon-provider.dll and usnjmon.man"
} finally { Pop-Location }
