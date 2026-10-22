# Native API decisions

Implementation was checked against these Microsoft references:

- [FindFirstVolumeW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findfirstvolumew),
  FindNextVolumeW and FindVolumeClose enumerate volume GUIDs (not drive-letter
  ordering). GetVolumeInformationW limits discovery to NTFS;
  GetVolumePathNamesForVolumeNameW supplies mount aliases.
- [OpenFileById](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-openfilebyid):
  zero desired access for parent metadata, full sharing and backup-semantics
  directory flag; no privilege enablement or file-content read.
- [GetFinalPathNameByHandleW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew):
  VOLUME_NAME_GUID works without drive letters. Results are current paths,
  not USN historical reconstruction; errors are labeled as unresolved.
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
- [DeviceIoControl](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-deviceiocontrol):
  use `GetOverlappedResult` for completed byte counts on overlapped handles,
  including calls that finish immediately. A non-invasive Windows test queries
  compression metadata on its own disposable file, not a journal or volume.
- [ReportEventW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-reporteventw) (legacy):
  write failures, insertion size limits, literal string insertion formatting and
  Windows-generated event timestamps.
  The resource uses `%1!s!` (wide strings for `FormatMessageW`); in-memory
  Windows tests check that Unicode and percent sequences survive insertion.
- [Event sources](https://learn.microsoft.com/en-us/windows/win32/eventlog/event-sources)
  and [reporting events](https://learn.microsoft.com/en-us/windows/win32/eventlog/reporting-an-event):
  custom source registration and message resources; avoid Application fallback.
  Legacy log registrations/resources are retained for old event rendering.
- [Instrumentation manifests](https://learn.microsoft.com/en-us/windows/win32/wes/writing-an-instrumentation-manifest)
  and [event templates](https://learn.microsoft.com/en-us/windows/win32/wes/defining-event-data-templates):
  modern provider identity, native named types and exact binary field order.
- [Channels](https://learn.microsoft.com/en-us/windows/win32/wes/defining-channels):
  a distinct operational channel, channel-specific descriptor keyword and ACL.
- [EventWrite](https://learn.microsoft.com/en-us/windows/win32/api/evntprov/nf-evntprov-eventwrite):
  events are ETW-accepted, not synchronously acknowledged as persisted EVTX.
  Success also includes no enabled consumers, so enablement is checked
  before/after emission and every nonzero write status fails collection.
- [Channel properties](https://learn.microsoft.com/en-us/windows/win32/api/winevt/nf-winevt-evtgetchannelconfigproperty):
  explicit channel enablement check in addition to provider EventEnabled;
  another ETW consumer alone must not mask a disabled destination channel.
- [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw):
  same-volume replacement and requested write-through; not a cross-system
  durable transaction with Event Log.
- [Service state transitions](https://learn.microsoft.com/en-us/windows/win32/services/service-status-transitions):
  SCM control handling and reporting terminal status.
