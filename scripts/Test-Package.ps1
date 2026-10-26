#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Archive,
    [Parameter(Mandatory)] [ValidatePattern('^\d+\.\d+\.\d+$')] [string] $ExpectedVersion
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archivePath = (Resolve-Path -LiteralPath $Archive).Path
$stem = "usnjmon-$ExpectedVersion-windows-x64"
$scratch = Join-Path ([IO.Path]::GetTempPath()) ("usnjmon-package-test-" + [guid]::NewGuid().ToString('N'))
$zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
try {
    $names = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $zip.Entries) {
        $name = $entry.FullName.Replace('/', '\')
        if (-not $name.StartsWith("$stem\", [StringComparison]::Ordinal) -or
            $name -match '(^|\\)\.\.?($|\\)|:|\\\\' -or -not $names.Add($name)) {
            throw "Unsafe or duplicate archive entry: $name"
        }
    }
} finally { $zip.Dispose() }
try {
    [IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $scratch)
    $package = Join-Path $scratch $stem
    foreach ($name in @('bin\usnjmon.exe', 'bin\usnjmon-provider.dll', 'bin\usnjmon.man',
        'scripts\Install.ps1', 'scripts\Uninstall.ps1', 'config.example.json', 'README.md',
        'docs\releases.md', 'third-party\Rust-standard-library.html', 'build-info.json',
        'sbom.cdx.json', 'FILES.sha256', 'LICENSE')) {
        if (-not (Test-Path -LiteralPath (Join-Path $package $name) -PathType Leaf)) {
            throw "Missing packaged file: $name"
        }
    }
    $listed = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    foreach ($line in [IO.File]::ReadAllLines("$package\FILES.sha256")) {
        if ($line -notmatch '^([a-f0-9]{64})  (.+)$') { throw 'Malformed package checksum entry.' }
        $expectedHash = $Matches[1]
        $relative = $Matches[2].Replace('/', '\')
        if ([IO.Path]::IsPathRooted($relative) -or $relative -match '(^|\\)\.\.?($|\\)|:' -or -not $listed.Add($relative)) {
            throw 'Unsafe or duplicate checksum path.'
        }
        if ((Get-FileHash -LiteralPath (Join-Path $package $relative)).Hash -ne $expectedHash) {
            throw "Package checksum mismatch: $relative"
        }
    }
    $files = @(Get-ChildItem -LiteralPath $package -File -Recurse)
    if ($files.Count -ne $listed.Count + 1 -or $listed.Contains('FILES.sha256')) {
        throw 'Package contains unlisted files.'
    }
    foreach ($file in $files) {
        $relative = $file.FullName.Substring($package.Length + 1)
        if ($relative -ne 'FILES.sha256' -and -not $listed.Contains($relative)) {
            throw "Unlisted package file: $relative"
        }
    }
    $build = Get-Content -LiteralPath "$package\build-info.json" -Raw | ConvertFrom-Json
    $sbom = Get-Content -LiteralPath "$package\sbom.cdx.json" -Raw | ConvertFrom-Json
    if ($build.projectLicense -ne 'MIT' -or
        @($sbom.metadata.component.licenses).Count -ne 1 -or
        $sbom.metadata.component.licenses[0].expression -ne 'MIT') {
        throw 'Package must declare the MIT project license in build metadata and SBOM.'
    }
    $sourceLicense = Join-Path (Split-Path $PSScriptRoot -Parent) 'LICENSE'
    if ((Get-FileHash -LiteralPath "$package\LICENSE").Hash -ne
        (Get-FileHash -LiteralPath $sourceLicense).Hash) {
        throw 'Packaged LICENSE does not match the source license.'
    }
    if ($build.version -ne $ExpectedVersion -or $build.target -ne 'x86_64-pc-windows-msvc' -or
        $build.commit -notmatch '^[a-f0-9]{40}$' -or $sbom.bomFormat -ne 'CycloneDX' -or
        $sbom.metadata.component.version -ne $ExpectedVersion -or $sbom.components.Count -eq 0) {
        throw 'Package identity or dependency inventory is invalid.'
    }
    foreach ($script in @('Install.ps1', 'Uninstall.ps1')) {
        $parseErrors = $null
        [void][Management.Automation.Language.Parser]::ParseFile(
            "$package\scripts\$script", [ref]$null, [ref]$parseErrors)
        if ($parseErrors) { throw ($parseErrors | Out-String) }
    }
    & "$package\bin\usnjmon.exe" check-config "$package\config.example.json"
    if ($LASTEXITCODE -ne 0) { throw 'Packaged configuration validation failed.' }
    & "$package\bin\usnjmon.exe" verify-resources "$package\bin\usnjmon-provider.dll"
    if ($LASTEXITCODE -ne 0) { throw 'Packaged message-resource validation failed.' }
    Write-Output "Verified $($files.Count) packaged files; no installation, journal access or event writes."
} finally {
    if (Test-Path -LiteralPath $scratch) { Remove-Item -LiteralPath $scratch -Recurse -Force }
}
