# Preview releases

Releases are started manually with the **Preview release** GitHub Actions
workflow, from `main`. Pushes, merges and tags do not publish a release.
The repository's visibility also controls access to release downloads.

## Download and install

Download `usnjmon-<version>-windows-x64.zip` and `SHA256SUMS.txt` from the
matching GitHub prerelease. Compare the ZIP's SHA-256 with the published
checksum before extracting:

```powershell
Get-FileHash .\usnjmon-0.5.2-windows-x64.zip -Algorithm SHA256
Expand-Archive .\usnjmon-0.5.2-windows-x64.zip -DestinationPath .\preview
Set-Location .\preview\usnjmon-0.5.2-windows-x64
.\bin\usnjmon.exe check-config .\config.example.json
.\bin\usnjmon.exe verify-resources .\bin\usnjmon-provider.dll
```

Keep the EXE, resource DLL and manifest together. Rust, Visual Studio and the
Windows SDK are not needed to use a release. The EXE can also convert extracted
journals without installing a service; see [conversion](conversion.md).

Review the configuration before installation: the example selects all accessible
local NTFS volumes, not just `C:`. To select only `C:`, change
`volumes.include` to `["C:\\"]` in your own copy of the configuration.
Then use elevated **64-bit Windows PowerShell 5.1**:

```powershell
.\scripts\Install.ps1 -BinaryDirectory .\bin -ConfigPath .\config.example.json
# Installed stopped, with Manual startup. Start only when ready:
Start-Service USNJMon
```

Upgrades use the same service/config/checkpoint locations:

```powershell
Stop-Service USNJMon
.\scripts\Install.ps1 -Upgrade -BinaryDirectory .\bin
# Inspect the result before explicitly restarting:
Start-Service USNJMon
```

Upgrade preserves configuration, checkpoints, startup mode and existing channel
settings unless explicitly overridden. It is not transactional: keep the prior
release, inspect `.previous` backups on failure, and do not delete checkpoints.
Default uninstall retains files, state and event resources. See the
[service guide](service.md) for recovery and uninstall details.

## Contents and trust

The ZIP includes only the three built artifacts, official install/uninstall
scripts, tracked documentation, example configuration, build metadata,
file checksums, the MIT `LICENSE`, and dependency notices/inventory. It excludes machine logs,
checkpoints, local settings and deployment/diagnostic reports.

`build-info.json` records the source commit, compiler, target, lockfile hash,
source cleanliness, project license and CI run URL when built by Actions. The CycloneDX 1.6
SBOM includes the target's non-dev Cargo closure (including build/proc-macro
dependencies) and Rust's standard library, not every Windows platform component.
Rust's standard-library notice file covers its bundled third-party notices.
First-party workspace packages appear as `pkg:generic` components with the same
version and MIT license as the application, covered by the root `LICENSE` and
the recorded source commit. Registry dependencies retain their `pkg:cargo`
identities and individual notices. Packaging rejects unreviewed non-workspace
path/git dependencies and workspace members outside `crates` or with a different
version/license.

Preview executables and scripts are **unsigned**. Checksums detect file changes;
they are not Authenticode signatures or signed build attestations.
USNJMon is licensed under the [MIT License](../LICENSE). Preserve its copyright
and permission notice when redistributing copies or substantial portions.
Dependencies retain their respective licenses; their notices are included in
`third-party`. Both build metadata and the SBOM identify the project license as MIT.

## Maintainer release procedure

1. Merge the desired changes and version update in `Cargo.toml`/`Cargo.lock`.
   Update `workspace.package.version` and the exact versions of both internal
   `workspace.dependencies` together; all three packages inherit that version.
2. Confirm main's Windows CI passes. Review [live acceptance](validation.md)
   on a disposable authorized machine; CI does not install/start services.
3. Run **Actions > Preview release > Run workflow**, select `main`, and enter
   the exact numeric Cargo version, for example `0.5.2`.
4. The workflow pins its dispatched commit, runs the official build checks,
   packages Windows x64 MSVC, and verifies the extracted files under Windows
   PowerShell 5.1 before publishing `v<version>` as a prerelease.
5. Inspect the release assets and download/check the ZIP. Published versions
   are never intentionally reused or overwritten; use a new version for changes.

Release publishing is serialized. Only the publishing job has repository-write
permission. A failed run may leave a partial release/tag; inspect and recover
that state explicitly rather than force-moving tags or replacing assets.

To package locally after `scripts\Build.ps1`, using a new output directory:

```powershell
$package = .\scripts\Package.ps1 -ExpectedVersion 0.5.2 -OutputDirectory C:\Artifacts\usnjmon-0.5.2
.\scripts\Test-Package.ps1 -Archive $package.Archive -ExpectedVersion 0.5.2
```

The release ZIP, external SBOM/build metadata, and `SHA256SUMS.txt` are the
four downloadable assets. Staged uncompressed contents remain in the output
directory for inspection. Local dirty builds are marked as such; the publishing
workflow requires the exact clean commit.

## Preview limitations

Journal and destination retention are separate. A small or busy source journal
can wrap before backfill reaches old records; event 102 reports the gap but cannot
count/recover missing records. Completion event 103 does not prove gap-free
history. The circular destination log can overwrite already-collected records.
Forward/export events when longer retention is needed.

USN records do not identify the responsible user/process or contain file-content
diffs. Full paths are current-parent candidates, not historical-path guarantees.
ETW acceptance is not acknowledgement of durable EVTX storage. Passing tests or a
brief live smoke check must not be described as proof of lossless collection.
