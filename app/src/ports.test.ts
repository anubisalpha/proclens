import { describe, expect, it } from "vitest";
import type { ExcludedRange, PortEndpoint, PortsSnapshot } from "./api";
import { connectionCounts, serviceLabel, countsOf, filterConnections, filterRows, isEnding, hostPort, queryPort, rangeLabel, reach, reservedFor, rowsOf, sortConnections, sortRows, stateLabel } from "./ports";

const ep = (over: Partial<PortEndpoint> = {}): PortEndpoint => ({
  proto: "tcp", v6: false, local_addr: "0.0.0.0", local_port: 80, remote_addr: null, remote_port: null, state: "listen", pid: 10, ...over,
});

const snap = (endpoints: PortEndpoint[]): PortsSnapshot => ({
  endpoints,
  owners: [
    { pid: 10, name: "nginx.exe", path: "C:\\srv\\nginx.exe", services: [] },
    { pid: 20, name: "chrome.exe", path: null, services: [] },
    { pid: 30, name: "svchost.exe", path: null, services: [{ name: "Dnscache", display: "DNS Client" }] },
    {
      pid: 31,
      name: "svchost.exe",
      path: null,
      services: ["RpcEptMapper", "RpcSs", "Schedule", "EventLog", "Dnscache"].map((n) => ({ name: n, display: n.toUpperCase() })),
    },
  ],
  services_error: null,
});

describe("rowsOf", () => {
  it("fills in the program from the owner list", () => {
    const [r] = rowsOf(snap([ep()]));
    expect(r.program).toBe("nginx.exe");
    expect(r.path).toBe("C:\\srv\\nginx.exe");
  });

  it("keeps a name when the path is unreadable, and says so when even the name is gone", () => {
    const rows = rowsOf(snap([ep({ pid: 20 }), ep({ pid: 999 })]));
    expect(rows[0]).toMatchObject({ program: "chrome.exe", path: null });
    expect(rows[1].program).toBe("Unknown program (pid 999)");
  });

  it("names the two pids that are not programs", () => {
    const rows = rowsOf(snap([ep({ pid: 0, state: "time_wait", remote_addr: "1.1.1.1", remote_port: 1 }), ep({ pid: 4 })]));
    expect(rows[0].program).toMatch(/closing/);
    expect(rows[1].program).toBe("System");
  });
});

describe("filterRows", () => {
  const rows = rowsOf(
    snap([
      ep({ local_port: 80 }),
      ep({ local_port: 8080, pid: 20 }),
      ep({ local_port: 180 }),
      ep({ proto: "udp", local_port: 5353, state: null, pid: 20 }),
      ep({ local_port: 51000, state: "established", remote_addr: "140.82.112.3", remote_port: 443, pid: 20 }),
    ]),
  );

  it("listening means TCP listeners and UDP sockets, connections means the rest", () => {
    expect(filterRows(rows, "listening", "").map((r) => r.local_port)).toEqual([80, 8080, 180, 5353]);
    expect(filterRows(rows, "connections", "").map((r) => r.local_port)).toEqual([51000]);
    expect(filterRows(rows, "all", "")).toHaveLength(5);
  });

  it("a number matches the start of a port, not the middle", () => {
    expect(filterRows(rows, "all", "80").map((r) => r.local_port)).toEqual([80, 8080]);
  });

  it("a number also finds the remote port of a connection", () => {
    expect(filterRows(rows, "all", "443").map((r) => r.local_port)).toEqual([51000]);
  });

  it("text matches program and path, ignoring case", () => {
    expect(filterRows(rows, "all", "CHROME")).toHaveLength(3);
    expect(filterRows(rows, "all", "srv\\ngi").map((r) => r.local_port)).toEqual([80, 180]);
    expect(filterRows(rows, "all", "nothing-like-this")).toEqual([]);
  });
});

