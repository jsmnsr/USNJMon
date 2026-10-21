# USN Event Log Bridge

An independent **Rust Windows x64 service** that reads existing local NTFS USN
journals, backfills their oldest readable retained records, then follows new
changes. It writes structured telemetry to the **UsnJournal** custom classic
Windows Event Log using source **UsnJournalBridge**. It does not depend on Sysmon
or any other project and does not impersonate a Microsoft provider.

This is a first implementation, not a forensic recovery tool or a complete
Sysmon replacement. USN records **do not identify the responsible process,
PID, user, command line, or file contents**. No process/user attribution is
inferred. The API calls only query/read journals; they never create, resize,
delete, or change them. No VSS or raw-disk recovery is used.

## Build and non-invasive validation

Supported targets: `x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu`, Windows
10/11 or Server 2016+ x64. Install Rust **1.99.1** with rustfmt and the Windows
10/11 SDK (`mc.exe`, `rc.exe`). MSVC additionally needs Visual Studio C++ Build
Tools; GNU needs MinGW GNU binutils (including `dlltool` and its assembler) or
LLVM's `llvm-dlltool` configured through `RUSTFLAGS=-C dlltool=...`. SDK tools
are discovered by the build script wrapper; alternatively set `WINDOWS_SDK_BIN`
to the SDK's `bin\<version>\x64` directory.

```powershell
rustup toolchain install 1.99.1 --profile minimal --component rustfmt
rustup override set 1.99.1
rustup target add x86_64-pc-windows-msvc
.\scripts\Build.ps1
# Or: .\scripts\Build.ps1 -Target x86_64-pc-windows-gnu
```

The lockfile pins dependencies. `Build.ps1` checks formatting, runs synthetic
tests, builds release binaries, and uses `LoadLibraryExW`/`FormatMessageW` to
verify all message IDs (including percent characters in payloads). It does
**not** register a source, install/start a service, read any live journal, or
write events. Both artifacts in `target\<target>\release` are needed:
`usn-eventlog.exe` and the resource-only `usn-eventlog-messages.dll`. CI runs
these same checks. No release is published automatically.

Tests cover V2/V3 synthetic record parsing, combined/unknown reasons, malformed
bounds/alignment/versions, invalid UTF-16 preservation, retained bounds,
restart, wrap/recreation/volume replacement, sink/checkpoint failures,
at-least-once replay identity, cancellation, and message resource rendering.
Mock control-handler tests do not prove real SCM or disk-driver behavior.
Live service integration requires an explicitly approved disposable Windows
test machine; see [validation](docs/validation.md).

## Install, operate, uninstall

Installation is an explicit **persistent host change**. Use an elevated
**64-bit Windows PowerShell 5.1**, not PowerShell 7, for installation/removal.
Review `config.example.json`, replace its volume selection, and save your
configuration outside the repository. Each volume needs an existing active
USN journal. Missing/inactive journals produce health warnings and retries,
not journal creation.

```powershell
# Does not start the service. -WhatIf previews registration/directory changes.
.\scripts\Install.ps1 -BinaryDirectory .\target\x86_64-pc-windows-msvc\release `
    -ConfigPath C:\Admin\usn-config.json
Start-Service UsnEventLog
Get-Service UsnEventLog
Stop-Service UsnEventLog
# Only after acceptance testing, if automatic startup is desired:
Set-Service UsnEventLog -StartupType Automatic
```

The service is installed with **Manual** startup and LocalSystem because
opening a volume for USN reads requires elevated volume access. It requests
only `GENERIC_READ` and never enables backup/manage-volume privileges or
broadens volume ACLs. This is not an unprivileged service. There is no network
listener. Binaries reside in `%ProgramFiles%\UsnEventLog`; only Administrators
and SYSTEM can modify them, while Users can read the message resource.
Configuration and state under `%ProgramData%\UsnEventLog` are accessible only
to SYSTEM and Administrators. The log ACL allows SYSTEM/Administrators full
access and Event Log Readers read access. These ACLs apply only to this
application's newly created directories and log.

Foreground collection writes the **same real Event Log** and needs the
installed registration and message DLL. Stop the service first; a shared
state-directory lock prevents concurrent writers:

```powershell
& "$env:ProgramFiles\UsnEventLog\usn-eventlog.exe" foreground `
    "$env:ProgramData\UsnEventLog\config.json" "$env:ProgramData\UsnEventLog\state"
# Ctrl+C requests cancellation.
```

