#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidatePattern('^\d+\.\d+\.\d+$')] [string] $ExpectedVersion,
    [Parameter(Mandatory)] [string] $OutputDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$target = 'x86_64-pc-windows-msvc'
$root = Split-Path $PSScriptRoot -Parent
$utf8 = New-Object Text.UTF8Encoding $false
function Write-Json($Value, [string] $Path) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 30), $utf8)
}
Push-Location $root
try {
    $metadataText = & cargo metadata --locked --offline --format-version 1 --filter-platform $target
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed; run Build.ps1 first.' }
    $metadata = ($metadataText -join "`n") | ConvertFrom-Json
    $application = $metadata.packages | Where-Object { $_.id -eq $metadata.resolve.root }
    if ($application.name -ne 'usnjmon' -or $application.version -ne $ExpectedVersion) {
        throw 'Requested version must match the usnjmon version in Cargo.toml.'
    }
    if (Test-Path -LiteralPath $OutputDirectory) { throw 'OutputDirectory must not already exist.' }
    $commit = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source commit.' }
    $status = @(& git status --porcelain --untracked-files=normal)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect source status.' }
    $compiler = & rustc --version
    if ($LASTEXITCODE -ne 0) { throw 'Cannot identify Rust compiler.' }
    $sysroot = & rustc --print sysroot
    if ($LASTEXITCODE -ne 0) { throw 'Cannot locate Rust runtime notices.' }
    $runtimeNotices = Join-Path $sysroot 'share\doc\rust\COPYRIGHT-library.html'
    if (-not (Test-Path -LiteralPath $runtimeNotices -PathType Leaf)) {
        throw 'Rust standard-library copyright notices are required.'
    }
    $binaryDirectory = Join-Path $root "target\$target\release"
    foreach ($name in @('usnjmon.exe', 'usnjmon-provider.dll', 'usnjmon.man')) {
        if (-not (Test-Path -LiteralPath (Join-Path $binaryDirectory $name) -PathType Leaf)) {
            throw "Missing release artifact: $name; run Build.ps1 first."
        }
    }
    New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
    $output = (Resolve-Path -LiteralPath $OutputDirectory).Path
    $stem = "usnjmon-$ExpectedVersion-windows-x64"
    $stage = Join-Path $output $stem
    foreach ($directory in @($stage, "$stage\bin", "$stage\scripts", "$stage\docs", "$stage\third-party")) {
        New-Item -ItemType Directory -Path $directory | Out-Null
    }
    foreach ($name in @('usnjmon.exe', 'usnjmon-provider.dll', 'usnjmon.man')) {
        Copy-Item -LiteralPath (Join-Path $binaryDirectory $name) -Destination "$stage\bin"
    }
    foreach ($name in @('Install.ps1', 'Uninstall.ps1')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination "$stage\scripts"
    }
    foreach ($name in @('README.md', 'config.example.json')) {
        Copy-Item -LiteralPath (Join-Path $root $name) -Destination $stage
    }
    $documents = @(& git ls-files -- 'docs/*.md')
    if ($LASTEXITCODE -ne 0) { throw 'Cannot list tracked documentation.' }
    foreach ($document in $documents) {
        Copy-Item -LiteralPath (Join-Path $root $document.Replace('/', '\')) -Destination "$stage\docs"
    }
    # Include project licensing if one is added; never invent a license for the owner.
    $projectNotices = @(& git ls-files -- 'LICENSE*' 'COPYING*' 'NOTICE*')
    if ($LASTEXITCODE -ne 0) { throw 'Cannot list project notices.' }
    foreach ($notice in $projectNotices) {
        Copy-Item -LiteralPath (Join-Path $root $notice) -Destination $stage
    }
    Copy-Item -LiteralPath $runtimeNotices -Destination "$stage\third-party\Rust-standard-library.html"

    $nodes = @{}
    $packages = @{}
    foreach ($node in $metadata.resolve.nodes) { $nodes[$node.id] = $node }
    foreach ($package in $metadata.packages) { $packages[$package.id] = $package }
    $selected = New-Object 'System.Collections.Generic.HashSet[string]'
    $queue = New-Object 'System.Collections.Generic.Queue[string]'
    $queue.Enqueue($application.id)
    while ($queue.Count -gt 0) {
        $id = $queue.Dequeue()
        if (-not $selected.Add($id)) { continue }
        foreach ($dependency in $nodes[$id].deps) {
            if (@($dependency.dep_kinds | Where-Object { $_.kind -ne 'dev' }).Count -gt 0) {
                $queue.Enqueue($dependency.pkg)
            }
        }
    }
    $refs = @{}
    $refs[$application.id] = "pkg:generic/usnjmon@$ExpectedVersion"
    $components = @()
    foreach ($id in ($selected | Sort-Object)) {
        if ($id -eq $application.id) { continue }
        $package = $packages[$id]
        if (-not $package.source -or -not $package.source.StartsWith('registry+')) {
            throw "Review licensing/provenance before packaging non-registry dependency: $($package.name)"
        }
        if (-not $package.license) { throw "Missing dependency license expression: $($package.name)" }
        $reference = "pkg:cargo/$($package.name)@$($package.version)"
        $refs[$id] = $reference
        $license = $package.license.Replace('MIT/Apache-2.0', 'MIT OR Apache-2.0')
        $components += [ordered]@{
            type = 'library'; 'bom-ref' = $reference; name = $package.name
            version = $package.version; purl = $reference
            licenses = @(@{ expression = $license })
        }
        $source = Split-Path $package.manifest_path -Parent
        $notices = @(Get-ChildItem -LiteralPath $source -File |
            Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)([._-]|$)' })
        if ($package.license_file) {
            $notices += Get-Item -LiteralPath (Join-Path $source $package.license_file)
        }
        $notices = @($notices | Sort-Object FullName -Unique)
        if ($notices.Count -eq 0) { throw "No license notices found for $($package.name)." }
        $destination = Join-Path "$stage\third-party" "$($package.name)-$($package.version)"
        New-Item -ItemType Directory -Path $destination | Out-Null
        foreach ($notice in $notices) {
            Copy-Item -LiteralPath $notice.FullName -Destination $destination
        }
    }
    $rustVersion = ($compiler -split ' ')[1]
    $rustRef = "pkg:generic/rust-std@$rustVersion"
    $components += [ordered]@{
        type = 'library'; 'bom-ref' = $rustRef; name = 'rust-std'; version = $rustVersion
        purl = $rustRef; licenses = @(@{ expression = 'MIT OR Apache-2.0' })
    }
    $dependencies = @()
    foreach ($id in ($selected | Sort-Object)) {
        $children = @($nodes[$id].deps | Where-Object {
            @($_.dep_kinds | Where-Object { $_.kind -ne 'dev' }).Count -gt 0
        } | ForEach-Object { $refs[$_.pkg] } | Sort-Object -Unique)
        if ($id -eq $application.id) { $children += $rustRef }
        $dependencies += @{ ref = $refs[$id]; dependsOn = $children }
    }
    $dependencies += @{ ref = $rustRef; dependsOn = @() }
    $timestamp = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
    $sbom = [ordered]@{
        bomFormat = 'CycloneDX'; specVersion = '1.6'; version = 1
        metadata = @{
            timestamp = $timestamp
            component = @{
                type = 'application'; 'bom-ref' = $refs[$application.id]
                name = 'usnjmon'; version = $ExpectedVersion; purl = $refs[$application.id]
            }
        }
        components = $components
        dependencies = $dependencies
    }
    Write-Json $sbom "$stage\sbom.cdx.json"
    $build = [ordered]@{
        version = $ExpectedVersion; target = $target; commit = $commit
        compiler = $compiler; packagedUtc = $timestamp
        cleanSource = ($status.Count -eq 0)
        cargoLockSha256 = (Get-FileHash -LiteralPath (Join-Path $root 'Cargo.lock')).Hash.ToLowerInvariant()
        authenticodeSigned = $false
        projectLicense = if ($projectNotices.Count) { 'See included project notices.' } else { 'No project license is declared; no redistribution license is granted by this package.' }
        sbomScope = 'Non-dev Cargo dependency closure (including build/proc-macro dependencies) and Rust standard library; not a complete inventory of the Windows platform.'
    }
    if ($env:GITHUB_ACTIONS -eq 'true') {
        $build.workflowRun = "$env:GITHUB_SERVER_URL/$env:GITHUB_REPOSITORY/actions/runs/$env:GITHUB_RUN_ID"
    }
    Write-Json $build "$stage\build-info.json"
    $fileHashes = @(Get-ChildItem -LiteralPath $stage -File -Recurse | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($stage.Length + 1).Replace('\', '/')
        '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant(), $relative
    })
    [IO.File]::WriteAllLines("$stage\FILES.sha256", $fileHashes, $utf8)
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = Join-Path $output "$stem.zip"
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $archive, [IO.Compression.CompressionLevel]::Optimal, $true)
    Copy-Item -LiteralPath "$stage\sbom.cdx.json" -Destination "$output\$stem.sbom.cdx.json"
    Copy-Item -LiteralPath "$stage\build-info.json" -Destination "$output\$stem.build-info.json"
    $assetHashes = @(Get-ChildItem -LiteralPath $output -File | Sort-Object Name | ForEach-Object {
        '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant(), $_.Name
    })
    [IO.File]::WriteAllLines("$output\SHA256SUMS.txt", $assetHashes, $utf8)
    [pscustomobject]@{ Version = $ExpectedVersion; Archive = $archive; Directory = $output }
} finally { Pop-Location }
