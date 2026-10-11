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
tools/       release packaging, third-party notices, elevated test scripts, DevTools helper
installer/   Inno Setup script for the setup .exe
docs/        REFERENCES.md, release notes (releases/), licence texts for crates that ship none (licenses/)
```

## Install

Download a build from the [Releases page](https://github.com/anubisalpha/proclens/releases) and read
its notes first: this is a beta, verified on one PC only. Compare the file's SHA-256 with the
`.sha256` file.

- **`proclens-<version>-windows-x64-setup.exe` (recommended).** Asks for administrator approval and
  installs for all users to `C:\Program Files\ProcLens`, with a ProcLens folder inside the shared **aity** folder in the Start menu
  (ProcLens, README, Release notes, Check this PC (discover), Uninstall), a desktop shortcut
  (ticked by default; untick it to skip) and an uninstaller under Apps & features. There is deliberately no per-user install: the
  app starts `collector.exe` from its own folder with administrator rights, so that folder must not be
  writable by ordinary programs. Setup and the uninstaller ask you to close ProcLens first if it is open.
- **`proclens-<version>-windows-x64.zip` (portable).** Extract it somewhere only administrators can
  write (for example under Program Files) for the same reason.

Both are unsigned, so SmartScreen will warn. Needs the Microsoft Edge WebView2 runtime (normally
already installed). Start `proclens-app.exe`, click **Start collector**, and approve the UAC prompt.

The strip at the bottom of the window has a link to this project and a **Check for updates** button.
The check runs only when clicked: it reads GitHub's public release list over HTTPS (through Windows'
own WinHTTP, with the system's TLS and proxy settings), including pre-releases, and links to the newest
release if it is newer than the running version. It never downloads or installs anything, and it is the
app's only network access.

## Build and run

```powershell
cd app; npm install; npm run build; cd ..
cargo build --release -p collector
cargo build --release -p proclens-app --features custom-protocol
.\target\release\proclens-app.exe      # click "Start collector", approve the UAC prompt
```

The app looks for `collector.exe` next to itself. (Debug builds also accept `PROCLENS_COLLECTOR`; release builds ignore it, because that path decides which program gets elevated.)

```powershell
cargo test --workspace                       # protocol framing, profile matching
.\target\release\collector.exe profile       # this PC's Windows build, matched profile, self-tests
.\target\release\collector.exe discover      # (elevated) derive the ETW tables from live events and compare with the profile
.\target\release\collector.exe discover --emit draft.json   # also write the discovered `etw` section as a draft profile
.\target\release\collector.exe mem|procs     # quick data dumps (mem's page lists need admin)
.\tools\run-elevated.ps1 -Seconds 15 -Out <prefix> -MarkerDir <dir>   # ETW spike with known workloads
```

## Checking your own PC: `profile` and `discover`

The collector has two diagnostic commands that need no UI. On an installed copy, the Start menu's
**aity > ProcLens > Check this PC (discover)** runs `discover` for you (one UAC prompt, window stays open).
Otherwise run them from `C:\Program Files\ProcLens`; from a source checkout use
`.\target\release\collector.exe`.

```powershell
& "C:\Program Files\ProcLens\collector.exe" profile    # normal PowerShell is fine
& "C:\Program Files\ProcLens\collector.exe" discover   # elevated PowerShell
```

- **`profile`** prints your Windows build and patch level, whether ProcLens has a verified profile for
  it, the result of each self-test (`ok`, `failed` or `notrun`), and the quirks recorded for the matched
  profile. From an unelevated prompt the memory-list, handle-table and ETW tests show `notrun` ("needs
  an elevated collector"); run it from an elevated prompt to exercise them, or read the same results on
  the app's **This PC** tab once the collector is started.
- **`discover`** (elevated) derives this build's ETW event tables from live events and compares them
  with the profile, reporting each as match, subset or mismatch. `discover --emit draft.json` also
  writes the discovered `etw` section as a draft profile. It prints event types and field names only.

On a Windows build other than 19045, attach both outputs to a
[GitHub issue](https://github.com/anubisalpha/proclens/issues): that is how new builds get profiled.

## Windows-build profiles

Much of what ProcLens reads is undocumented and can differ between Windows builds, so support is
tracked per build rather than assumed.

- `collector/profiles/<id>.json` records what was **verified by hand** on an exact build and patch
  level (UBR), plus the quirks found there. Only `windows-10-19045.json` exists (19045.7725).
- At startup the collector also runs **self-tests** for each capability. A failure switches that
  feature off instead of showing wrong numbers. This protects on an unprofiled build or after a
  Windows update. The UI shows the result ("Verified build" / "Same build, different patch level"
  / "Untested Windows build").

A profile also carries the build's ETW event tables (`etw` section: opcodes, kernel provider
flags, paging-I/O mask, disk-latency tick length). They are data, not code, and are validated at
startup and in `cargo test`.

To support another PC: run `collector profile` there, check the self-tests, repeat the ETW
checks (`tools/run-elevated.ps1`, comparing against Windows' own counters as in
`SPIKE_FINDINGS.md`), then add a profile JSON and register it in `collector/src/profile.rs`.

## Building a release

From a clean, committed checkout: `.\tools\package-release.ps1`. It checks the version numbers agree,
runs every test, builds (into `target-release\`, so a running dev copy can't block it), generates
`THIRD-PARTY-NOTICES.txt` with the full licence text of every shipped package (it fails if any is
missing), and writes `dist\proclens-<version>-windows-x64.zip` and
`dist\proclens-<version>-windows-x64-setup.exe`, each with a SHA-256 file. The installer needs
[Inno Setup 6](https://jrsoftware.org/isinfo.php) (`iscc`) on the build machine; its script is
[`installer/proclens.iss`](installer/proclens.iss). The app holds a named mutex (`ProcLens-App-Running`)
while it runs, which is how setup and the uninstaller detect it; the name must match `AppMutex` in
that script. Release notes live in [`docs/releases/`](docs/releases/).

## Security notes

The collector runs elevated and the app does not; they talk over a named pipe.

- **Who can connect:** only the account that started the collector and the account that runs the app (the pipe's access list names exactly those, and nothing sandboxed at Low integrity). Another Windows user, another machine or a sandboxed program cannot.
- **What that does not stop:** any other program running as *you* can also connect, read what the collector reports (per-process disk and file activity, including files of processes you could not inspect yourself) and ask it to shut down. ProcLens draws its line at the logged-on user, as Task Manager's elevated view does; it does not try to tell the app from other programs of yours. The collector is read-only: it cannot change a process, file or setting.
- **Which program is elevated:** the app elevates `collector.exe` from its own folder only (Program Files when installed), and before sending anything checks that the pipe is served by that exact file.
- **Messages:** a client that stops halfway through a message, sends an oversized one, or opens more than 16 connections is disconnected.

## FAQ

[`docs/FAQ.md`](docs/FAQ.md) covers the questions that came up while testing: a failed built-in check
while data is still arriving, why administrator approval and Program Files are needed, SmartScreen,
network access, and uninstall leftovers.

## References

[`docs/REFERENCES.md`](docs/REFERENCES.md) records every Microsoft page and other source the
project relies on, how far each was checked, and where the documentation and our measurements
disagree. [`SPIKE_FINDINGS.md`](SPIKE_FINDINGS.md) has the test results and gotchas.

## License

MIT, see [LICENSE](LICENSE). Copyright (c) 2026 Anubis Productions.

`vendor/ferrisetw` is a patched copy of [ferrisetw](https://github.com/n4r1b/ferrisetw) and stays
under its own terms (MIT OR Apache-2.0, see `vendor/ferrisetw/License.md`). Other dependencies keep
their own licences; releases ship `THIRD-PARTY-NOTICES.txt`, which lists every bundled package with
its full licence text (`docs/licenses/` holds the texts for the six crates whose archives have none).

## Status

Working and verified on this machine (Windows 10 22H2, 19045.7725): memory breakdown, process
list, **disk and file activity per process, busiest files, per-driver DPC/ISR CPU time** (streamed
from the collector's kernel trace; the trace runs only while the Disk or Drivers tab is open),
the "This PC" profile with live self-tests (including a real ETW workload test at collector
startup), launch via UAC, shutdown. See `SPIKE_FINDINGS.md`.

**Overview** (new, the first thing you see): a few plain sentences and tiles for memory, network and
this PC's verification status; with the collector not running it shows what needs no administrator
rights (network) and offers to start it.

**Activity log** (new, off until you switch it on): a JSON-lines record the app writes while it is
open, every 5, 10, 20, 30, 45 or 60 seconds as you choose, to `%LOCALAPPDATA%\ProcLens\logs`, one file per
UTC day, kept 28 days and at most 1 GB (so 5-second samples, about 48 MB a day, keep about 21 days). Each sample has memory and network numbers plus the busiest
programs, files and drivers, so the files contain program names and file paths (never network
addresses). Switching it on also keeps the kernel trace running, because disk and driver activity
are part of what it records. Tested here by switching it on in the app, reading the file it wrote,
and switching it off (start and stop lines, samples with real rates, nothing written while off);
the field list is in `app/src-tauri/src/logger.rs`. Anomaly detection is scoped in
`docs/ANOMALY_DETECTION.md`, not built.

**Network** (adapters only): per-adapter link state, speed, receive and send rates and totals,
read by the app itself with `GetIfTable2`, so it works before the collector is started and needs no
administrator approval. The counters were checked against Windows' own `Get-NetAdapterStatistics`
(five readings each bracketed by two independent ones) and the view was seen live on the one
verified PC, which has Ethernet and Bluetooth PAN adapters only. Per-program traffic, Wi-Fi and
Bluetooth devices are not built; see `docs/NETWORK_SCOPE.md`.

**Ports** and **Connections** (new): which TCP and UDP ports are open and which program holds each
one, and the live TCP connections grouped by program. Both read Windows' owner-PID tables
(`GetExtendedTcpTable` / `GetExtendedUdpTable`) in the app itself, so they work before the collector
is started and need no administrator approval. Ports shows who can reach a listener ("any address"
or "this PC only"), the ranges Windows keeps back (read from `netsh interface ipv4 show
excludedportrange`; only English output was checked), and a search box that says whether a port is
free, in use, or reserved. Connections has Open / Closing / All, an "only outside this PC" toggle and
a search box. A program Windows protects shows its name but no path. Inside a shared process such as
`svchost.exe` the view names the Windows services running in it (from the Service Control Manager,
unelevated): on the verified PC, nine `svchost.exe` processes owning ports showed exactly the
services PowerShell's `Win32_Service` lists. Where one process hosts several services the view says
"one of" them, because Windows does not say which one owns a given socket without administrator
rights. Remote addresses appear on screen and are never looked up.

**Connection log** (new, off until you switch it on, and off again at every start): a separate
JSON-lines record, apart from the activity log because it holds **real remote addresses**. One line is
written when a connection or listener ends: program, path, services, remote address and port, first
and last seen, duration, whether this program had talked to that address before, plus a `traffic`
line at every check with the whole PC's bytes in and out per adapter (the Network tab's counters; it
shows how busy the network was, not which program used it). Loopback traffic and connections that are
already closing are left out. It checks every 5 to 60 seconds, so a connection shorter than that can
be missed and times are only as exact as the interval (a connection held for 21 seconds was logged as
15.1 s at a 5 s interval). Files are `proclens-connections-YYYY-MM-DD.jsonl` in the same folder as the
activity log, kept 28 days and at most 256 MB. How many lines a day this makes has not been
measured. Format and limits: `docs/CONNECTION_LOG.md`.

Tests: `cargo test --workspace` (protocol, profile matching, activity conversion) and
`cd app; npm test` (rolling-window merge logic, network rate and adapter logic, port and connection filtering). `proto/examples/activity_demo.rs` checks the whole
streaming path against a known 64 MiB workload; `tools/watch-sessions.ps1` (elevated) logs whether
the kernel trace session exists, which is how start/stop on tab changes was verified.
