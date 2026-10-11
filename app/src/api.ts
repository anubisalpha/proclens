import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// These mirror proclens-proto (proto/src/lib.rs). All sizes are bytes.
export interface MemoryLists {
  free_zero: number;
  standby: number;
  modified: number;
  bad: number;
  in_use: number;
}

export interface MemoryStats {
  physical_total: number;
  physical_available: number;
  system_cache: number;
  kernel_nonpaged: number;
  kernel_paged: number;
  commit_total: number;
  commit_limit: number;
  handle_count: number;
  process_count: number;
  thread_count: number;
  lists: MemoryLists | null;
}

export interface ProcessInfo {
  pid: number;
  name: string;
  working_set: number;
  virtual_size: number;
  cpu_percent: number;
}

export interface ProcessList {
  processes: ProcessInfo[];
  total_working_set: number;
}

/** One interval (a delta, not a running total); mirrors proto's ActivitySnapshot. */
export interface ActivitySnapshot {
  interval_ms: number;
  logical_cpus: number;
  processes: ProcessActivity[];
  files: FileActivity[];
  drivers: DriverActivity[];
  health: ActivityHealth;
}

export interface ProcessActivity {
  pid: number;
  name: string;
  disk_read_bytes: number;
  disk_write_bytes: number;
  disk_ops: number;
  disk_latency_us: number | null;
  file_read_bytes: number;
  file_write_bytes: number;
  file_ops: number;
}

export interface FileActivity {
  path: string;
  bytes: number;
}

export interface DriverActivity {
  name: string;
  dpc_calls: number;
  dpc_time_us: number;
  dpc_max_us: number;
  isr_calls: number;
  isr_time_us: number;
  isr_max_us: number;
}

export interface ActivityHealth {
  events_lost: number;
  buffers_lost: number;
  io_failed: number;
  durations_dropped: number;
  unresolved_file_bytes: number;
  file_bytes_total: number;
}

export interface OsInfo {
  major: number;
  minor: number;
  build: number;
  ubr: number;
  arch: string;
  display_version: string;
  edition: string;
}

export type Verification = "exact" | "same_build_other_patch" | "untested";
export type CapabilityStatus = "ok" | "failed" | "not_run";

export interface Capability {
  id: string;
  status: CapabilityStatus;
  detail: string;
  verified_in_profile: boolean;
}

export interface SystemProfile {
  os: OsInfo;
  verification: Verification;
  profile_title: string | null;
  capabilities: Capability[];
}

/** One network adapter's counters; mirrors the app's `network::Adapter`. Bytes and packets are running totals. */
export interface NetworkAdapter {
  luid: number;
  name: string;
  description: string;
  medium: "ethernet" | "wifi" | "bluetooth" | "mobile" | "other";
  hardware: boolean;
  /** The adapters people mean by "my network"; see the app's `network::is_primary`. */
  primary: boolean;
  /** IF_OPER_STATUS: 1 up, 2 down, 3 testing, 4 unknown, 5 dormant, 6 not present, 7 lower layer down. */
  oper_status: number;
  media_connected: boolean | null;
  rx_speed_bps: number | null;
  tx_speed_bps: number | null;
  rx_bytes: number;
  tx_bytes: number;
  rx_packets: number;
  tx_packets: number;
  rx_errors: number;
  tx_errors: number;
  rx_discards: number;
  tx_discards: number;
}

export interface NetworkSnapshot {
  /** Milliseconds on a monotonic clock; only differences mean anything. */
  taken_ms: number;
  adapters: NetworkAdapter[];
}

/** State of the optional activity log; mirrors the app's `logger::LogStatus`. */
export interface LogStatus {
  enabled: boolean;
  interval_s: number;
  dir: string;
  file: string | null;
  lines: number;
  error: string | null;
  /** Retention rules, stated by the app so the card never hard-codes them. */
  keep_days: number;
  cap_mb: number;
}

