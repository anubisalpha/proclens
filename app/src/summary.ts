import type { MemoryStats } from "./api";
import { bytes, rate } from "./format";
import type { Rate } from "./network";

export interface MemoryUse {
  usedBytes: number;
  totalBytes: number;
  availableBytes: number;
  usedPct: number;
  /** Committed memory as a share of the commit limit (RAM plus page file). */
  commitPct: number;
}

/**
 * "In use" is the page-list figure when the collector is elevated (it excludes the standby cache,
 * which Windows gives back on demand) and otherwise total minus available, which is the same idea.
 */
export function memoryUse(m: MemoryStats): MemoryUse {
  const used = m.lists ? m.lists.in_use : Math.max(0, m.physical_total - m.physical_available);
  const pct = (part: number, whole: number) => (whole > 0 ? Math.min(100, (part / whole) * 100) : 0);
  return {
    usedBytes: used,
    totalBytes: m.physical_total,
    availableBytes: m.physical_available,
    usedPct: pct(used, m.physical_total),
    commitPct: pct(m.commit_total, m.commit_limit),
  };
}

export type Level = "ok" | "wait" | "off";

/** How full memory is, in words and a colour. Thresholds are plain rules of thumb, not measurements. */
export function memoryLevel(pct: number): { level: Level; label: string } {
  if (pct >= 90) return { level: "off", label: "Nearly full" };
  if (pct >= 75) return { level: "wait", label: "Getting full" };
  return { level: "ok", label: "Plenty free" };
}

/** "17.0 GB" style with one decimal for GB, used on the tiles. */
export function gb(n: number): string {
  return n >= 1024 ** 3 ? `${(n / 1024 ** 3).toFixed(1)} GB` : bytes(n);
}

export interface SummaryInput {
  connected: boolean;
  memory: MemoryStats | null;
  /** Total receive/send rate across connected adapters, bytes per second. */
  net: Rate;
  /** Adapters that are up. */
  adaptersUp: number;
  failedChecks: number;
}

/** The short plain-language lines at the top of the landing page. */
export function summaryLines(s: SummaryInput): string[] {
  const out: string[] = [];
  if (s.memory) {
    const u = memoryUse(s.memory);
    out.push(
      `Memory: ${gb(u.usedBytes)} of ${gb(u.totalBytes)} in use (${Math.round(u.usedPct)}%), ${gb(u.availableBytes)} available.`,
    );
  } else if (!s.connected) {
    out.push("Memory: start the collector to see where your RAM is going.");
  }
  if (s.net.rx !== null && s.net.tx !== null) {
    const r = rate(s.net.rx, 1);
    const t = rate(s.net.tx, 1);
    out.push(
      s.adaptersUp === 0
        ? "Network: no adapter is connected."
        : `Network: ${r === "–" ? "idle" : r} received, ${t === "–" ? "idle" : t} sent over ${s.adaptersUp} connected adapter${s.adaptersUp === 1 ? "" : "s"}.`,
    );
  }
  if (s.failedChecks > 0) {
    out.push(
      `${s.failedChecks} of this PC's self-checks failed, so some numbers may be wrong. See the This PC tab.`,
    );
  }
  return out;
}

/** The logging interval choices offered to the user, in seconds, with their labels. */
export const LOG_INTERVALS: { seconds: number; label: string }[] = [
  { seconds: 5, label: "5 seconds" },
  { seconds: 10, label: "10 seconds" },
  { seconds: 20, label: "20 seconds" },
  { seconds: 30, label: "30 seconds" },
  { seconds: 45, label: "45 seconds" },
  { seconds: 60, label: "1 minute" },
];

/** Roughly how much disk a day of logging takes, so the choice is an informed one. A rough figure, not a promise. */
export function approxMegabytesPerDay(intervalSeconds: number): number {
  const bytesPerLine = 2800; // a sample with the top offenders; one measured line was 2.76 KB
  return Math.max(1, Math.round(((86_400 / intervalSeconds) * bytesPerLine) / 1e6));
}

/** How many days of log fit: the retention limit, or fewer when the size cap is reached first. */
export function daysKept(intervalSeconds: number, keepDays: number, capMb: number): number {
  return Math.min(keepDays, Math.floor(capMb / approxMegabytesPerDay(intervalSeconds)));
}
