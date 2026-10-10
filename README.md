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
  installs for all users to `C:\Program Files\ProcLens`, with a ProcLens folder in the Start menu
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

The app looks for `collector.exe` next to itself (override with `PROCLENS_COLLECTOR`).

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
**ProcLens > Check this PC (discover)** runs `discover` for you (one UAC prompt, window stays open).
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

Tests: `cargo test --workspace` (protocol, profile matching, activity conversion) and
`cd app; npm test` (rolling-window merge logic). `proto/examples/activity_demo.rs` checks the whole
streaming path against a known 64 MiB workload; `tools/watch-sessions.ps1` (elevated) logs whether
the kernel trace session exists, which is how start/stop on tab changes was verified.
