# Network expansion: scope

Status: **scoping only, nothing built.** Written 2026-10-11 against Windows 10 22H2 build
19045.7725 (the one verified PC). Order the user asked for: **Ethernet, then Wi-Fi, then
Bluetooth.**

How sure each fact is, using the same words as `docs/REFERENCES.md`:

- **Read**: a Microsoft page was fetched and read on 2026-10-11.
- **Probed**: checked on this PC today with a read-only command.
- **Seen**: appeared in search results only.
- **Unverified**: an assumption or inference; a spike has to settle it.

## 1. What each medium can give us

| Medium | What we could show | Where it comes from | Elevation | Status |
|---|---|---|---|---|
| Ethernet | Per-adapter link state, speed, bytes and packets in/out, errors and discards; rates from counter deltas | `GetIfTable2` / `MIB_IF_ROW2` (docs-confirmed earlier, not re-read today) | **No** | Probed: `Get-NetAdapterStatistics` returns live counters unelevated (Ethernet 2: 6.9 GB in, 13.5 GB out) |
| Ethernet | Which **process** sent and received how many bytes | Kernel ETW `TcpIp` and `UdpIp` events, enabled by `EVENT_TRACE_FLAG_NETWORK_TCPIP` (0x00010000) | **Yes** (kernel trace) | Read: event types and field names below. Unverified: that they arrive on this build through our ferrisetw setup, and what `size` counts |
| Ethernet | Open connections (PID, local and remote endpoint, state), no byte counts | `GetExtendedTcpTable` / `GetExtendedUdpTable` with owner PID | No | Unchecked (general knowledge; not read today) |
| Wi-Fi | Connected or not, signal strength, link rate, channel, radio on/off | `WlanQueryInterface` opcodes `rssi`, `channel_number`, `realtime_connection_quality`, `radio_state`, `statistics`, `current_connection` | No | Read: opcodes exist. **Cannot be tested here** (see section 3) |
| Wi-Fi | SSID, BSSID, nearby networks | `current_connection`, `WlanGetAvailableNetworkList`, `WlanGetNetworkBssList` | No, but **needs location consent** | Read (see section 3) |
| Bluetooth | Radio present, paired devices, which are connected | PnP / Bluetooth APIs | No | Probed: ASUS USB dongle, `bthserv` running, three paired devices (TuneFlux-T6, Marc's S23 FE, MZ) |
| Bluetooth | Battery level of a device | Undocumented device property `{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2` (community scripts); BLE devices also have a GATT Battery service | No | Seen. Probed: on this PC the property exists but is **empty for all three paired devices** (I did not check whether they were connected at the time) |
| Bluetooth | **Throughput** | Nothing supported found | n/a | Unverified. Bluetooth audio and file traffic does not pass through the IP counters. The "Bluetooth Network Connection" adapter (PAN, 3 Mbps, currently disconnected) only carries traffic if PAN networking is in use. Providers `Microsoft-Windows-BTH-BTHPORT` and `-BTHUSB` exist (Probed) but I did not look at their events |

## 2. The per-process traffic facts (Read)

From Microsoft's TcpIp, TcpIp_SendIPV4, TcpIp_TypeGroup1 and UdpIp pages (pages dated 2018, updated 2021):

- Enable flag for both TCP and UDP events: `EVENT_TRACE_FLAG_NETWORK_TCPIP`. Our vendored ferrisetw already exposes `TCP_IP_PROVIDER` (0x00010000) and the UDP GUID.
- TCP IPv4: type 10 send (`TcpIp_SendIPV4`: `PID`, `size`, `daddr`, `saddr`, `dport`, `sport`, `startime`, `endtime`, `seqnum`, `connid`); 11 receive, 13 disconnect, 14 retransmit, 16 reconnect, 18 TCP copy (`TcpIp_TypeGroup1`: `PID`, `size`, `daddr`, `saddr`, `dport`, `sport`, `seqnum`, `connid`). Types 12 and 15 (connect, accept) use `TcpIp_TypeGroup2`, whose fields I did not read.
- TCP IPv6: 26 send, 27 receive, 28 connect, 29 disconnect, 30 retransmit, 31 accept, 32 reconnect, 34 copy.
- UDP: 10 send and 11 receive (IPv4), 26 and 27 (IPv6), 17 fail.
- The page warns that the **event header's `ProcessId` may not be the process that caused the traffic**; the `PID` field in the event body is the one to use.
- `size` is described only as "Size of the packet". Whether that is payload bytes or includes headers is **Unverified**.
- Probed: the manifest-based provider `Microsoft-Windows-Kernel-Network` is present and its event messages read "TCPv4: %2 bytes transmitted from %4:%6 to %3:%5" (PID, size, daddr, saddr, dport, sport), which agrees with the field order above. That is corroboration, not proof that the kernel-flag route delivers the same data on this build.
- ETW limits (Seen, from Microsoft Learn search snippets): up to 8 sessions can share the system trace provider, 2 slots are reserved, so a second named session next to `ProcLensTrace` should fit. Not tested here.

## 3. Things that block or change the plan

1. **No Wi-Fi hardware on this PC** (Probed: only two Ethernet adapters and the Bluetooth PAN adapter; `WlanSvc` is stopped). Wi-Fi code can be unit-tested against recorded structures but **cannot be verified live** here.
2. **Location consent gates Wi-Fi identity data** (Read, "Changes to API behavior for Wi-Fi access and location", page dated 2025-06). Without the user's precise-location consent, `WlanGetAvailableNetworkList`, `WlanGetNetworkBssList`, `WlanScan` and `WlanQueryInterface` with `wlan_intf_opcode_current_connection` return `ERROR_ACCESS_DENIED`. A one-time system prompt appears for processes running in the user's context outside `C:\Windows\System32`. The page does not list the `rssi` opcode as gated, but I have **not tested** that, and it does not say which Windows builds got the change (whether Windows 10 22H2 is affected is Unverified).
3. **Consent is a reason to keep Wi-Fi and Bluetooth out of the elevated collector.** The prompt is tied to a process in the user's context; whether an elevated process of the same user gets one is Unverified. Nothing about Wi-Fi or Bluetooth needs elevation.
4. **Bluetooth battery is undocumented** (Seen) and empty here. Treat it as best-effort per device, hidden when absent.
5. **Event volume is unknown.** Kernel network events fire per send/receive request; a gigabit transfer could be a lot. The existing session already handles about 10^5 events/s for file I/O, but network rates on this PC were not measured.
6. **Privacy.** Remote addresses and Wi-Fi names are personal. Keep addresses, SSIDs and BSSIDs out of `profile`, `discover` and the self-test details (the project's existing rule: those print event types and field names only).

## 4. Recommended design

**A. Split by who needs elevation.**
- **App backend (unelevated):** interface counters and adapter list, connection table, Wi-Fi status, Bluetooth devices. This also means the Network tab shows adapter traffic **without a UAC prompt or a running collector**, which no other tab can do.
- **Collector (elevated):** only what needs the kernel trace: per-process bytes.
- Cost: self-test results now come from two places; the This PC tab has to merge them. Capabilities from the app side would be reported by the app, not by `collector profile`.

**B. Per-process traffic is its own topic and its own trace session.** A new `Topic` (for example `Traffic`) with its own lease and a named kernel session (`ProcLensNet`) that runs only while the Network tab is open, so Disk and Drivers do not pay for network events and the existing session is untouched. Adding a `Topic` variant is additive: a new UI and an old collector get a "bad request" error, a new collector and an old UI never see it; protocol version stays 1 since app and collector ship together.

**C. Profile and self-tests.** New capability ids: `net_interface_counters`, `etw_network`, `wifi_status`, `bluetooth_devices`. The `etw_network` self-test needs no internet: open a loopback TCP and a loopback UDP connection in-process, send known byte counts and check the events (whether loopback traffic produces these events is itself Unverified). `etw.net` opcode tables go in the profile JSON like the existing ones; `discover` gains a network survey that reports event types and field names only. `wifi_status` reports `NotRun` ("no Wi-Fi adapter") on this PC, the same way elevation-only checks report `NotRun`.

**D. UI.** A fifth tab, "Network": adapter table (state, link speed, rate in and out, totals, errors), a per-process bytes table (when the collector is running), a Bluetooth devices list, and a Wi-Fi card that only appears when a Wi-Fi adapter exists. Remote addresses: see decision 3.

## 5. Phases

| Phase | What | Size | Verification | UAC clicks |
|---|---|---|---|---|
| 0. Spike | `collector netspike` (throwaway, like the earlier spikes): named kernel session with the network flag, loopback TCP and UDP traffic of known sizes, print event types, names and field names, compare `size` with bytes sent, count events per second during a real download | S | Settles the Unverified items in section 2 and the event-rate risk | 1 |
| 1. Ethernet adapters | Counters, adapter list, rates, Network tab, unelevated | M | Compare against `Get-NetAdapterStatistics` and Task Manager; unplug-cable and disabled-adapter cases on the Realtek port | 0 |
| 2. Per-process traffic | `Traffic` topic, trace session, profile tables, `etw_network` self-test, `discover` survey | M-L | Known transfer accounted for (as `activity_demo` does for disk); compare with Resource Monitor's Network tab | 1-2 |
| 3. Wi-Fi | Status card, consent handling (detect access denied, link to the location setting), unit tests with recorded structures | M | **Not live-testable here.** Needs a PC with Wi-Fi (or a USB adapter), or a tester running `profile` and reporting back | 0 |
| 4. Bluetooth | Devices, connected state, best-effort battery, PAN adapter counters; no throughput | S-M | Live here (3 paired devices), battery only if a device reports it | 0 |
| 5. Release | `0.1.0-beta.4`, same steps as beta.3 | S | Smoke-test exact files | 1-2 |

Sizes are relative (S, M, L); I have no timing data to give hours. Phase 3 and 4 can swap places; the user asked for Wi-Fi first, but Bluetooth can be verified here and Wi-Fi cannot.

## 6. Decisions needed from the user

1. **Is per-process traffic in scope**, or only per-adapter counters? (Per-process is the part that needs the elevated trace and most of the risk; per-adapter alone is a small, unelevated change.)
2. **Wi-Fi without a way to test it:** ship it labelled "not verified on real hardware" (as the 22H2-only profile already does for other builds), wait until there is a Wi-Fi PC to test on, or skip it for now?
3. **Remote addresses in the UI:** show IP addresses in the connection and per-process views, never do reverse DNS lookups (they generate traffic and leak what you browse to your resolver), and keep them out of anything copied into bug reports. Or hide addresses entirely behind a toggle?
4. **Architecture A** (unelevated sources in the app backend) or keep everything in the collector for uniformity?
5. **Bluetooth expectations:** accept "devices, connection state, battery when available, no throughput"?