/** State of the optional connection log; mirrors the app's `connlog::ConnLogStatus`. */
export interface ConnLogStatus {
  enabled: boolean;
  interval_s: number;
  dir: string;
  file: string | null;
  /** Lines written since logging was switched on. */
  lines: number;
  /** Sessions being watched right now that have not been written yet. */
  open_sessions: number;
  /** (program, address) pairs known, behind the "new address" flag. */
  seen_pairs: number;
  error: string | null;
  keep_days: number;
  cap_mb: number;
}

export interface NetworkSelfTest {
  ok: boolean;
  detail: string;
}

/** One TCP listener or connection, or one bound UDP socket; mirrors the app's `ports::Endpoint`. */
export interface PortEndpoint {
  proto: "tcp" | "udp";
  v6: boolean;
  local_addr: string;
  local_port: number;
  remote_addr: string | null;
  remote_port: number | null;
  /** TCP state in snake_case ("listen", "established"); null for UDP. */
  state: string | null;
  pid: number;
}

export interface PortService {
  /** Short name ("Dnscache"). */
  name: string;
  /** The name services.msc shows ("DNS Client"). */
  display: string;
}

export interface PortOwner {
  pid: number;
  name: string | null;
  path: string | null;
  /** Windows services running inside this process; empty for an ordinary program. */
  services: PortService[];
}

export interface PortsSnapshot {
  endpoints: PortEndpoint[];
  owners: PortOwner[];
  /** Set when the service list could not be read; then no program shows services. */
  services_error: string | null;
}

export interface ExcludedRange {
  proto: "tcp" | "udp";
  start: number;
  end: number;
  /** Set by an administrator (netsh marks these with `*`), rather than reserved by Windows itself. */
  administered: boolean;
}

export interface ExcludedRanges {
  ranges: ExcludedRange[];
  error: string | null;
}

export type Status =
  | { state: "disconnected"; reason: string | null }
  | { state: "connecting" }
  | { state: "connected"; pid: number; elevated: boolean; collector_version: string };

/** Snapshots arrive every 2 s; five of them make the 10-second window the views display. */
export const ACTIVITY_WINDOW = 5;

export function useCollector() {
  const [status, setStatus] = useState<Status>({ state: "disconnected", reason: null });
  const [memory, setMemory] = useState<MemoryStats | null>(null);
  const [processes, setProcesses] = useState<ProcessList | null>(null);
  const [profile, setProfile] = useState<SystemProfile | null>(null);
  const [activity, setActivity] = useState<ActivitySnapshot[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn[] = [];

    (async () => {
      const subs = await Promise.all([
        listen<Status>("collector-status", (e) => {
          setStatus(e.payload);
          if (e.payload.state !== "connected") {
            // Stale numbers from a collector that's gone would look live.
            setMemory(null);
            setProcesses(null);
            setProfile(null);
            setActivity([]);
          } else {
            setError(null);
          }
        }),
        listen<MemoryStats>("memory", (e) => setMemory(e.payload)),
        listen<ProcessList>("processes", (e) => setProcesses(e.payload)),
        listen<SystemProfile>("profile", (e) => setProfile(e.payload)),
        // Keep the last few snapshots; views show them as one rolling window.
        listen<ActivitySnapshot>("activity", (e) =>
          setActivity((prev) => [...prev, e.payload].slice(-ACTIVITY_WINDOW)),
        ),
        listen<string>("collector-error", (e) => setError(e.payload)),
      ]);
      if (cancelled) {
        subs.forEach((u) => u());
        return;
      }
      unlisten = subs;
      // The collector may already have connected before our listeners existed.
      const current = await invoke<Status>("get_status");
      if (!cancelled) setStatus((prev) => (prev.state === "disconnected" ? current : prev));
    })();

    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, []);

  const start = useCallback(() => {
    setError(null);
    invoke("start_collector").catch((e) => setError(String(e)));
  }, []);
  const stop = useCallback(() => {
    invoke("stop_collector").catch((e) => setError(String(e)));
  }, []);

  /** Ask the collector to run (or stop) its kernel trace. Safe to call repeatedly. */
  const setActivityEnabled = useCallback((enabled: boolean) => {
    if (!enabled) setActivity([]);
    invoke("set_activity", { enabled }).catch((e) => setError(String(e)));
  }, []);

  return { status, memory, processes, profile, activity, error, start, stop, setActivityEnabled };
}
