import type { ExcludedRange, PortEndpoint, PortOwner, PortService, PortsSnapshot } from "./api";

/** An endpoint with the program that owns it filled in. */
export interface PortRow extends PortEndpoint {
  program: string;
  path: string | null;
  services: PortService[];
}

export type PortFilter = "listening" | "connections" | "all";

/** Open: traffic can still flow. Closing: the connection is over or ending and Windows is tidying up. */
export type ConnectionFilter = "open" | "closing" | "all";

/** A TCP listener or a bound UDP socket: something other programs can send to. */
export function isListening(e: PortEndpoint): boolean {
  return e.proto === "udp" || e.state === "listen";
}

/** Everything else: a TCP connection in any state. */
export function isConnection(e: PortEndpoint): boolean {
  return e.proto === "tcp" && e.state !== "listen";
}

/** Windows gives PID 0 to connections in TIME_WAIT and similar, which no program owns any more. */
export function programName(pid: number, owner: PortOwner | undefined): string {
  if (pid === 0) return "No program (connection closing)";
  if (pid === 4) return "System";
  return owner?.name ?? `Unknown program (pid ${pid})`;
}

export function rowsOf(snap: PortsSnapshot): PortRow[] {
  const byPid = new Map(snap.owners.map((o) => [o.pid, o]));
  return snap.endpoints.map((e) => {
    const o = byPid.get(e.pid);
    return { ...e, program: programName(e.pid, o), path: o?.path ?? null, services: o?.services ?? [] };
  });
}

/** How many services are named in full before the rest are summed up ("and 5 more"). */
const SERVICES_SHOWN = 3;

/**
 * The services inside a process, in words, for the line under the program name. One service is stated
 * outright; several mean "one of these", because Windows does not say which one owns a given socket
 * without administrator rights. `title` always lists them all. Null for an ordinary program.
 */
export function serviceLabel(services: PortService[]): { text: string; title: string } | null {
  if (services.length === 0) return null;
  const full = services.map((s) => `${s.display} (${s.name})`).join("\n");
  if (services.length === 1) return { text: `Service: ${services[0].display} (${services[0].name})`, title: full };
  const shown = services.slice(0, SERVICES_SHOWN).map((s) => s.name).join(", ");
  const more = services.length - SERVICES_SHOWN;
  return { text: `One of ${services.length} services: ${shown}${more > 0 ? ` and ${more} more` : ""}`, title: full };
}

/**
 * Applies the listening/connections chip and the search box. The search matches the port number
 * exactly or as a prefix ("80" finds 80 and 8080 but not 180), and program names and paths by
 * substring, ignoring case.
 */
export function filterRows(rows: PortRow[], filter: PortFilter, query: string): PortRow[] {
  const q = query.trim().toLowerCase();
  return rows.filter((r) => {
    if (filter === "listening" && !isListening(r)) return false;
    if (filter === "connections" && !isConnection(r)) return false;
    if (!q) return true;
    if (/^\d+$/.test(q)) return String(r.local_port).startsWith(q) || String(r.remote_port ?? "").startsWith(q);
    return (
      r.program.toLowerCase().includes(q) ||
      (r.path ?? "").toLowerCase().includes(q) ||
      r.local_addr.includes(q) ||
      r.services.some((s) => s.name.toLowerCase().includes(q) || s.display.toLowerCase().includes(q))
    );
  });
}

/** By port, then TCP before UDP, then IPv4 before IPv6, so rows stay put between readings. */
export function sortRows(rows: PortRow[]): PortRow[] {
  return [...rows].sort(
    (a, b) =>
      a.local_port - b.local_port ||
      (a.proto === b.proto ? 0 : a.proto === "tcp" ? -1 : 1) ||
      Number(a.v6) - Number(b.v6) ||
      a.pid - b.pid ||
      (a.remote_port ?? 0) - (b.remote_port ?? 0),
  );
}

const ANY = new Set(["0.0.0.0", "::"]);
const LOCAL_ONLY = (a: string) => a === "::1" || a.startsWith("127.");