Use `check-config <path>` and `verify-resources <dll>` for non-invasive checks.
The collector refuses an absent/mismatched registration instead of silently
falling back to the Application log. Never place executable/config/state paths
in locations writable by untrusted users. Custom foreground state directories
must be provisioned with equivalent ACLs by an administrator.

For an upgrade: stop the service, back up configuration/checkpoints, replace
both installed artifacts as Administrator, run `verify-resources`, and start
explicitly. Do not change configured volume `name` values unless intentionally
starting fresh history for those entries.

```powershell
.\scripts\Uninstall.ps1
# Optional, DESTRUCTIVE: deletes the custom log and its source, not other logs.
.\scripts\Uninstall.ps1 -RemoveEventLog
```

Default uninstall stops/removes only the service. It retains the log, source,
message DLL, binaries, config and checkpoints to preserve old event rendering
and evidence. Export/archive the log before `-RemoveEventLog` if needed.
Directory/file removal is deliberately manual and limited to the two exact
application directories after service deletion. A fresh reinstall requires
those directories and this application's source/log to be absent. Deleting
checkpoints causes another retained-history backfill (and duplicates), not
recovery of expired history. The installer refuses existing artifacts rather
than taking ownership of unknown directories; a partial installation must be
inspected and cleaned up explicitly before retrying.

## Collection and durability

