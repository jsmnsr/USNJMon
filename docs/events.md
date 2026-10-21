# Event schema version 1

Source `UsnJournalBridge`, custom classic log `UsnJournal`. All events have
one insertion string containing a JSON object with `schema_version: 1`.
Descriptions come from `usn-eventlog-messages.dll`; keep it available to
render archived events. The resource uses literal string insertion formatting
so filenames containing `%1` are not treated as another formatting request.

| ID | Level | Meaning |
| --- | --- | --- |
| 100 | Information | One V2/V3 USN record; combined reasons preserved |
| 101 | Information | First retained coverage or restart/reconnect boundary |
| 102 | Warning | Continuity gap / identity change / invalid cursor and rebase |
| 103 | Information | Current startup/reconnect backfill boundary reached |
| 104 | Warning | Volume unavailable, query/read error, malformed/unsupported batch; retry |
| 105 | Error | Fatal configuration, checkpoint or sink failure (best effort if sink failed) |
| 106 | Information | Collector started/stopped |

IDs and payload semantics are stable in schema 1. Consumers must ignore
additional object fields and accept unknown reason bits. JSON 64-bit integers
must be decoded without IEEE-754 rounding (use a lossless decoder in JavaScript).

## Record event 100

| Field | Type / semantics |
| --- | --- |
| `event_identity` | Stable string: volume identity, 16-digit hex journal ID, decimal USN |
| `volume_identity` | Lowercase volume GUID root (without trailing slash) + hex volume serial |
| `journal_id` | Unsigned 64-bit queried journal identity |
| `historical` | Record USN below the fixed backfill boundary |
| `backfill_end` | Signed 64-bit exclusive startup/reconnect boundary |
| `path_status` / `full_path` | Always `"unresolved"` / null in this version |
| `record.major_version`, `minor_version` | Original record layout version |
| `record.record_length` | Original aligned record byte length |
| `record.file_reference`, `parent_reference` | Fixed 16/32 lowercase hex digits; little-endian ID bytes printed most-significant byte first |
| `record.usn` | Signed 64-bit source USN, not EventRecordID |
| `record.timestamp_filetime` | Original signed 64-bit count of 100 ns ticks since 1601-01-01 UTC |
| `record.reason` | All raw 32-bit reason bits, including unknown flags |
| `record.reason_names` | Array of all recognized set reason names, no `USN_REASON_` prefix |
| `record.unknown_reason_bits` | Raw bits not in the recognized name table |
| `record.source_info` | Original source flags; not process identity |
| `record.security_id` | File security metadata ID; not a SID or user attribution |
| `record.file_attributes` | Raw 32-bit file attribute mask |
| `record.filename` | JSON-escaped UTF-16-to-Unicode display name |
| `record.filename_utf16` | Original unsigned 16-bit units, including unpaired surrogates |
| `record.filename_lossy` | True when display name required replacement characters |

There is no historical full-path lookup. Old/new rename reason bits are
independent record facts, not a synthesized rename transaction. No filename,
timestamp, reason, identity or reference is inferred from the current file.

## Health/coverage

101/102 include `kind`, `volume_identity`, current `journal` (ID, first, next
and lowest valid USNs), nullable `previous_checkpoint`, `resume_usn`,
`backfill_end`, `coverage: "retained_records_only"` and
`lost_record_count: null`. Gap kinds are `volume_replaced`,
`journal_recreated_or_restamped`, `retained_history_wrapped`,
`cursor_ahead_of_journal`, and `read_cursor_invalidated`.
First use is `first_use`; a compatible restart/reconnect is `resume`.

103 includes `volume_identity`, `journal_id`, `backfill_end`, `next_usn`.
104 includes configured `volume_name`/`configured_path`, nullable `checkpoint`,
`error`, and `retry_interval_ms`. A 104 need not mean a proven loss; a later
102 records a detected discontinuity. 105 includes `error`. 106 includes
`state: "started" | "stopped"` and a `volume_count` on startup.

Metadata time is ingestion, never the source record timestamp. Health events
can repeat during retries/crash replay and do not have record deduplication
identities. A record replay may change `historical`/`backfill_end` after a
restart, but its stable `event_identity` remains unchanged.
