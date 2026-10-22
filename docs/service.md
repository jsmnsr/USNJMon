# Continuous service

USNJMon writes named, typed records to **USNJMon/Operational** while collecting
live changes and retained history independently. It reads existing local NTFS
journals; it never creates, resizes or deletes journals.

## Install and operate

Use elevated **64-bit Windows PowerShell 5.1**, not PowerShell 7.
Review `config.example.json` first: its default is all accessible NTFS volumes.

```powershell
.\scripts\Install.ps1 -BinaryDirectory .\target\x86_64-pc-windows-msvc\release `
    -ConfigPath C:\Admin\usnjmon.json
# Omit -ConfigPath to use config.example.json.
# Installer leaves the service STOPPED with Manual startup.
Start-Service USNJMon
Get-Service USNJMon
Stop-Service USNJMon
# Optional, after acceptance testing:
Set-Service USNJMon -StartupType Automatic
```

Use `-WhatIf` to preview installation changes. The service uses LocalSystem
because opening volumes requires elevated access, but requests only
`GENERIC_READ`. It does not enable backup/manage-volume privileges, broaden
volume ACLs, or expose a network listener.

Only Administrators and SYSTEM can modify `%ProgramFiles%\USNJMon`; Users can
read its message resources. Configuration/checkpoints in
`%ProgramData%\USNJMon` are private to Administrators and SYSTEM.
The channel grants SYSTEM/Administrators full access and Event Log Readers
read access. No unrelated ACLs or log policies are changed.

Foreground diagnostics write the same real log. Stop the service first:

```powershell
& "$env:ProgramFiles\USNJMon\usnjmon.exe" foreground `
    "$env:ProgramData\USNJMon\config.json" "$env:ProgramData\USNJMon\state"
# Ctrl+C requests cancellation.
```

A state-directory lock excludes concurrent collectors/upgrades. Custom state
directories need equivalent private ACLs. `check-config <config.json>` and
`verify-resources <usnjmon-provider.dll>` do not install or collect anything.

## Volume selectors

```json
{
  "volumes": { "include": [], "exclude": ["D:\\"] },
  "path_filters": {
    "whitelist": [],
    "blacklist": [
      "\\Windows\\Logs",
      "\\Windows\\Temp",
      "\\Windows\\SoftwareDistribution\\Download",
      "\\ProgramData\\UsnEventLog",
      "\\ProgramData\\USNJMon"
    ],
    "unresolved": "include"
  },
  "poll_interval_ms": 200,
  "event_interval_ms": 2,
  "retry_interval_ms": 10000
}
```

