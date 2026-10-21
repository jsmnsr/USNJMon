# USN Event Log Bridge

An independent **Rust Windows x64 service** that reads existing local NTFS USN
journals and collects **live changes while independently backfilling retained
history**. Version 0.3 writes native, named events through provider **UsnEventLog**
to **UsnEventLog/Operational**. Event Viewer shows readable, multiline
descriptions in **General** and typed fields in **Details**. No JSON parser is
needed. It does not depend on Sysmon or impersonate a Microsoft provider.

This is not a forensic recovery tool or a complete Sysmon replacement. USN
records **do not identify the responsible process, PID, user, command line, or
file contents**. No attribution is inferred. Journals are only queried/read:
never created, resized, deleted or changed. No VSS or raw-disk recovery is used.

## Events you can filter

| ID | Primary change category |
| --- | --- |
| 1000 | File/directory created |
| 1001 | File/directory deleted |
| 1002 | Rename - old name |
| 1003 | Rename - new name |
| 1010 | Data changed |
| 1020 | Security changed |
| 1030 | Attributes/metadata changed |
| 1099 | Other, including close-only/unknown reasons |

One USN record produces **one event**, not one event per reason bit. Priority
when reasons combine is **delete > rename-new > rename-old > create > security
> data > metadata > other**. Every reason remains visible in `Reasons` and
`ReasonFlags`, regardless of the primary category. These are journal-record
categories, not reconstructed operations or guaranteed rename pairs.
Health/coverage IDs 101-106 remain separate.

An event's General tab looks like:

```text
File/directory deleted

File name: example.txt
Object: File
Original time (UTC): ...
Historical backfill: true
Reasons: FILE_DELETE | CLOSE
Raw reason flags: 0x80000200
...
```

Details includes `FileName`, `OriginalTimestamp` (FILETIME), `Historical`
(Boolean), `Reasons`, `ReasonFlags`, `Usn`, `JournalId`, file/parent references,
attributes, record version and a stable `EventIdentity`. Original UTF-16 name
units remain available in `FileNameUtf16Hex`; display control characters are
escaped and labeled. [Full schema and query examples](docs/events.md).

## Build and non-invasive validation

Targets: `x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu`; Windows 10/11 or
Server 2016+ x64. Install Rust **1.99.1** with rustfmt and the Windows 10/11 SDK
(`mc.exe`, `rc.exe`). MSVC needs Visual Studio C++ Build Tools. GNU needs MinGW
binutils including `dlltool` and its assembler, or LLVM's `llvm-dlltool`
configured with an absolute `RUSTFLAGS=-C dlltool=...` path.

```powershell
rustup toolchain install 1.99.1 --profile minimal --component rustfmt
rustup override set 1.99.1
rustup target add x86_64-pc-windows-msvc
.\scripts\Build.ps1
# Or: .\scripts\Build.ps1 -Target x86_64-pc-windows-gnu
```

The wrapper discovers the SDK; alternatively set `WINDOWS_SDK_BIN` to its
`bin\<version>\x64` directory. Dependencies are locked. The wrapper checks
formatting, tests, release builds and native resource formatting. CI runs both
targets. Automated tests do not register providers, install/start services,
read journals, or write Event Log records. A disposable file is used to check
native I/O completion; other source records are synthetic.

Distribute these files from `target\<target>\release` together:

- `usn-eventlog.exe`
- `usn-eventlog-provider.dll` and `usn-eventlog.man` (native provider resources)
- `usn-eventlog-messages.dll` (legacy rendering resources)

The manifest compiler validates the schema. Tests compare every wire field's
name/type/order and event descriptor against the manifest/generated header,
check combined-reason priorities, failure/replay behavior, Windows resource
rendering, checkpoints, cancellation and journal parsing.

## Install and operate

Installation is a persistent host change. Use elevated **64-bit Windows
PowerShell 5.1**, not PowerShell 7. Review `config.example.json`, select volumes
with existing active journals, and save the configuration outside this repo.

```powershell
.\scripts\Install.ps1 -BinaryDirectory .\target\x86_64-pc-windows-msvc\release `
    -ConfigPath C:\Admin\usn-config.json
