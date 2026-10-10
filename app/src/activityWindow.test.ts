import { describe, expect, it } from "vitest";
import type { ActivitySnapshot, ProcessActivity } from "./api";
import { windowOf } from "./activityWindow";

const proc = (over: Partial<ProcessActivity>): ProcessActivity => ({
  pid: 1, name: "a.exe", disk_read_bytes: 0, disk_write_bytes: 0, disk_ops: 0, disk_latency_us: null,
  file_read_bytes: 0, file_write_bytes: 0, file_ops: 0, ...over,
});

const snap = (over: Partial<ActivitySnapshot>): ActivitySnapshot => ({
  interval_ms: 2000, logical_cpus: 8, processes: [], files: [], drivers: [],
  health: { events_lost: 0, buffers_lost: 0, io_failed: 0, durations_dropped: 0, unresolved_file_bytes: 0, file_bytes_total: 0 },
  ...over,
});

describe("windowOf", () => {
  it("returns null with no data", () => {
    expect(windowOf([])).toBeNull();
  });

  it("sums bytes and seconds across snapshots", () => {
    const w = windowOf([
      snap({ processes: [proc({ disk_read_bytes: 100, file_write_bytes: 5 })] }),
      snap({ interval_ms: 1990, processes: [proc({ disk_read_bytes: 50, file_write_bytes: 7 })] }),
    ])!;
    expect(w.seconds).toBeCloseTo(3.99);
    expect(w.processes).toHaveLength(1);
    expect(w.processes[0].disk_read_bytes).toBe(150);
    expect(w.processes[0].file_write_bytes).toBe(12);
  });

  it("weights latency by request count, ignoring snapshots without disk requests", () => {
    const w = windowOf([
      snap({ processes: [proc({ disk_ops: 1, disk_latency_us: 1000 })] }),
      snap({ processes: [proc({ disk_ops: 3, disk_latency_us: 200 })] }),
      snap({ processes: [proc({ disk_ops: 0, disk_latency_us: null })] }),
    ])!;
    // (1*1000 + 3*200) / 4 = 400
    expect(w.processes[0].disk_latency_us).toBeCloseTo(400);
  });

  it("keeps latency null for a process that never touched the disk", () => {
    const w = windowOf([snap({ processes: [proc({ file_ops: 2, file_read_bytes: 10 })] })])!;
    expect(w.processes[0].disk_latency_us).toBeNull();
  });

  it("takes the maximum of per-call maxima and sums the rest for drivers", () => {
    const d = (over = {}) => ({ name: "x.sys", dpc_calls: 1, dpc_time_us: 10, dpc_max_us: 4, isr_calls: 2, isr_time_us: 6, isr_max_us: 3, ...over });
    const w = windowOf([snap({ drivers: [d()] }), snap({ drivers: [d({ dpc_max_us: 9 })] })])!;
    expect(w.drivers[0]).toMatchObject({ dpc_calls: 2, dpc_time_us: 20, dpc_max_us: 9, isr_calls: 4, isr_time_us: 12, isr_max_us: 3 });
  });

  it("uses the newest cumulative loss counters but sums per-interval ones", () => {
    const h = (events_lost: number, io_failed: number) => ({ events_lost, buffers_lost: 0, io_failed, durations_dropped: 0, unresolved_file_bytes: 1, file_bytes_total: 10 });
    const w = windowOf([snap({ health: h(5, 1) }), snap({ health: h(7, 2) })])!;
    expect(w.health.events_lost).toBe(7);
    expect(w.health.io_failed).toBe(3);
    expect(w.health.unresolved_file_bytes).toBe(2);
    expect(w.health.file_bytes_total).toBe(20);
  });

  it("merges files by path and sorts largest first", () => {
    const w = windowOf([
      snap({ files: [{ path: "C:\\a", bytes: 5 }, { path: "C:\\b", bytes: 9 }] }),
      snap({ files: [{ path: "C:\\a", bytes: 10 }] }),
    ])!;
    expect(w.files).toEqual([{ path: "C:\\a", bytes: 15 }, { path: "C:\\b", bytes: 9 }]);
  });
});
