import { describe, expect, it } from "vitest";
import type { MemoryStats } from "./api";
import { approxMegabytesPerDay, daysKept, gb, LOG_INTERVALS, memoryLevel, memoryUse, summaryLines } from "./summary";

const GB = 1024 ** 3;

const mem = (over: Partial<MemoryStats> = {}): MemoryStats => ({
  physical_total: 32 * GB, physical_available: 12 * GB, system_cache: 0, kernel_nonpaged: 0, kernel_paged: 0,
  commit_total: 20 * GB, commit_limit: 60 * GB, handle_count: 0, process_count: 0, thread_count: 0, lists: null, ...over,
});

describe("memoryUse", () => {
  it("uses total minus available when the page lists are not available", () => {
    const u = memoryUse(mem());
    expect(u.usedBytes).toBe(20 * GB);
    expect(u.usedPct).toBeCloseTo(62.5);
    expect(u.commitPct).toBeCloseTo(33.33, 1);
  });

  it("prefers the page-list figure when the collector is elevated", () => {
    const u = memoryUse(mem({ lists: { free_zero: 0, standby: 0, modified: 0, bad: 0, in_use: 8 * GB } }));
    expect(u.usedBytes).toBe(8 * GB);
    expect(u.usedPct).toBeCloseTo(25);
  });

  it("copes with a zero total and with available above total", () => {
    expect(memoryUse(mem({ physical_total: 0, physical_available: 0, commit_limit: 0 })).usedPct).toBe(0);
    expect(memoryUse(mem({ physical_available: 40 * GB })).usedBytes).toBe(0);
  });
});

describe("memoryLevel", () => {
  it("has three bands", () => {
    expect(memoryLevel(10)).toEqual({ level: "ok", label: "Plenty free" });
    expect(memoryLevel(75).level).toBe("wait");
    expect(memoryLevel(89.9).level).toBe("wait");
    expect(memoryLevel(90)).toEqual({ level: "off", label: "Nearly full" });
  });
});

describe("summaryLines", () => {
  const base = { connected: true, memory: mem(), net: { rx: 258 * 1024, tx: 12.8 * 1024 * 1024 }, adaptersUp: 1, failedChecks: 0 };

  it("says what is using memory and what the network is doing", () => {
    expect(summaryLines(base)).toEqual([
      "Memory: 20.0 GB of 32.0 GB in use (63%), 12.0 GB available.",
      "Network: 258 KB/s received, 12.8 MB/s sent over 1 connected adapter.",
    ]);
  });

  it("asks for the collector when memory is unavailable because it is not running", () => {
    const l = summaryLines({ ...base, connected: false, memory: null });
    expect(l[0]).toContain("start the collector");
  });

  it("stays quiet about memory while connected but waiting for the first reading", () => {
    expect(summaryLines({ ...base, memory: null }).some((l) => l.startsWith("Memory"))).toBe(false);
  });

  it("says idle for zero traffic and notes when no adapter is up", () => {
    expect(summaryLines({ ...base, net: { rx: 0, tx: 0 }, adaptersUp: 2 })[1]).toBe("Network: idle received, idle sent over 2 connected adapters.");
    expect(summaryLines({ ...base, adaptersUp: 0 })[1]).toBe("Network: no adapter is connected.");
  });

  it("says nothing about the network before it has a rate", () => {
    expect(summaryLines({ ...base, net: { rx: null, tx: null } })).toHaveLength(1);
  });

  it("warns when self-checks failed", () => {
    expect(summaryLines({ ...base, failedChecks: 2 }).pop()).toContain("2 of this PC's self-checks failed");
  });
});

describe("helpers", () => {
  it("formats gigabytes", () => {
    expect(gb(1.5 * GB)).toBe("1.5 GB");
    expect(gb(300 * 1024 ** 2)).toBe("300 MB");
  });

  it("estimates log size per day", () => {
    expect(approxMegabytesPerDay(30)).toBe(8);
    expect(approxMegabytesPerDay(10)).toBe(24);
    expect(approxMegabytesPerDay(5)).toBe(48);
    expect(approxMegabytesPerDay(60)).toBe(4);
  });

  it("offers exactly 5, 10, 20, 30, 45 and 60 seconds", () => {
    expect(LOG_INTERVALS.map((i) => i.seconds)).toEqual([5, 10, 20, 30, 45, 60]);
  });

  it("keeps 28 days at every interval except the shortest, where the size cap comes first", () => {
    const days = Object.fromEntries(LOG_INTERVALS.map((i) => [i.seconds, daysKept(i.seconds, 28, 1024)]));
    expect(days).toEqual({ 5: 21, 10: 28, 20: 28, 30: 28, 45: 28, 60: 28 });
  });
});
