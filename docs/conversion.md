# Convert an extracted journal

```powershell
.\usnjmon.exe convert .\journal.usnj .\journal.evtx
```

The input must be the extracted binary **`$UsnJrnl:$J` attribute stream**,
either with its zero-filled sparse prefix or with that prefix removed. The
extension is not significant. Acquisition is outside USNJMon's scope: use an
appropriate trusted acquisition tool and retain the original evidence.
This command does not open a host volume, acquire a journal, parse a disk image,
or accept `fsutil` text, CSV, XML, or EVTX as input.

## Output and viewing

The result is a real, standalone EVTX file readable by Windows Event Viewer,
`Get-WinEvent`, and native Windows Event Log queries. It contains the same
record categories and named fields as the service, plus input provenance.
It does not write imported records into a live channel or install anything.
Administrator rights are unnecessary if you can read the input and write the
output directory.

```powershell
Get-WinEvent -Path .\journal.evtx -Oldest -FilterXPath '*[System[EventID=1000]]'
```

In Event Viewer, choose **Open Saved Log**, then inspect **Details > XML View**.
`TimeCreated` is conversion time; **`OriginalTimestamp` is the original USN time**.
The file is static and will not follow subsequent filesystem activity.

Without registered USNJMon resources, Windows may show a missing-description
notice in **General** and `Get-WinEvent.Message` may be empty. The records and
named Details fields remain readable. For General descriptions, optionally
install just the provider resources in elevated Windows PowerShell 5.1:

```powershell
.\scripts\Install.ps1 -ProviderOnly `
    -BinaryDirectory .\target\x86_64-pc-windows-msvc\release
```

This is a persistent, explicit installation: it copies resources/configuration
into the protected USNJMon directories and registers **USNJMon/Operational**,
but creates or starts no service. It is not needed for conversion. It must be
performed on the viewing computer; copying only an EVTX does not copy provider
resources. To add the stopped service later, run the same installer with
`-Upgrade` and without `-ProviderOnly`.

## What the input does and does not establish

An extracted `$J` does not contain a trustworthy volume identity, journal ID,
computer name, live/backfill split, or historical directory tree.
USNJMon never resolves its paths against the analysis computer.

| Field | Offline meaning |
| --- | --- |
| `SourceKind` | `extracted_usn_stream` |
| `InputSha256` | SHA-256 of the complete input, including any zero padding |
| `InputOffset` | Record byte offset within that input file |
| `EventIdentity` | `sha256:<input-hash>:<decimal-USN>` |
| `JournalIdentityStatus` | `not_present_in_input` |
| `VolumeIdentity` | `unknown:offline_input` |
| `JournalId`, `BackfillEnd` | Unavailable placeholders, **not real identities/bounds**: `0` |
| `Historical` | `true` for all imported records, not a service split classification |
| `PathStatus`, `FullPath` | `unresolved:offline_input`, empty |
| System `Computer` | `offline-conversion`, not the source computer |

All original name units, reason bits, references, attributes, record versions,
USNs and timestamps are preserved. Offline events use typed Binary XML template
substitutions matching the service's scalar/FILETIME types; `InputOffset` is
UInt64. Use named XML fields as documented in the [schema](events.md) rather
than depending on positional properties.

Deduplication identities are stable for the **same input bytes**. Extracting the
same records with different padding changes the input hash, so identities do
not automatically deduplicate overlapping acquisitions or service output.
Only record events are emitted; conversion has no live-service health events.

## Safety and limits

- V2/V3 records and 8-byte-aligned zero padding are supported. Record bounds,
  versions, filenames and increasing USNs are checked with the service parser.
  Unsupported/V4 records, truncation, nonzero corruption and reordered records
  fail explicitly. This is not a carving/recovery tool; nothing is silently skipped
  except zero padding. Empty/padding-only input is an error.
- Memory is bounded by fixed-size streaming buffers and one 64 KiB EVTX chunk.
  The input is hashed first, then parsed, so even sparse files require two
  sequential passes over their logical bytes. Large EVTX output can be much larger
  than the source; ensure enough destination space.
- The read-only input handle denies write/delete sharing during conversion.
  Existing writers cause an explicit open failure, not an inconsistent snapshot.
- The destination directory must exist. Output is staged beside the destination,
  flushed, closed and published without replacement. Existing output, malformed
  input, I/O failure or cooperative Ctrl+C never publishes a partial EVTX.
  An abrupt process kill or power failure can leave an application-owned
  `.usnjmon-<pid>-<nonce>.partial` file; remove that specific file after confirming
  its converter is no longer running.
- No records lost before acquisition can be recovered. USN timestamps are source
  data, not independent evidence of clock accuracy; no process or user attribution
  is invented.