# Installer leaves the service STOPPED with Manual startup.
Start-Service UsnEventLog
Get-Service UsnEventLog
Stop-Service UsnEventLog
# Optional, after acceptance testing:
Set-Service UsnEventLog -StartupType Automatic
```

Use `-WhatIf` to preview persistent installation changes. The service uses
LocalSystem because opening a volume requires elevated access, but requests
only `GENERIC_READ`; it never enables backup/manage-volume privileges or
broadens volume ACLs. There is no network listener.

Only Administrators and SYSTEM can modify `%ProgramFiles%\UsnEventLog`;
Users can read its message resources. Configuration/checkpoints under
`%ProgramData%\UsnEventLog` are private to Administrators and SYSTEM.
The new channel grants SYSTEM/Administrators full access and Event Log Readers
read access. These ACLs affect only this application's directories/channel.

Foreground mode writes the same real native log. Stop the service first:

```powershell
& "$env:ProgramFiles\UsnEventLog\usn-eventlog.exe" foreground `
    "$env:ProgramData\UsnEventLog\config.json" "$env:ProgramData\UsnEventLog\state"
# Ctrl+C requests cancellation.
```

A state-directory lock excludes concurrent collectors/upgrades. Custom state
directories must have equivalent private ACLs. `check-config <config.json>`
and `verify-resources <usn-eventlog-provider.dll>` are non-invasive.

## Upgrade from the JSON-only version

Stop all service/foreground collection, close Event Viewer if it has resources
open, then:

```powershell
.\scripts\Install.ps1 -Upgrade `
    -BinaryDirectory .\target\x86_64-pc-windows-msvc\release
```

Upgrade preserves configuration, checkpoints, service startup mode and the
legacy **UsnJournal / UsnJournalBridge** log/source/message DLL. It leaves the
service stopped and backs up replaced artifacts as `.previous`. Collection
continues from the existing checkpoint into **UsnEventLog/Operational**; it
does not replay already-accepted legacy records just to reformat them.
Legacy JSON events remain readable in their original log and format.
For complete history across the upgrade, retain/query **both** logs.
Subsequent native-provider upgrades preserve the channel's enabled state,
retention mode, ACL and size; passing `-MaximumSizeMB` explicitly changes its size.
Version 0.3 keeps existing checkpoints as the historical cursors and creates
separate `state\live\<volume-name>.json` cursors automatically. No manual
checkpoint editing or temporary demo collector is needed.

If registration/copying fails, leave collection stopped, inspect the explicit
error and `.previous` backups, and retry or restore the previous executable
and resources. Installation is not a transaction across files and registration;
it never silently rolls back checkpoints or deletes history.

## Uninstall

```powershell
.\scripts\Uninstall.ps1
# Optional, destructive: clear the native log and unregister its provider.
.\scripts\Uninstall.ps1 -RemoveEventLog
```

By default only the service is stopped/removed. The native channel/provider,
resources, binaries, config and checkpoints remain so recorded events render.
Export first before `-RemoveEventLog`. **Legacy UsnJournal is never removed**
by these scripts. Application directory/file removal is deliberately manual
after service deletion. Removing checkpoints starts another retained-history
backfill, not recovery of expired history.

## Collection and durability

Each volume has a stable administrator-chosen `name` plus a local mount-point
root (`C:\`) or volume GUID root with trailing `\`. Use one entry per volume,
not multiple aliases. Checkpoints bind the resolved **volume GUID + serial +
journal ID**, not a drive letter alone. Clones duplicating all identities are
outside this guarantee. Configuration is validated at startup.

Each volume has two independent worker threads and durable cursors. On first
use the live worker saves the queried `NextUsn` as its starting cursor and a
fixed split boundary. The historical worker starts at
`max(FirstUsn, LowestValidUsn)`, or resumes the existing legacy checkpoint,
and stops at that exclusive boundary. It never emits records belonging to the
live range. Thus the first retained history is **not skipped**, but it cannot
hold new file changes behind a large backfill.

Both cursors resume after restart; the split is preserved for the same journal.
The live worker also collects retained changes missed while the service was
stopped. Journal replacement establishes a new split and explicit gap notices;
same-journal wrap preserves the split and reports lost coverage. A history
worker whose remaining range has wrapped reports a gap rather than replaying
the live range. Source errors retry per lane; sink/state failures cancel all
workers. Stop/Shutdown cancels and joins every worker.

`Historical=true` identifies records before the fixed split; `false` identifies
the live range, including any downtime backlog. Event 101 identifies
`live_start`/`live_resume` and historical coverage; 103 marks completion of the
historical range. Ingestion order interleaves old and new records. Windows
event metadata uses ingestion time; `OriginalTimestamp` preserves source time.

Memory is bounded by one 64 KiB read buffer/batch per lane per volume.
`event_interval_ms` defaults to 2 (minimum 1), `poll_interval_ms` to 200, and
`retry_interval_ms` to 10000. These limits apply independently to each lane.
OS scheduling, native channel checks and disk
overhead can make throughput substantially slower than the theoretical
500-record/second per-lane limit. Slow backfill may lose ground to journal wrap.
Live events normally arrive within seconds, but heavy sustained activity,
a large downtime backlog, or stalled Windows I/O can still delay them.
There is no unbounded queue or dropped-overflow fallback.

For your own file changes, use the **Operational channel**, not an exported
`.evtx` snapshot. Press **Refresh / F5** in Event Viewer to update its event list.
To hide historical backfill in a custom XML query, use:

```xml
<QueryList>
  <Query Id="0" Path="UsnEventLog/Operational">
    <Select Path="UsnEventLog/Operational">*[EventData[Data[@Name='Historical']='false']]</Select>
  </Query>
