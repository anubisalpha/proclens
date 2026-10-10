# ProcLens

See what a Windows PC is really running: the memory breakdown, per-process and per-file disk I/O,
and CPU time spent in drivers, beyond what Task Manager shows.

Two processes, split by privilege:

- **collector** (`collector.exe`): elevated, read-only. Reads the system and serves it on a named pipe.
- **app** (`proclens-app.exe`, Tauri 2 + React): runs at normal privilege and displays what the
  collector sends. It launches the collector on request (one UAC prompt).

```
proto/       shared wire format (JSON frames over \\.\pipe\proclens-collector-v1) + client library
collector/   the elevated collector; profiles/ holds per-Windows-build knowledge
app/         Tauri 2 + React UI (src-tauri/ is the Rust side)
vendor/      ferrisetw 1.2.0 with one patch (see vendor/PATCHES.md)
tools/       elevated test scripts, DevTools helper
```

## Build and run

```powershell
cd app; npm install; npm run build; cd ..
cargo build --release -p collector
cargo build --release -p proclens-app --features custom-protocol
.\target\release\proclens-app.exe      # click "Start collector", approve the UAC prompt
```

The app looks for `collector.exe` next to itself (override with `PROCLENS_COLLECTOR`).

```powershell
cargo test --workspace                       # protocol framing, profile matching
.\target\release\collector.exe profile       # this PC's Windows build, matched profile, self-tests
.\target\release\collector.exe mem|procs     # quick data dumps (mem's page lists need admin)
.\tools\run-elevated.ps1 -Seconds 15 -Out <prefix> -MarkerDir <dir>   # ETW spike with known workloads
```

## Windows-build profiles

Much of what ProcLens reads is undocumented and can differ between Windows builds, so support is
tracked per build rather than assumed.

- `collector/profiles/<id>.json` records what was **verified by hand** on an exact build and patch
  level (UBR), plus the quirks found there. Only `windows-10-19045.json` exists (19045.7725).
- At startup the collector also runs **self-tests** for each capability. A failure switches that
  feature off instead of showing wrong numbers. This protects on an unprofiled build or after a
  Windows update. The UI shows the result ("Verified build" / "Same build, different patch level"
  / "Untested Windows build").

To support another PC: run `collector profile` there, check the self-tests, repeat the ETW
checks (`tools/run-elevated.ps1`, comparing against Windows' own counters as in
`SPIKE_FINDINGS.md`), then add a profile JSON and register it in `collector/src/profile.rs`.

## License

MIT, see [LICENSE](LICENSE). Copyright (c) 2026 Anubis Productions.

`vendor/ferrisetw` is a patched copy of [ferrisetw](https://github.com/n4r1b/ferrisetw) and stays
under its own terms (MIT OR Apache-2.0, see `vendor/ferrisetw/License.md`). Other dependencies keep
their own licences.

## Status

Working and verified on this machine (Windows 10 22H2, 19045.7725): memory breakdown, process
list, **disk and file activity per process, busiest files, per-driver DPC/ISR CPU time** (streamed
from the collector's kernel trace; the trace runs only while the Disk or Drivers tab is open),
the "This PC" profile with live self-tests (including a real ETW workload test at collector
startup), launch via UAC, shutdown. See `SPIKE_FINDINGS.md`.

Tests: `cargo test --workspace` (protocol, profile matching, activity conversion) and
`cd app; npm test` (rolling-window merge logic). `proto/examples/activity_demo.rs` checks the whole
streaming path against a known 64 MiB workload; `tools/watch-sessions.ps1` (elevated) logs whether
the kernel trace session exists, which is how start/stop on tab changes was verified.
