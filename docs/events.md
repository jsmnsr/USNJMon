# Native event schema 2

Provider **UsnEventLog**, GUID `{4FBEAC1D-5FE8-44C1-AB4E-A06A645BF2D8}`, channel
**UsnEventLog/Operational**. Events have descriptor version 2 and
`SchemaVersion=2`. `resources/provider.man` is the native field/type/order
contract. `usn-eventlog-provider.dll` supplies metadata and multiline messages.
Keep provider registration/resources available for archived-event rendering.

## Event IDs

| ID | Level | Meaning |
| --- | --- | --- |
| 1000 | Information | File/directory created |
| 1001 | Information | File/directory deleted |
| 1002 | Information | Rename - old name |
| 1003 | Information | Rename - new name |
| 1010 | Information | Data changed (including named data and transactions) |
| 1020 | Information | Security changed |
| 1030 | Information | Attributes/metadata changed |
| 1099 | Information | Other, including close-only/unknown reasons |
| 101 | Information | First retained coverage or restart/reconnect boundary |
| 102 | Warning | Continuity gap / identity change / invalid cursor and rebase |
| 103 | Information | Startup/reconnect backfill boundary reached |
| 104 | Warning | Source read/parse failure, including unsupported records; retry |
| 105 | Error | Fatal collector failure (best effort if destination failed) |
| 106 | Information | Collector started/stopped |

Exactly one category is assigned per USN record. Priority is delete,
rename-new, rename-old, create, security, data, metadata, other. For example,
CREATE+DELETE+CLOSE is **1001**, but `Reasons` includes all three and
`ReasonFlags` preserves every bit. Do not interpret the primary ID as the only
change or sum categories as counts of distinct filesystem operations.
Rename records are not paired/invented. Unknown bits are never discarded.

## Record fields (all eight record IDs)

| Field | Native type / semantics |
| --- | --- |
| `SchemaVersion` | UInt32, 2 |
| `Category` | UnicodeString, readable primary category |
| `FileName` | UnicodeString, display name; control characters escaped |
| `ObjectType` | UnicodeString, File or Directory from raw attributes |
| `OriginalTimestamp` | FILETIME, original UTC source timestamp (100 ns ticks since 1601) |
| `Historical` | Boolean, USN below fixed startup/reconnect backfill boundary |
| `Reasons` | UnicodeString, recognized reason names separated by ` \| ` |
| `ReasonFlags`, `UnknownReasonBits` | UInt32, rendered in hex; all reasons / unrecognized bits |
| `Usn` | Int64, source cursor; not the destination EventRecordID |
| `JournalId` | UInt64 rendered in hex, original journal identity |
| `VolumeIdentity` | UnicodeString, lowercase volume GUID + hex serial |
| `FileReference`, `ParentReference` | UnicodeString, 16/32 lowercase hex digits; most-significant byte first |
| `FileAttributes`, `SourceInfo` | UInt32 rendered in hex, raw metadata/flags |
| `SecurityId` | UInt32, file security metadata ID, not a user SID |
| `MajorVersion`, `MinorVersion` | UInt16, source record layout version |
| `RecordLength` | UInt32, original aligned record byte length |
| `BackfillEnd` | Int64, exclusive startup/reconnect boundary |
| `PathStatus`, `FullPath` | UnicodeString, `unresolved` / empty in this version |
| `FileNameUtf16Hex` | UnicodeString, space-separated four-digit original UTF-16 units |
| `FileNameLossy` | Boolean, display Unicode needed replacement characters |
| `FileNameDisplayEscaped` | Boolean, display control characters were escaped |
| `EventIdentity` | UnicodeString, volume identity + hex journal ID + decimal USN |

The raw 64-bit FILETIME is preserved by the native field. Event XML renders it
as a UTC timestamp. Destination `TimeCreated` is ingestion time, not backdated
source time. Embedded NUL/control characters cannot truncate or add lines to
the display filename; original units remain lossless in `FileNameUtf16Hex`.
Hex output types format the General description; Event XML can serialize the
same unsigned values as decimal. Parse numeric fields numerically rather than
requiring a `0x` prefix.

## Health/coverage fields

101/102: `SchemaVersion`, `Kind`, `VolumeIdentity`, `JournalId`, `FirstUsn`,
`LowestValidUsn`, `NextUsn`, `ResumeUsn`, `BackfillEnd`,
`HasPreviousCheckpoint`, `PreviousVolumeIdentity`, `PreviousJournalId`,
`PreviousNextUsn`, `Coverage`, `LostRecordCount`.

Gap kinds: `volume_replaced`, `journal_recreated_or_restamped`,
`retained_history_wrapped`, `cursor_ahead_of_journal`,
`read_cursor_invalidated`. Other coverage kinds: `first_use`, `resume`.
Missing counts are `"unknown"`: USN offsets are not record counts. Absent
previous checkpoint fields are empty/zero **only when the presence flag is
false**, not real zero identities.

103: `SchemaVersion`, `VolumeIdentity`, `JournalId`, `BackfillEnd`, `NextUsn`.

104: `SchemaVersion`, `VolumeName`, `ConfiguredPath`, `Error`, `RetryIntervalMs`,
`HasCheckpoint`, `VolumeIdentity`, `JournalId`, `NextUsn`. An unavailable
checkpoint uses empty/zero fields with `HasCheckpoint=false`. A 104 does not
prove lost history; a subsequent 102 reports a detected gap.

105: `SchemaVersion`, `Error`.
106: `SchemaVersion`, `State` (`started`/`stopped`), `VolumeCount`.

Health events can repeat on retry/replay and have no record deduplication
identity. A replay may have a different historical boundary, but the same
record retains its `EventIdentity`.

## Event Viewer / PowerShell queries

Event Viewer: **Applications and Services Logs > UsnEventLog > Operational**.
Filter by ID in the normal filter UI. The Details XML contains fields such as
`<Data Name="FileName">example.txt</Data>`, not a JSON insertion.

```powershell
Get-WinEvent -FilterHashtable @{LogName='UsnEventLog/Operational'; Id=1001} -MaxEvents 10 |
    Format-List TimeCreated, Id, Message

# Native field filter, usable in Event Viewer's XML query editor as well:
$filter = "*[System[EventID=1001]] and *[EventData[Data[@Name='FileName']='example.txt']]"
Get-WinEvent -LogName 'UsnEventLog/Operational' -FilterXPath $filter

# Extract names without parsing JSON or depending on property positions:
Get-WinEvent -LogName 'UsnEventLog/Operational' -MaxEvents 20 | ForEach-Object {
    [xml]$xml = $_.ToXml()
    $fields = @{}
    foreach ($data in $xml.Event.EventData.Data) { $fields[$data.Name] = $data.InnerText }
    [pscustomobject]@{ Id=$_.Id; FileName=$fields.FileName; Reasons=$fields.Reasons; Usn=$fields.Usn }
}
```

## Legacy schema 1

Existing **UsnJournal / UsnJournalBridge** records remain JSON in
`Properties[0]`, with record ID 100 and health IDs 101-106. Their DLL remains
`usn-eventlog-messages.dll`. The native provider uses a **different channel and
provider identity**, so old IDs/messages are never reinterpreted.

No historical conversion/replay is automatic. Checkpoints remain schema 1 and
are reused across the upgrade. Correlate legacy `event_identity` with native
`EventIdentity`; raw journal identities, reason flags and timestamps are
preserved across formats. Do not parse native `Properties[0]` as JSON: it is
now the numeric schema version. Consumers should select provider/channel and
schema version explicitly.
