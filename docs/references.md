# Native API decisions

Implementation was checked against these Microsoft references:

- [FSCTL_READ_USN_JOURNAL](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal):
  local volume handles and returned USN buffer.
- [USN_JOURNAL_DATA_V0](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_journal_data_v0)
  and [V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_journal_data_v2):
  first/lowest-valid/next bounds, journal identity, advertised record versions.
- [READ_USN_JOURNAL_DATA_V1](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v1):
  all-reason mask, `ReturnOnlyOnClose=0`, `BytesToWaitFor=0`, next-cursor prefix.
  Requests include all advertised major versions from 2 upward so unsupported
  records are reported, not silently excluded by a version filter.
- [USN_RECORD_V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2)
  and [V3](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v3):
  variable record lengths, 64-bit alignment, filename byte offsets, combined
  reasons, 64/128-bit file IDs, timestamps and minor-version compatibility.
- [CancelIoEx](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex):
  cancellation is not completion; retain buffers until overlapped I/O drains.
- [ReportEventW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-reporteventw):
  write failures, insertion size limits, literal string insertion formatting and
  Windows-generated event timestamps.
  The resource uses `%1!s!` (wide strings for `FormatMessageW`); in-memory
  Windows tests check that Unicode and percent sequences survive insertion.
- [Event sources](https://learn.microsoft.com/en-us/windows/win32/eventlog/event-sources)
  and [reporting events](https://learn.microsoft.com/en-us/windows/win32/eventlog/reporting-an-event):
  custom source registration and message resources; avoid Application fallback.
  Microsoft recommends modern manifest providers for new applications; this
  implementation explicitly uses the supported classic API for synchronous
  write errors and accepts its unnamed-field limitation.
- [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw):
  same-volume replacement and requested write-through; not a cross-system
  durable transaction with Event Log.
- [Service state transitions](https://learn.microsoft.com/en-us/windows/win32/services/service-status-transitions):
  SCM control handling and reporting terminal status.