Empty `include` selects **all discovered accessible local NTFS volumes**,
including GUID-only volumes. Omitting `volumes` does the same. Exclusion wins
across drive, directory mount-point and GUID aliases. Selectors are exact roots
(`C:`, `C:\`, or `\\?\Volume{...}\`), not patterns. A requested include that cannot
be discovered is an error. At most 128 volumes may be selected.

Discovery runs at startup, not continuously; restart to discover new volumes.
Selected volumes retry after disconnect/reconnect. Non-NTFS volumes are not
selected; inaccessible candidates report 104. No eligible volumes is an error.
Missing/inactive journals produce health events and retry, never journal creation.
Automatic checkpoint names are `volume-<GUID>`.

Legacy explicit lists preserve their existing checkpoint names:

```json
"volumes": [
  { "name": "system", "path": "C:\\" },
  { "name": "data", "path": "D:\\" }
]
```

Explicit paths must end in `\`. Do not mix forms or select aliases of the same
volume. Switching an existing explicit list to discovery changes checkpoint
filenames and can replay history; no cursor-name migration is automatic.

## Path whitelist and blacklist

Rules apply to both live and historical record events, never health/lifecycle
events. Rules are literal, volume-relative prefixes with component boundaries:
`\Users\Alice\Documents` matches that directory and descendants, but not
`\Users\Alice\DocumentsBackup`. Specific filenames also work. Comparisons use
Unicode lowercase, normalize `/` to `\`, and ignore trailing separators.
Wildcards, drive letters, UNC paths, environment expansion and `..` are rejected.

Blacklist wins. An empty whitelist permits everything not blacklisted. For
focused collection, use `"whitelist": ["\\Users\\Alice\\Documents", "\\Source"]`.
At most 256 rules/selectors are accepted. Custom arrays **replace** defaults;
`"blacklist": []` disables the five default exclusions.

Defaults reduce Windows log/temp/update-download and collector-state noise,
not all background activity. User profiles, user Temp, Recycle Bin and executable
directories are not broadly excluded. Windows Temp can contain important
activity: remove exclusions for more complete security collection. Suppression
is a deliberate coverage reduction, not a forensic guarantee.

USN records contain a name and parent ID, **not a historical full path**.
USNJMon opens the parent by file ID with zero desired access, queries its current
GUID path and appends the original name. `PathStatus=parent_current_only` labels
this candidate. Parent moves, hard links, reparse points and name reuse can make
current-path filtering differ from historical intent. The file need not still
exist, but its parent must be resolvable. No historical directory tree is invented.

Unresolvable/inaccessible paths default to **included, even with a whitelist**.
Their `FullPath` is empty and `PathStatus=unresolved:<reason>` includes errors.
Choose `"unresolved": "exclude"` only if losing those records is acceptable.
These filters are not an access-control boundary.

Event **107** reports per-batch blacklist/whitelist suppression and unresolved
counts before checkpoint advancement. All-filtered batches advance only after
the summary succeeds; counts can repeat on replay. Filter changes affect future
processing, not previously suppressed records or existing log data.
Restart after editing configuration.

## Upgrading and migrating

For an existing **USNJMon** installation, stop its service/foreground collectors,
close Event Viewer if it holds resource files open, then run:

```powershell
.\scripts\Install.ps1 -Upgrade `
    -BinaryDirectory .\target\x86_64-pc-windows-msvc\release
```

This preserves config, checkpoints, startup mode, channel enablement/retention/
ACL/size, and leaves collection stopped. `-MaximumSizeMB` explicitly changes
channel size. Replaced artifacts are backed up as `.previous`. Installation
is not transactional; on failure inspect the error/backups, keep collection
stopped, and retry or restore the previous binary/resources.

**UsnEventLog is a separate legacy installation.** The USNJMon rename uses a
new provider GUID, channel, service and directories. `-Upgrade` does not rename
or overwrite UsnEventLog. Migration is explicit:

1. Stop legacy service/foreground collection and keep its binaries, provider
   registration, logs and configuration for rollback and old-event rendering.
2. Install USNJMon fresh, passing the old configuration with `-ConfigPath`.
   Keep explicit volume names if present; do not silently broaden selection.
3. With both collectors stopped, copy the legacy `state\<name>.json` files and
   `state\live\<name>.json` files into matching paths under the new protected
   state directory. Copy only completed `.json` checkpoints, not lock/temp files.
   Preserve the new directory ACLs. Checkpoint schema 1 is unchanged.
4. Start only USNJMon. Verify resume/coverage events and retain both old and
   new channels to query history spanning migration.

Without copied checkpoints, USNJMon starts a new retained-history backfill and
can duplicate prior history. No already-written old events are automatically
reformatted. Legacy **UsnEventLog/Operational**, **UsnJournal/UsnJournalBridge**
and their installed message DLLs are never removed or re-registered by these
scripts. The renamed product no longer builds obsolete classic-log resources.

## Uninstall

```powershell
.\scripts\Uninstall.ps1
# Optional, destructive: export first, then clear and unregister the new channel.
.\scripts\Uninstall.ps1 -RemoveEventLog
```

By default only the service is stopped/deleted. Provider/channel/resources,
binary/config/checkpoints remain so old events render. File/directory removal
is deliberately manual after service deletion. Removing checkpoints causes
replay, not recovery of expired records. Legacy/unrelated logs remain untouched.

## Collection, durability and limits

Two workers per volume have independent durable cursors. At first use, live
starts at queried `NextUsn` and saves that fixed split. History starts at
`max(FirstUsn, LowestValidUsn)` or its saved checkpoint, and ends before the
split. Both resume after restart; live also collects retained downtime changes.
`Historical=true` identifies the pre-split range, not ingestion time.
Original timestamps remain separate from Windows ingestion timestamps.

Checkpoints bind volume GUID + serial + journal ID, not drive letters alone.
Duplicated clone identities are outside this guarantee. Wrap/recreation/
replacement/invalid cursors produce 102 before rebasing. A completed historical
range does not report false gaps when old records wrap. Source failures retry
per lane; sink/state failures stop all lanes. Missing-record counts are unknown.

Memory is bounded per lane, with 64 KiB journal buffers. The configured
`event_interval_ms` (minimum 1) applies independently per lane. Native checks,
path lookups, I/O and scheduling can be much slower than nominal pacing.
Backfill can lose ground to wrap; sustained live activity or downtime backlog
can delay live delivery. There is no latency SLA or unbounded overflow queue.

Checkpoints advance only after successful batch event writes, then flush and
atomic replacement with write-through requested. Cancellation/crash/save failure
can replay an accepted prefix: **at least once**, not exactly once.
`EventIdentity` supports deduplication.

The sink verifies registration, channel enablement and `EventEnabled` before/
after `EventWrite` to reject known silent-discard conditions. **ETW acceptance
is not acknowledgement of durable EVTX delivery.** Reconfiguration races,
downstream failures, buffers and retention are not transactional with checkpoints.
Forward/export important telemetry and monitor the destination independently.

The channel defaults to **256 MiB, overwrite as needed**. A large backfill can
overwrite its own earliest events. Size appropriately and forward/export promptly.
Clearing/overwriting a log is not detected by checkpoints.

Only NTFS V2/V3 records are supported; no ReFS, SMB, Cluster Shared Volumes, ARM,
32-bit or recovery of wrapped/deleted journal history. Unsupported records fail
closed. Source read/parse errors emit 104 without advancing; fatal sink/state
errors attempt 105 and return failure, but an unavailable log cannot guarantee
delivery of its own health event.

SCM Stop/Shutdown and Ctrl+C cancel waits and pending I/O, retaining buffers until
completion. Stalled drivers, synchronous path/Event Log calls or flushes can
delay shutdown. Logs/checkpoints can generate self-traffic; default filters reduce
resolvable standard-path noise, not unresolved/nonstandard-location traffic.

See [schema and queries](events.md), [acceptance checks](validation.md), and
[native API references](references.md).