/** Who can reach a listening socket, in words. Windows' firewall can still block any of it. */
export function reach(addr: string): { label: string; exposed: boolean } {
  if (ANY.has(addr)) return { label: "Any address", exposed: true };
  if (LOCAL_ONLY(addr)) return { label: "This PC only", exposed: false };
  return { label: addr, exposed: true };
}

/** "listen" -> "Listening", "fin_wait_1" -> "Fin wait 1". */
export function stateLabel(state: string | null): string {
  if (state === null) return "";
  if (state === "listen") return "Listening";
  const s = state.replace(/_/g, " ");
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** "[::1]:80" for IPv6, "127.0.0.1:80" for IPv4. */
export function hostPort(addr: string, port: number | null): string {
  const host = addr.includes(":") ? `[${addr}]` : addr;
  return port === null ? host : `${host}:${port}`;
}

/** The reserved ranges that contain `port`, for either protocol. */
export function reservedFor(port: number, ranges: ExcludedRange[]): ExcludedRange[] {
  return ranges.filter((r) => port >= r.start && port <= r.end);
}

/** The whole-number port the search box holds, or null when it holds anything else. */
export function queryPort(query: string): number | null {
  const q = query.trim();
  if (!/^\d{1,5}$/.test(q)) return null;
  const n = Number(q);
  return n >= 1 && n <= 65535 ? n : null;
}

/** "50000–50059", or just "2869" for a range of one. */
export function rangeLabel(r: ExcludedRange): string {
  return r.start === r.end ? String(r.start) : `${r.start}–${r.end}`;
}

export interface PortCounts {
  listening: number;
  connections: number;
  programs: number;
}

export function countsOf(rows: PortRow[]): PortCounts {
  const listening = rows.filter(isListening);
  return {
    listening: listening.length,
    connections: rows.filter(isConnection).length,
    programs: new Set(listening.filter((r) => r.pid > 4).map((r) => r.pid)).size,
  };
}

const ENDING = new Set(["time_wait", "fin_wait_1", "fin_wait_2", "closing", "last_ack", "closed", "delete_tcb"]);

/** A TCP connection that is over or on its way out (TIME_WAIT and friends), as opposed to a live one. */
export function isEnding(e: PortEndpoint): boolean {
  return e.state !== null && ENDING.has(e.state);
}

/** The other end is this PC itself (loopback), so the traffic never leaves it. */
export function isLocalPeer(e: PortEndpoint): boolean {
  return e.remote_addr !== null && !reach(e.remote_addr).exposed;
}

/**
 * The Connections view's filter: TCP connections only, narrowed by open/closing, optionally without
 * the ones between programs on this PC, then the same search as the Ports view.
 */
export function filterConnections(rows: PortRow[], filter: ConnectionFilter, outsideOnly: boolean, query: string): PortRow[] {
  return filterRows(rows, "connections", query).filter((r) => {
    if (filter === "open" && isEnding(r)) return false;
    if (filter === "closing" && !isEnding(r)) return false;
    return !(outsideOnly && isLocalPeer(r));
  });
}

/** Grouped by program so one program's connections sit together, then by local port and peer. */
export function sortConnections(rows: PortRow[]): PortRow[] {
  return [...rows].sort(
    (a, b) =>
      a.program.localeCompare(b.program) ||
      a.pid - b.pid ||
      a.local_port - b.local_port ||
      (a.remote_addr ?? "").localeCompare(b.remote_addr ?? "") ||
      (a.remote_port ?? 0) - (b.remote_port ?? 0),
  );
}

export interface ConnectionCounts {
  open: number;
  closing: number;
  programs: number;
  /** Distinct remote addresses of open connections that leave this PC. */
  remotes: number;
}

export function connectionCounts(rows: PortRow[]): ConnectionCounts {
  const conns = rows.filter(isConnection);
  const open = conns.filter((r) => !isEnding(r));
  return {
    open: open.length,
    closing: conns.length - open.length,
    programs: new Set(open.filter((r) => r.pid > 4).map((r) => r.pid)).size,
    remotes: new Set(open.filter((r) => !isLocalPeer(r)).map((r) => r.remote_addr)).size,
  };
}
