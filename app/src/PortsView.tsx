import { useMemo, useState } from "react";
import { countsOf, filterRows, queryPort, rangeLabel, reach, reservedFor, rowsOf, serviceLabel, sortRows, stateLabel } from "./ports";
import type { PortsState } from "./usePorts";

/** Rows shown before "show all": a busy PC has hundreds of connections. */
const PAGE = 100;

/**
 * Which ports are in use and by which program, plus the ranges Windows keeps back. Read by the app
 * itself, so it works without the collector and without administrator rights; a program Windows
 * protects shows by name only.
 */
export function PortsView({ ports, standalone }: { ports: PortsState; standalone?: boolean }) {
  const { snapshot, excluded, error, refresh } = ports;
  const [query, setQuery] = useState("");
  const [all, setAll] = useState(false);

  const rows = useMemo(() => (snapshot ? rowsOf(snapshot) : []), [snapshot]);
  const counts = useMemo(() => countsOf(rows), [rows]);
  const shown = useMemo(() => sortRows(filterRows(rows, "listening", query)), [rows, query]);

  const asked = queryPort(query);
  const reserved = asked !== null && excluded ? reservedFor(asked, excluded.ranges) : [];
  // Judged over every row, not just the ones the chip shows: a port held only by a connection is still in use.
  const inUse = asked === null ? 0 : rows.filter((r) => r.local_port === asked).length;
  const visible = all ? shown : shown.slice(0, PAGE);

  if (!snapshot) {
    return (
      <section aria-labelledby="ports-h" className="card">
        <h2 id="ports-h">Ports</h2>
        {error ? <p className="banner error" role="alert">{error}</p> : <p className="dim" role="status">Reading the port tables…</p>}
      </section>
    );
  }

  return (
    <>
      <section aria-labelledby="ports-h" className="card">
        <h2 id="ports-h">Ports</h2>
        <p className="lede">
          <strong>{counts.listening}</strong> ports are open for incoming traffic, held by <strong>{counts.programs}</strong>{" "}
          program{counts.programs === 1 ? "" : "s"}. The live connections are on the Connections tab.
          {standalone && " This part works without the collector."}
        </p>
        {error && <p className="banner error" role="alert">{error}</p>}
        {snapshot.services_error && (
          <p className="banner error" role="alert">{snapshot.services_error}. Services inside shared processes are not shown.</p>
        )}

        <p className="sortbar">
          <input
            type="search"
            className="search"
            aria-label="Find a port or program"
            placeholder="Port number or program"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <button className="secondary" onClick={refresh}>Refresh</button>
        </p>

        {asked !== null && (
          <p className={reserved.length ? "banner error" : "banner info"} role="status">
            {inUse === 0 && reserved.length === 0 && <>Nothing is using port {asked} right now.</>}
            {inUse > 0 && <>Port {asked} is in use{shown.length === 0 ? " by a connection only; see the Connections tab" : " (see below)"}. </>}
            {reserved.length > 0 && (
              <>
                Port {asked} is inside a range Windows keeps back ({reserved.map((r) => `${rangeLabel(r)} ${r.proto.toUpperCase()}`).join(", ")}):
                no program can use it, even though nothing is shown holding it.
              </>
            )}
          </p>
        )}

        <table className="plain procs ports">
          <thead>
            <tr>
              <th scope="col" className="num">Port</th>
              <th scope="col">Protocol</th>
              <th scope="col">Local address</th>
              <th scope="col">Program</th>
              <th scope="col">State</th>
            </tr>
          </thead>
          <tbody>
            {visible.map((r, i) => {
              const rc = reach(r.local_addr);
              return (
                // Windows can list the same socket twice (identical UDP sockets from one program), so the
                // position is part of the key; rows hold no state, so reusing them by position is harmless.
                <tr key={`${i}-${r.proto}-${r.local_addr}-${r.local_port}-${r.remote_addr}-${r.remote_port}-${r.pid}`}>
                  <th scope="row" className="num">{r.local_port}</th>
                  <td>
                    {r.proto.toUpperCase()}
                    <span className="pid dim">{r.v6 ? "IPv6" : "IPv4"}</span>
                  </td>
                  <td>
                    <span className={rc.exposed ? undefined : "dim"}>{rc.label}</span>
                    {rc.label !== r.local_addr && <span className="sub dim">{r.local_addr}</span>}
                  </td>
                  <td>
                    {r.program}
                    <span className="pid dim">pid {r.pid}</span>
                    {r.path && <span className="sub dim">{r.path}</span>}
                    {serviceLabel(r.services) && (
                      <span className="sub dim" title={serviceLabel(r.services)!.title}>{serviceLabel(r.services)!.text}</span>
                    )}
                  </td>
                  <td>{stateLabel(r.state) || "Bound"}</td>
                </tr>
              );
            })}
            {shown.length === 0 && (
              <tr>
                <td colSpan={5} className="dim">Nothing matches.</td>
              </tr>
            )}
          </tbody>
        </table>

        {shown.length > PAGE && (
          <p className="sortbar">
            <button className="chip" onClick={() => setAll((v) => !v)}>
              {all ? `Show the first ${PAGE}` : `Show all ${shown.length}`}
            </button>
          </p>
        )}

        <p className="note">
          "Any address" means the program accepts connections arriving on any network adapter; Windows Firewall may still
          block them. UDP has no connections, so a UDP row is just a program waiting for packets. Programs Windows protects
          show a name but no path. A shared Windows process such as svchost.exe shows the service or services inside it.
        </p>
      </section>

      <section aria-labelledby="reserved-h" className="card">
        <h2 id="reserved-h">Ports Windows keeps back</h2>
        {excluded === null ? (
          <p className="dim" role="status">Reading the reserved ranges…</p>
        ) : (
          <>
            {excluded.error && <p className="banner error" role="alert">{excluded.error}. The list below may be incomplete.</p>}
            {excluded.ranges.length === 0 && !excluded.error ? (
              <p className="dim">No ranges are reserved.</p>
            ) : (
              <table className="plain procs">
                <thead>
                  <tr>
                    <th scope="col" className="num">Ports</th>
                    <th scope="col">Protocol</th>
                    <th scope="col">Set by</th>
                  </tr>
                </thead>
                <tbody>
                  {excluded.ranges.map((r) => (
                    <tr key={`${r.proto}-${r.start}-${r.end}`}>
                      <th scope="row" className="num">{rangeLabel(r)}</th>
                      <td>{r.proto.toUpperCase()}</td>
                      <td>{r.administered ? "An administrator" : "Windows"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <p className="note">
              A program cannot start listening on a reserved port, and no program is shown holding it. Ranges marked
              "An administrator" were set with netsh; the others were taken by Windows itself.
            </p>
          </>
        )}
      </section>
    </>
  );
}
