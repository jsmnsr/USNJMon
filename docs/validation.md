# Validation boundaries

Automated tests use **synthetic records and owned disposable files only**. They must not install a service,
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
   category IDs 1000-1099 include the known older changes, 103 reaches the initial boundary,
   and new changes have `Historical=false`. Compare raw record timestamps with
   ingestion time and inspect percent/Unicode filename rendering in Event Viewer.
3. Stop mid-backfill, restart with unchanged state, and verify cursor resume.
   Exercise a process crash and deduplicate by `event_identity`; accepted-prefix
   duplicates are expected (`EventIdentity` in schema 2). Verify malformed checkpoint files cause a visible
   fatal failure rather than implicit reset.
4. Exercise log-full/write-denied and state-disk failure cases **only on the
   disposable machine**. Confirm no checkpoint advances beyond accepted writes,
   surfaced failures stop the collector, and restart replays the uncommitted batch.
   ETW success does not guarantee destination persistence or synchronously
   report every downstream log-full/loss condition; inspect actual EVTX output.
5. Disconnect/reconnect an expendable configured volume; verify periodic 104,
   later resume or explicit 102, and continued collection of another volume.
   Wrap/recreation tests should use synthetic unit tests unless journal
   modification on that expendable volume is separately authorized.
6. Start with SCM, issue Stop and Shutdown controls during idle, backfill and
   read activity; verify stopped/failed status and safe resume. Driver/RPC stalls
   may lengthen stop time and must not cause early buffer freeing.
7. On a legacy installation, migrate while stopped using the service guide. Confirm legacy UsnJournal
   history/registration/resources and checkpoint contents remain unchanged.
   New records should resume into USNJMon/Operational with named fields,
   multiline General descriptions, correct primary IDs and complete Reasons.
   Exercise a field-name XPath query. Disable only the test native channel and
   verify collection refuses to advance; restore its prior configuration.
8. Uninstall normally and verify retained events still render. Export first,
   then test `-RemoveEventLog` and confirm legacy/unrelated logs are untouched.
9. With a large historical backlog, start USNJMon and create/modify/rename/
   delete owned fixture files after the `live_start` notice. Verify their
   `Historical=false` events arrive within a measured few-second budget while
   historical events and their cursor keep advancing. Check both state files,
   graceful SCM restart/resume, and disjoint ranges at the persisted split.
   Do not reset the existing historical cursor to demonstrate live behavior.
10. For volume selectors, compare startup discovery with known local NTFS GUIDs
    and mount aliases; include/exclude a test volume through different aliases.
    Confirm excluded volumes have no workers, no-match includes fail explicitly,
    and unreadable discovery candidates report 104. No journals should be created.
11. On an approved fixture volume, create files inside/outside configured
    whitelist/blacklist roots. Check case and component boundaries, both lanes,
    default unresolved inclusion, opt-in unresolved exclusion and 107 counts.
    Delete/rename parents and confirm no historical paths are fabricated.
    Verify all-filtered batches advance while injected summary-write failures
    retain the old checkpoint. Changing filters must not replay skipped history
    automatically. Native automated path tests open only their owned directory
    fixture by ID; they do not enumerate host journals or register/write events.

12. Convert a synthetic extracted V2/V3 stream without provider registration.
    Native queries must read every event across chunk boundaries and preserve
    Unicode names, source time, raw reasons and input provenance. Check failure
    cleanup, no-overwrite behavior, empty/truncated/unsupported input and Ctrl+C.
    On an explicitly approved disposable machine, separately install the provider
    with `-ProviderOnly`, verify General descriptions, then promote to service with
    `-Upgrade`. Confirm conversion itself never registers or writes a live channel.

Record OS/build, filesystem, toolchain, target and results without committing
private filenames, machine events or credentials. Live SCM installation,
live-channel integration, real device cancellation and failure injection are
not implied by passing mock/parser/resource-formatting or offline native-reader tests.
