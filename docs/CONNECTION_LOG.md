# Connection log

Status: built 2026-10-11, tested on the one verified PC (Windows 10 22H2 build 19045). Code:
`app/src-tauri/src/connlog.rs`, card in `app/src/ConnectionLogCard.tsx` (bottom of the Connections tab).

## Decisions (user, 2026-10-11)

- A **separate, opt-in** log. The activity log keeps its rule of never holding network addresses.
- **Real remote addresses** are recorded (a per-day hash was offered and not chosen).
- **Loopback excluded.**
- **No per-connection or per-program byte counts** in the first version (per-process traffic needs the
  elevated kernel trace, see `NETWORK_SCOPE.md`, which is not built).
- Asked next, same day: "log the traffic at the point of the log" to get some idea of throughput. Done as
  a `traffic` line at every check: the **whole PC's** bytes in and out per adapter since the previous
  check, from the same counters as the Network tab (no elevation). It says how busy the network was,
  not who used it.

## Files

In the same folder as the activity log (`%LOCALAPPDATA%\ProcLens\logs`):

- `proclens-connections-YYYY-MM-DD.jsonl`, one per UTC day, kept 28 days and 256 MB in total (oldest
  first). The activity log's pruner only touches `proclens-YYYY-MM-DD.jsonl` and this one only
  `proclens-connections-...`; a test checks that neither can match the other's files.
- `proclens-connections-seen.json`, the (program, address) pairs seen so far (at most 20,000, oldest
  forgotten first). Never pruned by date.

Off whenever the app starts. Written by the app (never the elevated collector), only while the
window is open. Switching it off, or closing the window, writes the sessions still open.

## Lines

Every line has `v` (schema, 1). Times are UTC RFC 3339.

| `event` | When | Fields |
|---|---|---|
| `start` | logging switched on | `t`, `app_version`, `interval_s`, `known_pairs` (0 on the first ever run) |
| `connection` | a TCP connection to another machine has ended | `first_seen`, `last_seen`, `duration_s`, `program`, `path`, `pid`, `proto`, `local_addr`, `local_port`, `remote_addr`, `remote_port`, `last_state`, `new_peer`, `already_open`, `ended_by`, and `services` when the process hosts Windows services |
| `listener` | a TCP listener or UDP socket that accepts non-local traffic has gone | the same, without the remote fields, `last_state` and `new_peer` |
| `traffic` | every check after the first | `t`, `seconds` (measured from the clock, so a skipped check gives a longer interval, not a wrong rate), `rx_bytes`, `tx_bytes`, `rx_bytes_per_s`, `tx_bytes_per_s` (all adapters together), `adapters` (`name`, `rx_bytes`, `tx_bytes` each), `open_connections` |
| `stop` | logging switched off | `t` |

- `new_peer`: true the first time this program (compared ignoring case) is seen with this remote
  address. On the first run everything is new, so read `known_pairs` on the `start` line.
- `already_open`: the session was there when logging started, so `first_seen` is only a lower bound.
- `ended_by`: `closed` (gone from the table) or `log_stopped`.
- `services`: the short names of the Windows services running inside the process (`["Dnscache"]` for a
  `svchost.exe`), from the Service Control Manager, absent for an ordinary program. If a process hosts
  several, all are listed and the socket belongs to one of them (Windows only says which with the elevated
  owner-module tables, not used). `new_peer` treats `svchost.exe[Dnscache]` and `svchost.exe[EventLog]` as
  different programs. On the verified PC all but one of the `svchost.exe` processes owning ports hosted a
  single service; that depends on the PC (Windows groups services more when memory is short).
- `program` is `unknown (pid N)` when Windows would not give a name; `path` is null for protected
  processes.

## What it can and cannot tell you

- A session is found by comparing readings taken every 5 to 60 seconds (default 10). One shorter than
  the interval can be missed. `first_seen` can be up to one interval after the real start and
  `last_seen` up to one interval before the real end. In one test, a connection held for 21 s was
  logged as 15.1 s with a 5 s interval.
- Left out: loopback in either direction, TCP connections already closing (TIME_WAIT and the other
  closing states), listeners bound only to loopback.
- No bytes per connection or program. The owner tables carry no counters. The `traffic` lines count only
  adapters that are up and "primary" (cards, Wi-Fi, Bluetooth, mobile; the Network tab's rule), and include
  traffic to other machines on your own network (a Plex stream, a backup), not only the internet. An adapter
  whose counters went backwards, or that appeared since the last check, is left out of that line. If the
  counters cannot be read, that check writes no `traffic` line.
- To tell which program caused a burst you can only line up the times: look at the sessions that were open
  and the programs involved. That is a hint, not an attribution.
- A crash loses sessions that were still open.
- Size: 354 bytes a line on average in a one-minute test. How many lines a day this makes has not
  been measured; check the folder after a day before trusting any estimate.
