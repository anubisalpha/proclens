# ProcLens findings (updated 2026-10-10)

Everything below was observed on **one machine**: Windows 10 Home 22H2, build **19045.7725**, AMD64,
Intel i7-2600K (8 logical CPUs), 32 GB RAM, 2 SATA SSDs + 3 USB disks, NVIDIA GPU. Rust 1.97.1,
`ferrisetw` 1.2.0 (vendored, patched), `windows` 0.62, `sysinfo` 0.39. Treat it as "worked here",
not "works on every Windows box". That is why per-build profiles exist (see README).

## Decisions made
- **Name:** ProcLens. **Stack:** Tauri 2 + Rust + React.
- **Elevation:** collector elevated, UI not. v1 = collector launched on demand via UAC; a Windows
  service is a possible later swap (the pipe interface doesn't assume how the collector started).
- **Collector is read-only**; the only mutating request is `Shutdown` (stops the collector itself).
- **Per-build profiles + runtime self-tests** (user request): support is tracked per exact Windows
  build and patch level, and a failed self-test switches a feature off instead of showing wrong data.
- Kernel driver (hidden-process detection) deferred; we only see what Windows itself reports.

## What is verified
| Capability | How | Evidence |
|---|---|---|
| Memory breakdown | `GetPerformanceInfo` + `NtQuerySystemInformation(80)` (elevated) | free+zero+standby == `PhysicalAvailable` within 1 MB. Now also enforced per sample (see below). |
| Process list | `sysinfo` | 277-284 processes |
| Per-process disk I/O | ETW `DiskIo` + thread-to-pid map | ETW write total (~30 MB) agreed with Windows' PhysicalDisk counter (~30 MB) |
| Per-process file I/O, true bytes | ETW `FileIo` request (op67/68) matched to OperationEnd (op76) by `IrpPtr` | marker file read back as exactly **400.00 MB** (200 written + 200 read); pre-opened file exactly **50.00 MB**. Requested vs transferred over one run: 827 MB vs 738 MB |
| File names | Create (op64) `OpenPath`; handle table for files opened before the trace | marker and pre-opened file both named; ~8-9% of bytes stay unnamed (handle gone/protected) |
| DPC/ISR -> driver, with durations | ETW PerfInfo (DPC op66/68/69, ISR op67, ISR-MSI op50) + raw timestamps (patched ferrisetw) | 0 negative durations; typical DPC averages 3-16 us. **Cross-checked against Windows' `% DPC Time` / `% Interrupt Time` counters: DPC total within ~14-27%, ISR within ~27% (collector low). Same order of magnitude, not exact.** |
| Session health | `ControlTraceW` QUERY | 0 events lost at ~100k events/s (release build, 128 KB x 32..128 buffers) |
| Collector <-> UI pipe | named pipe, user-only DACL, first-instance flag, remote clients rejected, Medium integrity label | unelevated client launched the elevated collector through UAC and exchanged all request types |
| Pipe squatting defence | client checks the server process image; collector uses `FILE_FLAG_FIRST_PIPE_INSTANCE` | a normal-privilege squatter was refused by the client (named pid + path) and the elevated collector refused to start ("Access is denied") |
| App end to end | real app, driven over WebView2 DevTools | UAC launch, live memory/process/profile data, Stop collector -> collector process gone, no error banner |
| ETW streamed to the UI | collector `serve` runs one trace session while the UI is on Disk/Drivers and pushes 2 s delta snapshots; UI shows a 10 s rolling window | `activity_demo`: a 64 MiB write + read-back in a client process was reported as exactly 64.0 MiB written, 64.0 MiB read, 128.0 MiB on the file. In the real app a 14 s PowerShell workload showed 268 MB/s read / 282 MB/s write at file level but ~6 KB/s at the disk (cache). Drivers view lists dxgkrnl, nvlddmkm, ntoskrnl, ndis, storport... |
| Trace lifecycle | elevated watcher logging `logman query -ets` each second | no session on Memory tab; session appears ~2 s after opening Disk or Drivers and is gone ~1 s after leaving; Stop collector leaves none; none leaked for 30+ s |
| Live ETW self-tests | collector startup runs a ~2.5 s session with an 8 MiB workload | all four ETW capabilities report Ok with evidence (e.g. "3550 DPCs and 1992 ISRs timed; 38 of 38 DPC routines mapped to a driver") |
| Build profile | `collector profile`, UI "This PC" card | shows "Verified build" on 19045.7725; 4 live self-tests pass; 4 ETW capabilities show "Not checked" |

## Corrections to earlier statements
- I earlier said ~1 GB of RAM sat "outside any process". **That was a visibility artifact**: an
  unelevated process list under-reports working sets of protected processes (11.7 GB unelevated vs
  14.1-14.8 GB elevated). Summed working sets now exceed "in use" because shared pages are counted
  once per process; the UI says so rather than inventing a residual.
- The 200 MB write-through test did not reach the disk (149 ms, faster than SATA; Windows counters
  agreed), so that workload said nothing about large physical writes.

## Gotchas (each bit during the work)
1. **Header timestamps are FILETIME unless raw timestamps are requested; DPC/ISR `InitialTime` is raw QPC.** Fixed by the vendored ferrisetw patch (`vendor/PATCHES.md`). `EventRecord::timestamp()` is invalid with the patch; use `raw_timestamp()`.
2. **Some DPC events have `InitialTime` = 0**, giving "time since boot" as a duration. Durations over 1 s are dropped and counted.
3. **A crashed collector leaks its ETW session** and the name stays taken (`AlreadyExist`). Call `stop_trace_by_name` at startup.
4. **FileObject addresses are recycled after Close.** Resolve names when each event arrives; drop the mapping on Close (op66).
5. **`IoSize` on FileIo Read/Write is the requested size.** Use OperationEnd `ExtraInfo` for bytes transferred.
6. **Name events (op0/op36) never arrive in a live session** on this build. Use Create `OpenPath`, plus the system handle table for pre-existing files.
7. **FILE_IO flag alone gives only OperationEnd**; request/size events need FILE_IO_INIT (0x04000000).
8. **ISR-MSI (op50) is most of the interrupt time** (GPU/NIC/NVMe); ignoring it made ISR totals ~30x too low.
9. **A synchronous pipe handle serialises I/O**: a blocked read stalls writes on the same handle. Both ends poll with `PeekNamedPipe` from a single thread.
10. **Windows crate features only compile when unified** with another crate's: build each crate alone (`cargo test -p ...`) to catch missing features.
11. **`cargo build --release` of a Tauri app needs `--features custom-protocol`** or it loads the dev-server URL.
12. **PowerShell 5.1 `Set-Content -Encoding utf8` writes a BOM** that broke `package.json`. Use the Write tool for JSON/TOML.
15. **Cache-flush (paging) writes are credited to the calling process.** `sync_all` on a 64 MiB write made the process look like it wrote 128 MiB. Requests with `IoFlags & (0x2 | 0x40)` are excluded from per-process/per-file figures (they still show as physical disk traffic). Found by the streaming test, confirmed by re-running it.
16. **End-of-file is not a failure**: the final read of every file completes with `STATUS_END_OF_FILE`; counting it as failed made `io_failed` look alarming.
17. **Unelevated tools can't see kernel ETW sessions** (`logman query -ets` shows nothing), so "session stopped" checks must run elevated. My first such check was meaningless for that reason.
18. **`cargo build -p a -p b --examples` did not rebuild the binary under test**, so one test run exercised stale code. Build each target explicitly and check timestamps.
19. Killing the app (instead of Stop) leaves the elevated collector until its idle-exit; an unelevated process can't kill it, and its exe stays locked for rebuilds meanwhile.
13. `Schema::properties()` is crate-private in ferrisetw; TDH parsing per event is costly. For the real ETW stream, parse the fixed MOF layouts directly. Debug builds are too slow for ETW; use release.
14. ETW buffers are nonpaged memory; keep them modest.

## Profile and self-test design
- `collector/profiles/windows-10-19045.json`: what was verified, at which UBR, on what hardware, plus quirks and ETW opcode tables (as data; the ETW code still uses constants and should move to the profile when ETW is streamed).
- Match levels: **exact** (same build + UBR), **same build, other patch** (self-tests decide), **untested**.
- Runtime self-tests today: process list; memory counters; memory page lists (invariant `available == free + zero + standby`, also enforced on every sample, so a changed structure layout yields "no breakdown" rather than nonsense); handle-table layout (opens its own exe, finds that handle in the system table, checks the resolved path).
- ETW capabilities are now live-tested at startup too (see above). The ETW opcode tables sit in the profile JSON as data but the code still uses constants; moving to the profile is the step that makes a second build truly data-driven.

## Not yet verified / not built
- Disk latency: `HighResResponseTime` is assumed to be in 100 ns units. Values of 70-260 us on SATA SSDs look plausible; not independently checked, and QPC = 10 MHz here hides any unit confusion.
- Longer runs: memory growth/CPU cost of the trace over hours has not been measured. The `pending_io`, name and handle maps are bounded or cleaned on Close, but this is untested at length.
- Sort/windowing UI is checked with real data on one machine only; no automated UI test beyond the window-merge unit tests. The "This PC" tab's live capability list was not re-screenshotted after the self-tests went live (the data it shows was verified through `activity_demo`).
- The close-window path that shuts down a collector the UI launched (idle-exit 30 s is the backstop). Not exercised because the collector in the final test may not have been launched by the app.
- Per-thread CPU, context switches, per-process modules/handles/network connections, "suspicious" flags.
- Windows 11, other editions, other hardware, other patch levels.
- Release hardening: the client trusts a pipe served by an exact binary path; that only holds if the collector is installed somewhere a normal user can't write (e.g. Program Files). No installer yet.
- Not a git repository yet.
