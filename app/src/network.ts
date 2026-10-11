import type { NetworkAdapter, NetworkSnapshot } from "./api";

/** How many readings the rate is averaged over (one per second, so about three seconds). */
export const RATE_WINDOW = 4;

export interface Rate {
  /** Bytes per second received / sent; null when there is nothing trustworthy to say yet. */
  rx: number | null;
  tx: number | null;
}

/**
 * Bytes per second per adapter from consecutive readings. The counters are running totals, so a
 * rate is a difference over time. A step where a counter went down (the adapter was reset or
 * re-plugged) is skipped rather than turned into a huge or negative rate, and an adapter that
 * appears in only one reading has no rate.
 */
export function ratesOf(history: NetworkSnapshot[]): Map<number, Rate> {
  const out = new Map<number, Rate>();
  const latest = history[history.length - 1];
  if (!latest) return out;

  for (const a of latest.adapters) {
    let rx = 0;
    let tx = 0;
    let ms = 0;
    for (let i = 1; i < history.length; i++) {
      const before = history[i - 1].adapters.find((b) => b.luid === a.luid);
      const after = history[i].adapters.find((b) => b.luid === a.luid);
      const dt = history[i].taken_ms - history[i - 1].taken_ms;
      if (!before || !after || dt <= 0) continue;
      if (after.rx_bytes < before.rx_bytes || after.tx_bytes < before.tx_bytes) continue; // reset
      rx += after.rx_bytes - before.rx_bytes;
      tx += after.tx_bytes - before.tx_bytes;
      ms += dt;
    }
    out.set(a.luid, ms > 0 ? { rx: (rx * 1000) / ms, tx: (tx * 1000) / ms } : { rx: null, tx: null });
  }
  return out;
}

/**
 * The adapters people mean by "my network"; the rule lives in the app's Rust side (`is_primary`) so
 * the view and the activity log agree. Software adapters stay behind a toggle.
 */
export function isPrimary(a: NetworkAdapter): boolean {
  return a.primary;
}

export interface AdapterStatus {
  label: string;
  tone: "ok" | "wait" | "off";
}

/** IF_OPER_STATUS plus the cable/radio state, in words. */
export function statusOf(a: NetworkAdapter): AdapterStatus {
  switch (a.oper_status) {
    case 1:
      return { label: "Connected", tone: "ok" };
    case 2:
      return a.media_connected === false
        ? { label: a.medium === "ethernet" ? "Cable unplugged or no link" : "Not connected", tone: "off" }
        : { label: "Disabled or down", tone: "off" };
    case 3:
      return { label: "Testing", tone: "wait" };
    case 5:
      return { label: "Waiting (dormant)", tone: "wait" };
    case 6:
      return { label: "Not present", tone: "off" };
    case 7:
      return { label: "Lower layer down", tone: "off" };
    default:
      return { label: "Unknown", tone: "wait" };
  }
}

/** "1 Gbps", "100 Mbps", "3 Mbps"; link speed is quoted in bits per second, as Windows does. */
export function linkSpeed(bps: number | null): string {
  if (bps === null) return "–";
  if (bps >= 1e9) return `${+(bps / 1e9).toFixed(1)} Gbps`;
  if (bps >= 1e6) return `${+(bps / 1e6).toFixed(1)} Mbps`;
  return `${Math.round(bps / 1e3)} kbps`;
}

export const MEDIUM_LABEL: Record<NetworkAdapter["medium"], string> = {
  ethernet: "Ethernet",
  wifi: "Wi-Fi",
  bluetooth: "Bluetooth",
  mobile: "Mobile broadband",
  other: "Other",
};

/** Errors and discards in both directions, as a short phrase, or null when there are none. */
export function problems(a: NetworkAdapter): string | null {
  const parts: string[] = [];
  const errors = a.rx_errors + a.tx_errors;
  const discards = a.rx_discards + a.tx_discards;
  if (errors > 0) parts.push(`${errors.toLocaleString()} errors`);
  if (discards > 0) parts.push(`${discards.toLocaleString()} dropped`);
  return parts.length ? parts.join(", ") : null;
}

/** Connected adapters first, then by name, so rows do not jump around as traffic changes. */
export function sortAdapters(list: NetworkAdapter[]): NetworkAdapter[] {
  const up = (a: NetworkAdapter) => (a.oper_status === 1 ? 0 : 1);
  return [...list].sort(
    (a, b) => up(a) - up(b) || Number(b.hardware) - Number(a.hardware) || a.name.localeCompare(b.name),
  );
}

/** Sum of the current rates over the adapters that are up; null when no adapter has a rate yet. */
export function totalRate(adapters: NetworkAdapter[], rates: Map<number, Rate>): Rate {
  let rx: number | null = null;
  let tx: number | null = null;
  for (const a of adapters) {
    if (a.oper_status !== 1) continue;
    const r = rates.get(a.luid);
    if (!r || r.rx === null || r.tx === null) continue;
    rx = (rx ?? 0) + r.rx;
    tx = (tx ?? 0) + r.tx;
  }
  return { rx, tx };
}