describe("sortRows", () => {
  it("orders by port, then TCP before UDP, then IPv4 before IPv6, and does not change its input", () => {
    const input = rowsOf(
      snap([
        ep({ local_port: 90 }),
        ep({ proto: "udp", local_port: 80, state: null }),
        ep({ v6: true, local_addr: "::", local_port: 80 }),
        ep({ local_port: 80 }),
      ]),
    );
    const before = input.map((r) => r.local_port);
    const out = sortRows(input);
    expect(out.map((r) => `${r.proto}${r.v6 ? "6" : "4"}:${r.local_port}`)).toEqual(["tcp4:80", "tcp6:80", "udp4:80", "tcp4:90"]);
    expect(input.map((r) => r.local_port)).toEqual(before);
  });
});

describe("reach", () => {
  it("any-address listeners are exposed, loopback ones are not", () => {
    expect(reach("0.0.0.0")).toEqual({ label: "Any address", exposed: true });
    expect(reach("::")).toEqual({ label: "Any address", exposed: true });
    expect(reach("127.0.0.1")).toEqual({ label: "This PC only", exposed: false });
    expect(reach("127.5.5.5").exposed).toBe(false);
    expect(reach("::1").exposed).toBe(false);
    expect(reach("192.168.1.10")).toEqual({ label: "192.168.1.10", exposed: true });
  });
});

describe("labels", () => {
  it("states read as words", () => {
    expect(stateLabel("listen")).toBe("Listening");
    expect(stateLabel("fin_wait_1")).toBe("Fin wait 1");
    expect(stateLabel(null)).toBe("");
  });

  it("IPv6 hosts get brackets", () => {
    expect(hostPort("::1", 80)).toBe("[::1]:80");
    expect(hostPort("10.0.0.1", 443)).toBe("10.0.0.1:443");
    expect(hostPort("10.0.0.1", null)).toBe("10.0.0.1");
  });
});

describe("reserved ranges", () => {
  const ranges: ExcludedRange[] = [
    { proto: "tcp", start: 2869, end: 2869, administered: false },
    { proto: "tcp", start: 50000, end: 50059, administered: true },
    { proto: "udp", start: 50000, end: 50059, administered: true },
  ];

  it("finds every range that holds a port, either end included", () => {
    expect(reservedFor(50000, ranges)).toHaveLength(2);
    expect(reservedFor(50059, ranges)).toHaveLength(2);
    expect(reservedFor(50060, ranges)).toHaveLength(0);
    expect(reservedFor(2869, ranges)).toHaveLength(1);
  });

  it("labels one-port ranges as a single number", () => {
    expect(rangeLabel(ranges[0])).toBe("2869");
    expect(rangeLabel(ranges[1])).toBe("50000–50059");
  });

  it("only a whole port number in the search box counts as a port question", () => {
    expect(queryPort("8080")).toBe(8080);
    expect(queryPort(" 22 ")).toBe(22);
    expect(queryPort("0")).toBeNull();
    expect(queryPort("65536")).toBeNull();
    expect(queryPort("123456")).toBeNull();
    expect(queryPort("nginx")).toBeNull();
    expect(queryPort("80a")).toBeNull();
  });
});

describe("countsOf", () => {
  it("counts listeners, connections and the distinct programs holding listeners", () => {
    const rows = rowsOf(
      snap([
        ep({ local_port: 80 }),
        ep({ local_port: 81 }),
        ep({ local_port: 5353, proto: "udp", state: null, pid: 20 }),
        ep({ local_port: 135, pid: 4 }),
        ep({ local_port: 51000, state: "established", remote_addr: "1.1.1.1", remote_port: 443, pid: 20 }),
      ]),
    );
    expect(countsOf(rows)).toEqual({ listening: 4, connections: 1, programs: 2 });
  });
});

