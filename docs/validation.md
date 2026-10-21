# Validation boundaries

Automated tests use **synthetic data only**. They must not install a service,
register an Event Log source, read host journals, or write real events.
`verify-resources` loads the resource DLL as data and formats messages entirely
in memory. CI does not perform privileged/live acceptance tests.

Before production, explicitly authorize a disposable Windows x64 NTFS test
machine and review this procedure. Do not use an existing host or VM without
scoped approval.

1. Confirm an existing active USN journal on an expendable test volume and
   create test files/renames/deletes **before** starting the collector. This
   application does not create a journal. Configure only the test volume.
2. Install with a sufficiently large custom log, verify private directory and
   custom-log ACLs, and run foreground. Confirm 101 starts at retained bounds,
   100 events include the known older changes, 103 reaches the initial boundary,
   and new changes have `historical: false`. Compare raw record timestamps with
   ingestion time and inspect percent/Unicode filename rendering in Event Viewer.
3. Stop mid-backfill, restart with unchanged state, and verify cursor resume.
   Exercise a process crash and deduplicate by `event_identity`; accepted-prefix
   duplicates are expected. Verify malformed checkpoint files cause a visible
   fatal failure rather than implicit reset.
4. Exercise log-full/write-denied and state-disk failure cases **only on the
   disposable machine**. Confirm no checkpoint advances beyond accepted writes,
   failures stop the collector, and restart replays the uncommitted batch.
5. Disconnect/reconnect an expendable configured volume; verify periodic 104,
   later resume or explicit 102, and continued collection of another volume.
   Wrap/recreation tests should use synthetic unit tests unless journal
   modification on that expendable volume is separately authorized.
6. Start with SCM, issue Stop and Shutdown controls during idle, backfill and
   read activity; verify stopped/failed status and safe resume. Driver/RPC stalls
   may lengthen stop time and must not cause early buffer freeing.
7. Uninstall normally and verify retained events still render. Export first,
   then test `-RemoveEventLog` and confirm unrelated logs/services are untouched.

Record OS/build, filesystem, toolchain, target and results without committing
private filenames, machine events or credentials. Live SCM installation,
Get-WinEvent integration, real device cancellation and failure injection are
not implied by passing mock/parser/resource-formatting tests.
