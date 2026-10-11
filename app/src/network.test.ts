import { describe, expect, it } from "vitest";
import type { NetworkAdapter, NetworkSnapshot } from "./api";
import { isPrimary, linkSpeed, problems, ratesOf, sortAdapters, statusOf, totalRate } from "./network";

const adapter = (over: Partial<NetworkAdapter> = {}): NetworkAdapter => ({
  luid: 1, name: "Ethernet", description: "NIC", medium: "ethernet", hardware: true, primary: true, oper_status: 1,
  media_connected: true, rx_speed_bps: 1e9, tx_speed_bps: 1e9, rx_bytes: 0, tx_bytes: 0,
  rx_packets: 0, tx_packets: 0, rx_errors: 0, tx_errors: 0, rx_discards: 0, tx_discards: 0, ...over,
});

const snap = (taken_ms: number, ...adapters: NetworkAdapter[]): NetworkSnapshot => ({ taken_ms, adapters });

describe("ratesOf", () => {
  it("has no rate from a single reading", () => {
    const r = ratesOf([snap(0, adapter())]);
    expect(r.get(1)).toEqual({ rx: null, tx: null });
  });

  it("is bytes per second from the counters and the monotonic clock, not from a timer", () => {
    const r = ratesOf([
      snap(1000, adapter({ rx_bytes: 1_000_000, tx_bytes: 0 })),
      snap(3000, adapter({ rx_bytes: 5_000_000, tx_bytes: 200_000 })), // 2 s apart
    ]);
    expect(r.get(1)).toEqual({ rx: 2_000_000, tx: 100_000 });
  });

  it("averages over every step in the window", () => {
    const r = ratesOf([
      snap(0, adapter({ rx_bytes: 0 })),
      snap(1000, adapter({ rx_bytes: 1000 })),
      snap(2000, adapter({ rx_bytes: 4000 })),
    ])!;
    expect(r.get(1)!.rx).toBe(2000); // 4000 bytes over 2 s
  });

  it("skips a step where the counters went backwards (adapter reset) instead of reporting nonsense", () => {
    const r = ratesOf([
      snap(0, adapter({ rx_bytes: 9_000_000, tx_bytes: 9_000_000 })),
      snap(1000, adapter({ rx_bytes: 100, tx_bytes: 100 })), // reset
      snap(2000, adapter({ rx_bytes: 1100, tx_bytes: 100 })),
    ]);
    expect(r.get(1)).toEqual({ rx: 1000, tx: 0 });
  });

  it("gives an adapter that has just appeared no rate, and ignores one that vanished", () => {
    const r = ratesOf([
      snap(0, adapter({ luid: 1, rx_bytes: 0 }), adapter({ luid: 2, name: "Old" })),
      snap(1000, adapter({ luid: 1, rx_bytes: 500 }), adapter({ luid: 3, name: "New" })),
    ]);
    expect(r.get(1)!.rx).toBe(500);
    expect(r.get(3)).toEqual({ rx: null, tx: null });
    expect(r.has(2)).toBe(false);
  });

  it("ignores readings with no time between them", () => {
    const r = ratesOf([snap(5, adapter({ rx_bytes: 0 })), snap(5, adapter({ rx_bytes: 999 }))]);
    expect(r.get(1)).toEqual({ rx: null, tx: null });
  });

  it("copes with counters beyond 2^32 (a busy adapter after weeks of uptime)", () => {
    const big = 7_615_677_666;
    const r = ratesOf([snap(0, adapter({ rx_bytes: big })), snap(1000, adapter({ rx_bytes: big + 12_345_678 }))]);
    expect(r.get(1)!.rx).toBe(12_345_678);
  });
});

describe("which adapters are shown", () => {
  it("shows what the backend marks primary and hides the rest", () => {
    expect(isPrimary(adapter())).toBe(true);
    expect(isPrimary(adapter({ hardware: false, primary: false, name: "Local Area Connection* 6" }))).toBe(false);
  });

  it("sorts connected first, then hardware, then by name", () => {
    const list = [
      adapter({ luid: 1, name: "Zed", oper_status: 2 }),
      adapter({ luid: 2, name: "Beta", oper_status: 1, hardware: false }),
      adapter({ luid: 3, name: "Alpha", oper_status: 1 }),
    ];
    expect(sortAdapters(list).map((a) => a.name)).toEqual(["Alpha", "Beta", "Zed"]);
  });
});

describe("statusOf", () => {
  it("distinguishes a missing cable from a disabled adapter", () => {
    expect(statusOf(adapter({ oper_status: 1 })).label).toBe("Connected");
    expect(statusOf(adapter({ oper_status: 2, media_connected: false })).label).toBe("Cable unplugged or no link");
    expect(statusOf(adapter({ oper_status: 2, media_connected: true })).label).toBe("Disabled or down");
    expect(statusOf(adapter({ oper_status: 2, media_connected: false, medium: "bluetooth" })).label).toBe("Not connected");
    expect(statusOf(adapter({ oper_status: 99 })).label).toBe("Unknown");
  });
});

describe("formatting", () => {
  it("quotes link speed in bits", () => {
    expect(linkSpeed(1e9)).toBe("1 Gbps");
    expect(linkSpeed(2.5e9)).toBe("2.5 Gbps");
    expect(linkSpeed(100e6)).toBe("100 Mbps");
    expect(linkSpeed(3e6)).toBe("3 Mbps");
    expect(linkSpeed(64_000)).toBe("64 kbps");
    expect(linkSpeed(null)).toBe("–");
  });

  it("describes errors and drops only when there are some", () => {
    expect(problems(adapter())).toBeNull();
    expect(problems(adapter({ rx_discards: 187686 }))).toBe("187,686 dropped");
    expect(problems(adapter({ rx_errors: 2, tx_errors: 1, tx_discards: 4 }))).toBe("3 errors, 4 dropped");
  });
});

describe("totalRate", () => {
  it("adds up connected adapters that have a rate and ignores the rest", () => {
    const a = adapter({ luid: 1 });
    const b = adapter({ luid: 2, oper_status: 2 });
    const c = adapter({ luid: 3 });
    const rates = new Map([
      [1, { rx: 100, tx: 10 }],
      [2, { rx: 999, tx: 999 }],
      [3, { rx: null, tx: null }],
    ]);
    expect(totalRate([a, b, c], rates)).toEqual({ rx: 100, tx: 10 });
    expect(totalRate([b], rates)).toEqual({ rx: null, tx: null });
  });
});
