# USNJMon

**Turn NTFS USN journal records into Windows events.** Convert an extracted
journal to an EVTX file, or run a Windows service that collects new changes
while backfilling retained history.

USNJMon is read-only toward journals. It does not create, resize or delete
them, and it does not depend on Sysmon. USN records contain **no process or
user attribution** and cannot recover history that has already wrapped.

## 1. Convert a journal to EVTX

```powershell
.\usnjmon.exe convert .\journal.usnj .\journal.evtx
Get-WinEvent -Path .\journal.evtx -Oldest | Select-Object TimeCreated, Id
```

Input is an extracted **`$UsnJrnl:$J` stream** containing NTFS V2/V3 records,
not a disk image, CSV or existing EVTX. Conversion needs no service,
administrator rights or live-log registration. It preserves source timestamps
and raw metadata, handles zero-filled sparse regions, and never overwrites
existing output.

Open the result in Event Viewer with **Open Saved Log**. Named fields are
available in **Details** without installation. General-tab descriptions require
the USNJMon provider resources; [conversion instructions](docs/conversion.md)
explain optional resource-only installation and unavailable source metadata.

## 2. Monitor continuously

Download a Windows x64 preview from [GitHub Releases](https://github.com/jsmnsr/USNJMon/releases)
and follow the [release installation guide](docs/releases.md), or build from source.
In elevated **Windows PowerShell 5.1**, from a built source checkout:

```powershell
.\scripts\Install.ps1 -BinaryDirectory .\target\x86_64-pc-windows-msvc\release
# Installs a stopped, Manual-start service using config.example.json.
Start-Service USNJMon
# Optional: Set-Service USNJMon -StartupType Automatic
```

View **Applications and Services Logs > USNJMon > Operational**. Press F5
to refresh. Stop with `Stop-Service USNJMon`.

By default, the service discovers all accessible local NTFS volumes and applies
conservative noise filters. Pass `-ConfigPath` to install your own volume
selectors and path whitelist/blacklist. [Service configuration and operation](docs/service.md)
cover filters, checkpoints, retention, upgrades and removal.

## Find the events you need

| Event ID | Change |
| --- | --- |
| 1000 / 1001 | Create / delete |
| 1002 / 1003 | Rename old / new name |
| 1010 / 1020 / 1030 | Data / security / metadata |
| 1099 | Other |

One event represents one USN record, including all combined reason flags.
Filter by `FileName` in Details XML; [schema and query examples](docs/events.md)
include exact filename and live-only filters.

## Build and compatibility

Windows x64, Rust 1.99.1, Windows SDK; MSVC and GNU targets.
Run `.\scripts\Build.ps1` after [setting up the build tools](docs/development.md).
Preview ZIPs include the executable, provider resources and installation scripts;
no compiler is needed on the destination machine. Releases are manually published,
unsigned prereleases; see [packaging and release policy](docs/releases.md).

Renamed from **UsnEventLog**: existing installations are not changed automatically.
Read the [migration instructions](docs/service.md#upgrading-and-migrating) before
switching services. The repository URL remains unchanged.

## License

USNJMon is licensed under the [MIT License](LICENSE).
Release ZIPs include this license and third-party notices in `third-party`;
dependencies remain covered by their respective licenses.
