#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $ExpectedVersion,
    [Parameter(Mandatory)] [string] $OutputDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Assert-Fails([scriptblock] $Action, [string] $Message) {
    try {
        & $Action | Out-Host
        throw 'Expected validation to reject this input.'
    } catch {
        if ($_.Exception.Message -notlike $Message) { throw }
    }
}
$package = & "$PSScriptRoot\Package.ps1" -ExpectedVersion $ExpectedVersion -OutputDirectory $OutputDirectory
& "$PSScriptRoot\Test-Package.ps1" -Archive $package.Archive -ExpectedVersion $ExpectedVersion | Out-Host
Assert-Fails {
    & "$PSScriptRoot\Package.ps1" -ExpectedVersion $ExpectedVersion -OutputDirectory $OutputDirectory
} '*OutputDirectory must not already exist*'
$wrongVersion = if ($ExpectedVersion -ne '0.0.0') { '0.0.0' } else { '0.0.1' }
Assert-Fails {
    & "$PSScriptRoot\Package.ps1" -ExpectedVersion $wrongVersion -OutputDirectory $OutputDirectory
} '*Requested version must match*'
$scratch = Join-Path ([IO.Path]::GetTempPath()) ("usnjmon-packaging-negative-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $corrupt = Join-Path $scratch 'corrupt.zip'
    Copy-Item -LiteralPath $package.Archive -Destination $corrupt
    $zip = [IO.Compression.ZipFile]::Open($corrupt, [IO.Compression.ZipArchiveMode]::Update)
    try {
        $entry = $zip.Entries | Where-Object { $_.FullName.Replace('/', '\').EndsWith('\bin\usnjmon-provider.dll') }
        $stream = $entry.Open()
        try {
            $stream.SetLength(0)
            $stream.WriteByte(0)
        } finally { $stream.Dispose() }
    } finally { $zip.Dispose() }
    Assert-Fails {
        & "$PSScriptRoot\Test-Package.ps1" -Archive $corrupt -ExpectedVersion $ExpectedVersion
    } '*checksum mismatch*'
    $missingLicense = Join-Path $scratch 'missing-license.zip'
    Copy-Item -LiteralPath $package.Archive -Destination $missingLicense
    $zip = [IO.Compression.ZipFile]::Open($missingLicense, [IO.Compression.ZipArchiveMode]::Update)
    try {
        $entry = $zip.Entries | Where-Object {
            $_.FullName.Replace('/', '\') -eq "usnjmon-$ExpectedVersion-windows-x64\LICENSE"
        }
        $entry.Delete()
    } finally { $zip.Dispose() }
    Assert-Fails {
        & "$PSScriptRoot\Test-Package.ps1" -Archive $missingLicense -ExpectedVersion $ExpectedVersion
    } '*Missing packaged file: LICENSE*'
    $unsafe = Join-Path $scratch 'unsafe.zip'
    Copy-Item -LiteralPath $package.Archive -Destination $unsafe
    $zip = [IO.Compression.ZipFile]::Open($unsafe, [IO.Compression.ZipArchiveMode]::Update)
    try { [void]$zip.CreateEntry('../escape.txt') } finally { $zip.Dispose() }
    Assert-Fails {
        & "$PSScriptRoot\Test-Package.ps1" -Archive $unsafe -ExpectedVersion $ExpectedVersion
    } '*Unsafe or duplicate archive entry*'
    $duplicate = Join-Path $scratch 'duplicate.zip'
    Copy-Item -LiteralPath $package.Archive -Destination $duplicate
    $zip = [IO.Compression.ZipFile]::Open($duplicate, [IO.Compression.ZipArchiveMode]::Update)
    try { [void]$zip.CreateEntry($zip.Entries[0].FullName) } finally { $zip.Dispose() }
    Assert-Fails {
        & "$PSScriptRoot\Test-Package.ps1" -Archive $duplicate -ExpectedVersion $ExpectedVersion
    } '*Unsafe or duplicate archive entry*'
    $sbomPath = Join-Path $package.Directory "usnjmon-$ExpectedVersion-windows-x64.sbom.cdx.json"
    $sbom = Get-Content -LiteralPath $sbomPath -Raw | ConvertFrom-Json
    if (@($sbom.components | Where-Object { $_.name -eq 'roxmltree' }).Count -ne 0) {
        throw 'Dev-only dependency leaked into the release inventory.'
    }
    foreach ($name in @('usnjmon-core', 'usnjmon-evtx')) {
        $component = @($sbom.components | Where-Object { $_.name -eq $name })
        if ($component.Count -ne 1 -or $component[0].version -ne $ExpectedVersion -or
            $component[0].purl -ne "pkg:generic/$name@$ExpectedVersion" -or
            $component[0].licenses[0].expression -ne 'MIT') {
            throw "Workspace dependency missing version, provenance or MIT license: $name"
        }
        $applicationDeps = $sbom.dependencies | Where-Object { $_.ref -eq $sbom.metadata.component.'bom-ref' }
        if ($component[0].'bom-ref' -notin $applicationDeps.dependsOn) {
            throw "Workspace dependency missing from application graph: $name"
        }
    }
    $references = @($sbom.components | ForEach-Object { $_.'bom-ref' }) + @($sbom.metadata.component.'bom-ref')
    if (@($references | Select-Object -Unique).Count -ne $references.Count) { throw 'Duplicate SBOM component references.' }
    foreach ($dependency in $sbom.dependencies) {
        if ($dependency.ref -notin $references) { throw 'Unknown SBOM dependency source.' }
        foreach ($child in $dependency.dependsOn) {
            if ($child -notin $references) { throw 'Unknown SBOM dependency target.' }
        }
    }
    foreach ($line in Get-Content -LiteralPath (Join-Path $package.Directory 'SHA256SUMS.txt')) {
        if ($line -notmatch '^([a-f0-9]{64})  ([a-zA-Z0-9.-]+)$') { throw 'Malformed asset checksum entry.' }
        if ((Get-FileHash -LiteralPath (Join-Path $package.Directory $Matches[2])).Hash -ne $Matches[1]) {
            throw 'Asset checksum mismatch.'
        }
    }
    Write-Host 'Packaging checks passed: extraction/resources, MIT licensing, version/output guards, corruption, traversal, duplicate entries, SBOM references and asset hashes.'
    $package
} finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force
}