</QueryList>
```

Checkpoints advance only after all event writes in a batch return success,
using a flushed temporary file and atomic replacement with write-through
requested. Cancellation, crashes or persistence failures can replay an
accepted prefix; `EventIdentity` supports deduplication across both formats.
There is **no exactly-once or power-loss-proof storage guarantee**.

The native sink uses **ETW EventWrite**, not the legacy ReportEvent API.
ETW can report success when no consumer is enabled, so the collector validates
registration and checks both channel enablement and `EventEnabled` before/after
each write, treating disabled channels and nonzero write results as failures.
**Success is ETW acceptance, not acknowledgement that an event reached durable
EVTX storage.** Channel reconfiguration races, downstream Event Log failures,
buffering and retention cannot be made transactional with checkpoints.
Native Event Log full/disabled/failure behavior needs deployment acceptance
testing; do not assume every downstream loss is returned synchronously.
Forward/export important telemetry and monitor the destination independently.

Wrap, journal replacement/restamping, volume changes and invalid cursors emit
102 before rebasing. Missing-record counts are unknown. Source read/parse
errors, missing journals and disconnected volumes emit 104 and retry without
advancing their cursors. V4/range records fail closed, not silently filtered.
Sink/checkpoint failures stop collection, attempt 105 and report stderr/
nonzero SCM status. If the destination is unavailable, a health event itself
cannot be guaranteed.

## Coverage and operational limits

Only retained NTFS history exists to read. Wrapped/deleted records and activity
before journal creation or while journaling was disabled cannot be recovered.
USN reasons aggregate changes and may repeat at close. No full historical path
is guessed: `PathStatus=unresolved`, `FullPath` empty. File/parent references and
names are record data. `SecurityId` is file metadata, **not an actor**; system
PID/thread fields, when present, identify the collector.

Local NTFS V2/V3 only; no ReFS, SMB, CSV, ARM or 32-bit support. Minor extensions
use record offsets. Unsupported records/malformed lengths stop that batch.
Original UTF-16 units survive lossy display conversion.

SCM Stop/Shutdown and Ctrl+C cancel delays and pending overlapped I/O, retaining
buffers until completion. Stalled drivers, Event Log calls and disk flushes can
delay shutdown; there is no hard forced deadline.

Monitoring the volumes that hold this application's logs/checkpoints can
generate **self-traffic**. Nothing is silently filtered to conceal it. For busy
deployments, explicitly review placement on non-monitored storage and rate
limits; the installer uses standard fixed Windows locations.

The new channel defaults to **256 MiB**, overwrite-as-needed. A large backfill
can overwrite its own earliest events. Size it for expanded events and
forward/export promptly. The installer changes no unrelated log policies;
the old UsnJournal log is untouched. Checkpoints cannot detect a manually
cleared/overwritten destination. See [acceptance testing](docs/validation.md)
and [native API references](docs/references.md).
