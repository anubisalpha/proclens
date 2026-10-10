# References

Every external page, document and project ProcLens relies on, what we use each for, and how far
each was actually checked. Much of what ProcLens reads is undocumented or only partly documented,
so this file is also the place to see **where Microsoft's documentation stops and our own testing
starts**.

**Last checked: 2026-10-10**, against Windows 10 22H2 build 19045.7725 (see
[`SPIKE_FINDINGS.md`](../SPIKE_FINDINGS.md) for how things were verified on that build).

## How to read the "Status" column

| Status | Meaning |
|---|---|
| **Read** | The page was fetched and read on the date above. The notes describe what it says. |
| **Seen** | Appeared in search results but was not fetched in full. Treat details as unconfirmed. |
| **Unchecked** | Used from general knowledge; no page was read for this. The URL, if given, is a pointer, not evidence. |

Microsoft Learn pages carry their own "last updated" metadata; it is noted where it matters,
because several of the ETW pages date from 2018-2021.

---

## 1. Kernel ETW events (the disk, file and driver-CPU views)

| Page | Status | What we rely on it for | Notes |
|---|---|---|---|
| [DiskIo_TypeGroup1](https://learn.microsoft.com/en-us/windows/win32/etw/diskio-typegroup1) | Read | Disk read/write events (types 10, 11): `TransferSize`, `IssuingThreadId`, `IrpFlags`, `HighResResponseTime` | **`HighResResponseTime` is in `KeQueryPerformanceCounter` tick units** (so latency is converted with the machine's QPC frequency, not a fixed 100 ns). `IssuingThreadId` is documented as unsupported on Windows 7 and earlier. `IrpFlags` lists the flag *names* (e.g. `IRP_PAGING_IO`); the numeric values are in the WDK headers. Page metadata dated 2018. |
| [FileIo_ReadWrite](https://learn.microsoft.com/en-us/windows/win32/etw/fileio-readwrite) | Read | File read/write requests (types 67, 68): `IrpPtr`, `FileObject`, `IoSize`, `IoFlags` | **`IoSize` is "Number of bytes requested"**, which matches what testing showed (requested size over-counts); actual bytes come from OpEnd. `IoFlags` is "IO request packet flags" with no bit values given. Shown as simplified 32-bit MOF; real fields are pointer-sized on 64-bit. Dated 2018. |
| [FileIo_OpEnd](https://learn.microsoft.com/en-us/windows/win32/etw/fileio-opend) | Read | Completion event (type 76): `IrpPtr`, `ExtraInfo`, `NtStatus` | `ExtraInfo` for a read is "the actual number of bytes that were read". Remarks: FileIo events are logged at the *beginning* of the operation and OpEnd is enabled separately; `IrpPtr` correlates begin and end. |
| [DPC class](https://learn.microsoft.com/en-us/windows/win32/etw/dpc) | Read | DPC events (types 66 ThreadDPC, 68 DPC, 69 TimerDPC): `InitialTime`, `Routine` | `InitialTime` is "DPC entry time" with a `WmiTime` qualifier; **its units/encoding are not stated**. Says to map `Routine` to a driver via Image events. See "Observed vs documented" below for a wording that disagrees with how we use it. |
| [PerfInfo class](https://learn.microsoft.com/en-us/windows/win32/etw/perfinfo) | Read | Which opcodes mean what; GUID `{ce1dbfb4-137e-4da6-87b0-3f59aa102cbc}` | Table lists 46, 51, 52, 66, 67 (ISR), 68, 69. **Opcode 50 (ISR-MSI) is not in the table**; we use it because it carries most interrupt time on this PC (see "Undocumented things we depend on"). |
| [EVENT_TRACE_PROPERTIES](https://learn.microsoft.com/en-us/windows/win32/api/evntrace/ns-evntrace-event_trace_properties) | Read | Provider flag values and which events each enables; buffer sizing; `EventsLost` and other health counters; session naming | `FILE_IO` (0x02000000) enables `FileIo_OpEnd`; `FILE_IO_INIT` (0x04000000) enables Create, DirEnum, Info, **ReadWrite**, SimpleOp; `DISK_FILE_IO` (0x200) enables `FileIo_Name` and "you must also enable `DISK_IO`"; `DPC` (0x20) and `INTERRUPT` (0x40) enable DPC and ISR. `EnableFlags` is only valid for system loggers. Buffer guidance: 64-128 KB for high event rates, 256 KB+ only for diagnostics (we use 128 KB). **Session naming: "If your component's session is already running when your component starts, your component should clean up the orphaned session rather than creating a second session"**, which is the stale-session cleanup in `trace.rs`. `EventsLost` is readable with `ControlTrace` `EVENT_TRACE_CONTROL_QUERY`. |
| [TRACE_EVENT_INFO](https://learn.microsoft.com/en-us/windows/win32/api/tdh/ns-tdh-trace_event_info) | Read | Basis for decoding events at run time; the route to discovering opcode and field names by name instead of number | Provides provider, task, opcode names, `DecodingSource` (manifest vs MOF) and a per-property array. Not yet used for discovery (only through ferrisetw's schema lookup). |
| [WPR SystemProvider keywords](https://msdn.microsoft.com/en-us/library/hh448204.aspx) | Seen | Possible alternative way to enable kernel events | Windows 8-era page. Not confirmed to cover every flag we use. |

## 2. Memory and system information

| Page | Status | What we rely on it for | Notes |
|---|---|---|---|
| [GetPerformanceInfo](https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-getperformanceinfo) | Read | Totals for the Memory view | Exists; Windows XP and later. |
| [PERFORMANCE_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-performance_information) | Read | Field meanings and units | All sizes are in **pages** (`PageSize` bytes). **`PhysicalAvailable` is "the sum of the size of the standby, free, and zero lists"**: the invariant our `memory_lists` self-test checks, so that one is documented, not just observed. `SystemCache` is "the size of the standby list plus the system working set" (so it overlaps the page lists). |
| [NtQuerySystemInformation](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntquerysysteminformation) | Read | Context for the undocumented classes we call | Page updated 2025-08-27. States the function and its structures "are internal to the operating system and subject to change from one release of Windows to another" and **recommends run-time dynamic linking (`GetProcAddress`)** so code can react if it changes. We link `ntdll` statically by name (raw-dylib), which would fail at load instead; the functions have existed for decades, but this is a known deviation. Documents only a handful of classes (basic, code integrity, exception, interrupt, KVA shadow, leap second, lookaside, performance, policy, process, basic process, processor performance, query-performance-counter, registry quota, speculation control, time of day). **`SystemMemoryListInformation` (80) and the extended handle table (64) are not among them.** |
| [RtlGetVersion](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-rtlgetversion) | Read | OS major/minor/build for profile matching | Documented as the kernel-mode equivalent of `GetVersionEx`. It does **not** say it resists manifest-based version shimming, which is why the code comment was reworded; we verified empirically that it reports build 19045, matching the registry. |
| [Windows 10 release information](https://learn.microsoft.com/en-us/windows/release-health/release-information) | Read (first ~100k characters) | Build numbers and revisions (UBR) that profiles match on | Lists version 22H2 (OS build 19045) with each cumulative update's revision, including this machine's 19045.7725 ("2026-09 B") and a later 19045.7727 ("2026-09 OOB"). Says nothing about internal structure changes. |

## 3. Pipe, security and process APIs

| Page | Status | What we rely on it for | Notes |
|---|---|---|---|
| [CreateNamedPipe](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createnamedpipea) (covers A and W) | Read | `FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS`, default security | A second instance of a pipe created with the first-instance flag fails with `ERROR_ACCESS_DENIED` (matches our squatter test). **The default pipe security descriptor grants read access to Everyone and anonymous**, which is why we set our own DACL. Overlapped mode is what allows "simultaneous read and write operations on the same pipe handle"; without it, operations "do not return until the operation is finished", the reason client and server poll with `PeekNamedPipe` from one thread. Pipe buffers come from nonpaged pool. (An earlier link ending `createnamedpipew` returned 404.) |
| [ACE strings](https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-strings) | Read | SDDL used for the pipe: `D:P(A;;GA;;;<user SID>)S:(ML;;NW;;;ME)` | Confirms `ML` (mandatory label) ACE type and `NW` (no write up). |
| [SID strings](https://learn.microsoft.com/en-us/windows/win32/secauthz/sid-strings) | Read | `ME` in the label ACE | `ME` is the Medium mandatory integrity level. |
| [GetFinalPathNameByHandleW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew) | Read | Naming files opened before the trace | `VOLUME_NAME_NT` (0x2) returns the NT device path (`\Device\HarddiskVolumeN\...`); recommends `QueryDosDevice` on each drive letter to map it back, which is what the collector does. No blocking warning on the page. |
| [GetFileType](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfiletype) | Read | Skipping non-disk handles during name resolution | **No warning about blocking on pipes.** Our worker-thread-with-timeout around handle queries rests on widely reported behaviour, not on this page. |

## 4. Not Microsoft: other sources and dependencies

| Source | Status | Used for | Notes |
|---|---|---|---|
| [Geoff Chappell, system information classes](https://www.geoffchappell.com/studies/windows/km/ntoskrnl/inc/api/ntexapi/system_information_class.htm) | Seen (search result only) | Background that the public header's class list is far from complete | Respected independent reference; not Microsoft. Not used as the source of any structure layout in the code. |
| [ferrisetw](https://github.com/n4r1b/ferrisetw) 1.2.0 | Read (source vendored and patched) | Rust ETW consumer | MIT OR Apache-2.0. One patch, in [`vendor/PATCHES.md`](../vendor/PATCHES.md): raw QPC timestamps. |
| [windows-rs](https://github.com/microsoft/windows-rs) (`windows` crate 0.62) | Unchecked | Win32 bindings | MIT OR Apache-2.0 (licence metadata of the crate; not re-read). |
| [sysinfo](https://crates.io/crates/sysinfo) 0.39 | Unchecked | Process list | Crate metadata only. |
| [Tauri 2](https://tauri.app), React 19, Vite 7, Vitest 3 | Unchecked | Desktop shell and UI | Versions in `Cargo.lock` / `app/package-lock.json`. Licences not re-read. |

---

## Observed vs documented

Places where our testing and a Microsoft page do not line up, or where a page is silent. The
profile and self-tests, not the documentation, are what protect us here.

1. **DPC/ISR timing.** The DPC page says "These events are logged when a DPC is entered" and that
   the entry time stamp "is used to compute the duration". We compute duration as
   *event time minus `InitialTime`*, which only works if the event is stamped at the *end* of the
   routine. Measured: 0 negative durations, typical averages of 3-16 us, and totals within about
   14-27% (DPC) and 27% (ISR) of Windows' own `% DPC Time` / `% Interrupt Time` counters. So the
   "logged on entry" wording looks imprecise, but the units of `InitialTime` remain undocumented
   and our reading rests on those measurements.
2. **`FileIo_Name` events.** `EVENT_TRACE_FLAG_DISK_FILE_IO` is documented to enable them (with
   `DISK_IO`), and we enable both. `collector discover` shows that only two of the four name-bearing
   types arrive live: FileCreate (32) and FileDelete (35), with parsable `FileName`s, and only for
   files created or deleted during the trace. Name (0) and FileRundown (36) were never observed, so
   names for files already open come from Create events and the system handle table. Why 0 and 36
   are silent is unexplained; "rundown happens at stop" is a guess we did not check.
3. **`IoSize`.** Documented as "requested"; confirmed by testing (827 MB requested vs 738 MB
   transferred in one run).
4. **Paging I/O.** The page names `IRP_PAGING_IO` but gives no value. We use `0x2 | 0x40`
   (`IRP_PAGING_IO | IRP_SYNCHRONOUS_PAGING_IO`), and `collector discover` now derives that mask
   from behaviour instead of from headers: flush writes carry `IoFlags 0x60043`; ordinary data
   writes carry `0x0` or `0x60a00`; an unbuffered control write carries `0x60a01`. Removing what
   ordinary and unbuffered writes carry leaves exactly `0x42`. (`0x1` is `IRP_NOCACHE`, shared with
   the unbuffered control, so it is not a paging marker.) A flush made a 64 MiB write appear as
   128 MiB until paging requests were excluded. Unexplained: four 1 MiB writes with flags `0x60a00`
   are attributed to the cached-write step rather than the unbuffered one.

## Undocumented things we depend on (no Microsoft page found)

Each is guarded by a runtime check, so a different build shows up as a failed check on the This PC tab
and as a warning on the affected view (the memory breakdown also turns itself off), instead of going
unnoticed.

| Dependency | Where | Source of our knowledge | Guard |
|---|---|---|---|
| `NtQuerySystemInformation(80)` memory-list structure and `SeProfileSingleProcessPrivilege` requirement | `collector/src/mem.rs` | General knowledge of community-maintained NT headers; **no specific source was checked for this work** | `PhysicalAvailable == free + zero + standby` (documented relationship), checked per sample |
| `NtQuerySystemInformation(64)` extended handle table entry layout (x64) | `collector/src/files.rs` | Same as above | Self-test finds the collector's own handle and resolves it to its own path |
| PerfInfo opcode 50 (ISR-MSI) with the same fields as ISR | profile `perf.isr_opcodes` | Observed in live traces; absent from Microsoft's table | ISR total vs Windows' interrupt-time counter; self-test parses it |
| Event timestamps as raw QPC when `PROCESS_TRACE_MODE_RAW_TIMESTAMP` is requested | `vendor/ferrisetw` patch | Microsoft's `EVENT_HEADER` documentation describes this mode in general (page not re-read for this work); effect verified by 0 negative durations | Self-test fails on negative durations |
| `FileObject` addresses are recycled after Close | `trace.rs` | Found by testing (names attached to the wrong files) | Names resolved at event time; mapping dropped on Close |

## Used from general knowledge (pages not checked)

APIs used where no page was read for this work. Behaviour was exercised by the tests and
verification runs described in `SPIKE_FINDINGS.md`, but treat the documentation as unverified:

`OpenProcessToken`, `AdjustTokenPrivileges`, `LookupPrivilegeValueW`, `GetTokenInformation`
(`TokenElevation`, `TokenUser`), `ConvertSidToStringSidW`,
`ConvertStringSecurityDescriptorToSecurityDescriptorW`, `ConnectNamedPipe`, `PeekNamedPipe`,
`GetNamedPipeServerProcessId`, `OpenProcess`, `DuplicateHandle`, `QueryFullProcessImageNameW`,
`ShellExecuteExW` (`runas`), `EnumDeviceDrivers`, `GetDeviceDriverBaseNameW`, `QueryDosDeviceW`
(only mentioned by the `GetFinalPathNameByHandleW` page), `QueryPerformanceFrequency`,
`RegGetValueW`, `ControlTraceW`, `StartTrace`/`CloseTrace` (via ferrisetw), WebView2's remote
debugging port (test tooling only).

## Maintaining this file

- Add a row whenever code starts to depend on a new page, API or structure, with the status
  honestly set. Don't upgrade "Seen" or "Unchecked" to "Read" without reading the page.
- Re-read the Microsoft event pages when adding a profile for a new Windows build, and note any
  per-version remarks they contain (the DiskIo page lists differences all the way back to
  Windows 2000).
- Microsoft Learn URLs move; if one 404s, search for the page title and update the link and the
  date above.
- Prefer linking to the live page over copying its text here; quotes are kept short on purpose.