Each configured volume has a stable administrator-chosen `name` and a local
mount-point root (for example `C:\`) or volume GUID root with trailing `\`.
Use one entry per physical volume; do not configure multiple aliases for the
same volume. A renamed/reassigned mount point does not define identity: the
checkpoint contains the resolved **volume GUID + volume serial + journal ID**.
Clones that duplicate all identities are outside this identity guarantee.
Configuration is read only on startup; unknown keys and invalid values fail.

First use starts at `max(FirstUsn, LowestValidUsn)`, not the current tail.
Subsequent starts use the saved next USN. The queried `NextUsn` at startup or
reconnect is a fixed historical/live boundary, persisted in the checkpoint.
Records below it are marked `historical: true`; later records are live relative
to that boundary, not necessarily recent by wall clock. Event 101 records
coverage; event 103 marks reaching that boundary. The record's original
FILETIME is payload data; Windows supplies **ingestion time** in event metadata.
No backdating occurs.

Query/read buffers are bounded at 64 KiB; each volume gets one batch per
round, with cancellation checks between records and interruptible delays.
`event_interval_ms` (default 2, minimum 1) caps record emission at roughly 500
events/second globally before other overhead. `poll_interval_ms` defaults to
200; failed volumes retry every `retry_interval_ms` (default 10000). Slow
backfill can lose ground to journal wrap. There is no unbounded queue or
silently dropped overflow. Configure fewer volumes or adjust the documented
rates based on source churn and log capacity.

Only successful `ReportEventW` calls permit checkpoint advancement. A batch
is committed after all its writes succeed. Checkpoints use a flushed same-
directory temporary file and atomic replacement with write-through requested.
On a crash, cancellation mid-batch, or checkpoint failure, an accepted batch
prefix can replay. Stable `event_identity` values support deduplication.
**This is at-least-once replay behavior, not exactly-once delivery or a
power-loss-proof evidence store.** Event Log API acceptance does not guarantee
physical storage durability; hardware, OS buffering, corruption, administrative
clearing and retention can still lose data. There is no distributed transaction
between a checkpoint and the Windows Event Log.

Wrap, journal restamping/recreation, volume replacement and an invalid cursor
produce gap event 102 **before** resetting to the oldest available bound.
The number of missing records is unknown; USN offsets are not counts.
Disappearing volumes and query/read/parser failures emit event 104 and retain
their checkpoint while other volumes continue. Unsupported major versions
(including V4 range records) fail closed at the affected batch, with health
warnings; they are not interpreted as V2/V3 or silently skipped. Persistent
failures need operator attention. Event Log/checkpoint failures stop the whole
collector with a nonzero service exit code, attempting event 105 and writing
stderr in foreground. If Event Log itself is unavailable, a health event
cannot be guaranteed; inspect SCM status and foreground diagnostics.

## Coverage and limits

Only already-retained journal records can be read. Wrapped/deleted records,
changes before journal creation, and activity while journaling was disabled
cannot be reconstructed. A USN journal is an aggregate change record, not a
complete ordered stream of every write. Reason bits can combine and repeat
at close. Rename old/new records are emitted independently without invented
pairing. Filename and parent ID are historical record data; **full paths are
not resolved** (`path_status: "unresolved"`). Deleted files, hard links and
renamed parents make retrospective paths unreliable.

Local NTFS only: no ReFS/SMB/CSV, remote volumes, 32-bit or ARM builds. V2/V3
records are parsed; minor-version extensions use on-record offsets. There is
no claim of support for V4 extent/range records. Maximum NTFS filename size
is enforced, original UTF-16 units are preserved, and lossy display conversion
is labeled. SecurityId is file security-descriptor metadata, **not an actor**.
System event process/thread metadata, if exposed, refers to the collector.

SCM Stop/Shutdown and console Ctrl+C cancel delays and outstanding overlapped
volume I/O; buffers remain alive until cancelled I/O completes. Unresponsive
drivers, synchronous Event Log RPC calls and disk flushes can delay shutdown;
there is no false hard shutdown deadline or forced unsafe buffer release.
Real SCM/driver integration has to be validated on the deployment environment.

The collector can observe **its own checkpoint and Event Log writes** if those
files reside on monitored volumes. This can sustain extra journal traffic;
no records are silently filtered to hide that feedback. For high-volume
deployments, place the custom log/state on a separate non-monitored storage
volume through an explicitly reviewed deployment configuration, or account
for the feedback and rate cap. The provided installer uses standard fixed
Windows locations and does not relocate system logs.

## Event Log capacity and querying

The installer configures only `UsnJournal`: default **256 MiB**, overwrite-
as-needed, configurable with `-MaximumSizeMB`. A long backfill can overwrite
its own oldest events before finishing. Size the log for measured expanded
event size, forward events to durable storage, or export and archive frequently.
No settings for Application, System, Security or any other log are changed.
If you manually select a non-overwriting policy and the log fills, collection
stops on write failure; it does not drop records to keep going. Checkpoints
do not detect a manually cleared/overwritten destination log.

Event Viewer: **Applications and Services Logs > UsnJournal**. Filter natively
by source, Event ID, level and ingestion time. The classic API was deliberately
chosen for synchronous write-error reporting. It has one JSON insertion string
(`Properties[0]` / unnamed XML `EventData/Data`), **not manifest-named XML fields**.
Parse that JSON for field-level queries; this is not an ETW analytic channel.

```powershell
Get-WinEvent -FilterHashtable @{LogName='UsnJournal'; Id=100} -MaxEvents 100 |
    ForEach-Object { $_.Properties[0].Value | ConvertFrom-Json } |
    Where-Object { $_.record.reason_names -contains 'FILE_DELETE' } |
    Select-Object event_identity, historical, @{n='Name';e={$_.record.filename}}
```

See [event schema](docs/events.md) and [Microsoft API references](docs/references.md).
