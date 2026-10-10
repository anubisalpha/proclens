import type {
  ActivityHealth,
  ActivitySnapshot,
  DriverActivity,
  FileActivity,
  ProcessActivity,
} from "./api";

/** Several consecutive 2-second snapshots merged into one steadier view. */
export interface ActivityWindow {
  seconds: number;
  logicalCpus: number;
  processes: ProcessActivity[];
  files: FileActivity[];
  drivers: DriverActivity[];
  health: ActivityHealth;
}

const MAX_FILES = 60;

export function windowOf(history: ActivitySnapshot[]): ActivityWindow | null {
  if (history.length === 0) return null;
  const latest = history[history.length - 1];
  const seconds = history.reduce((s, a) => s + a.interval_ms, 0) / 1000;

  const procs = new Map<number, ProcessActivity & { latencyWeighted: number; latencyOps: number }>();
  const files = new Map<string, number>();
  const drivers = new Map<string, DriverActivity>();

  for (const snap of history) {
    for (const p of snap.processes) {
      const e =
        procs.get(p.pid) ??
        { ...p, disk_read_bytes: 0, disk_write_bytes: 0, disk_ops: 0, file_read_bytes: 0, file_write_bytes: 0, file_ops: 0, disk_latency_us: null, latencyWeighted: 0, latencyOps: 0 };
      e.name = p.name; // newest name wins
      e.disk_read_bytes += p.disk_read_bytes;
      e.disk_write_bytes += p.disk_write_bytes;
      e.disk_ops += p.disk_ops;
      e.file_read_bytes += p.file_read_bytes;
      e.file_write_bytes += p.file_write_bytes;
      e.file_ops += p.file_ops;
      if (p.disk_latency_us !== null && p.disk_ops > 0) {
        // The snapshot's latency is a mean over its requests, so weight by request count.
        e.latencyWeighted += p.disk_latency_us * p.disk_ops;
        e.latencyOps += p.disk_ops;
      }
      procs.set(p.pid, e);
    }
    for (const f of snap.files) files.set(f.path, (files.get(f.path) ?? 0) + f.bytes);
    for (const d of snap.drivers) {
      const e = drivers.get(d.name) ?? {
        name: d.name, dpc_calls: 0, dpc_time_us: 0, dpc_max_us: 0, isr_calls: 0, isr_time_us: 0, isr_max_us: 0,
      };
      e.dpc_calls += d.dpc_calls;
      e.dpc_time_us += d.dpc_time_us;
      e.dpc_max_us = Math.max(e.dpc_max_us, d.dpc_max_us);
      e.isr_calls += d.isr_calls;
      e.isr_time_us += d.isr_time_us;
      e.isr_max_us = Math.max(e.isr_max_us, d.isr_max_us);
      drivers.set(d.name, e);
    }
  }

  const health: ActivityHealth = {
    // These two are cumulative since the trace started, so the newest value is the right one.
    events_lost: latest.health.events_lost,
    buffers_lost: latest.health.buffers_lost,
    io_failed: 0,
    durations_dropped: 0,
    unresolved_file_bytes: 0,
    file_bytes_total: 0,
  };
  for (const snap of history) {
    health.io_failed += snap.health.io_failed;
    health.durations_dropped += snap.health.durations_dropped;
    health.unresolved_file_bytes += snap.health.unresolved_file_bytes;
    health.file_bytes_total += snap.health.file_bytes_total;
  }

  return {
    seconds,
    logicalCpus: latest.logical_cpus,
    processes: [...procs.values()].map(({ latencyWeighted, latencyOps, ...p }) => ({
      ...p,
      disk_latency_us: latencyOps > 0 ? latencyWeighted / latencyOps : null,
    })),
    files: [...files.entries()]
      .map(([path, bytes]) => ({ path, bytes }))
      .sort((a, b) => b.bytes - a.bytes)
      .slice(0, MAX_FILES),
    drivers: [...drivers.values()],
    health,
  };
}