describe("connections", () => {
  const conn = (over: Partial<PortEndpoint>) =>
    ep({ state: "established", remote_addr: "140.82.112.3", remote_port: 443, local_addr: "192.168.1.5", local_port: 51000, pid: 20, ...over });
  const rows = rowsOf(
    snap([
      ep({ local_port: 80 }), // a listener is not a connection
      ep({ proto: "udp", local_port: 5353, state: null }), // nor is a UDP socket
      conn({ local_port: 51000 }),
      conn({ local_port: 51001, state: "time_wait", pid: 0 }),
      conn({ local_port: 51002, state: "close_wait" }),
      conn({ local_port: 51003, remote_addr: "127.0.0.1", remote_port: 5173, local_addr: "127.0.0.1", pid: 10 }),
      conn({ local_port: 51004, remote_addr: "::1", remote_port: 5173, local_addr: "::1", v6: true, pid: 10 }),
    ]),
  );

  it("open leaves out connections that are ending; closing is only those", () => {
    expect(isEnding(rows[3])).toBe(true);
    expect(isEnding(rows[2])).toBe(false);
    expect(filterConnections(rows, "open", false, "").map((r) => r.local_port)).toEqual([51000, 51002, 51003, 51004]);
    expect(filterConnections(rows, "closing", false, "").map((r) => r.local_port)).toEqual([51001]);
    expect(filterConnections(rows, "all", false, "")).toHaveLength(5);
  });

  it("close_wait still counts as open: the program has not closed its end yet", () => {
    expect(isEnding(rows[4])).toBe(false);
  });

  it("outside-only drops the connections between programs on this PC, IPv4 and IPv6", () => {
    expect(filterConnections(rows, "open", true, "").map((r) => r.local_port)).toEqual([51000, 51002]);
  });

  it("the search applies on top", () => {
    expect(filterConnections(rows, "all", false, "chrome")).toHaveLength(2); // the pid-0 row has no program name
    expect(filterConnections(rows, "all", false, "5173").map((r) => r.local_port)).toEqual([51003, 51004]);
  });

  it("sorts by program, then port, and does not change its input", () => {
    const order = rows.map((r) => r.local_port);
    const out = sortConnections(filterConnections(rows, "all", false, ""));
    expect(out.map((r) => r.program)).toEqual(["chrome.exe", "chrome.exe", "nginx.exe", "nginx.exe", "No program (connection closing)"]);
    expect(out.map((r) => r.local_port)).toEqual([51000, 51002, 51003, 51004, 51001]);
    expect(rows.map((r) => r.local_port)).toEqual(order);
  });

  it("counts open and closing connections, programs, and distinct outside peers", () => {
    expect(connectionCounts(rows)).toEqual({ open: 4, closing: 1, programs: 2, remotes: 1 });
  });
});

describe("services", () => {
  it("rows carry the services of their process", () => {
    const rows = rowsOf(snap([ep({ pid: 30 }), ep({ pid: 10, local_port: 81 })]));
    expect(rows[0].services.map((s) => s.name)).toEqual(["Dnscache"]);
    expect(rows[1].services).toEqual([]);
  });

  it("a single service is stated outright", () => {
    const l = serviceLabel([{ name: "Dnscache", display: "DNS Client" }])!;
    expect(l.text).toBe("Service: DNS Client (Dnscache)");
  });

  it("several services are 'one of', capped, with every one in the tooltip", () => {
    const rows = rowsOf(snap([ep({ pid: 31 })]));
    const l = serviceLabel(rows[0].services)!;
    expect(l.text).toBe("One of 5 services: RpcEptMapper, RpcSs, Schedule and 2 more");
    expect(l.title.split("\n")).toHaveLength(5);
    expect(l.title).toContain("EventLog");
  });

  it("an ordinary program has no label", () => {
    expect(serviceLabel([])).toBeNull();
  });

  it("the search finds a service by short or display name, ignoring case", () => {
    const rows = rowsOf(snap([ep({ pid: 30, local_port: 53 }), ep({ pid: 10, local_port: 80 })]));
    expect(filterRows(rows, "all", "dnscache").map((r) => r.local_port)).toEqual([53]);
    expect(filterRows(rows, "all", "dns client").map((r) => r.local_port)).toEqual([53]);
    expect(filterRows(rows, "all", "svchost").map((r) => r.local_port)).toEqual([53]);
  });
});
