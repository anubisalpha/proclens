import { useMemo, useState } from "react";
import { connectionCounts, filterConnections, hostPort, reach, rowsOf, serviceLabel, sortConnections, stateLabel } from "./ports";
import type { ConnectionFilter } from "./ports";
import { ConnectionLogCard } from "./ConnectionLogCard";
import type { ConnLogState } from "./useConnLog";
import type { PortsState } from "./usePorts";

/** Rows shown before "show all": a busy PC has hundreds of connections. */
const PAGE = 100;

const FILTERS: { id: ConnectionFilter; label: string }[] = [
  { id: "open", label: "Open" },
  { id: "closing", label: "Closing" },
  { id: "all", label: "All" },
];

/**
 * Live TCP connections: which program talks to which address and port. Read by the app itself, so
 * it works without the collector and without administrator rights. Remote addresses are shown
 * here and nowhere else: the activity log never records them and nothing is looked up.
 */
export function ConnectionsView({ ports, connlog, standalone }: { ports: PortsState; connlog: ConnLogState; standalone?: boolean }) {
  const { snapshot, error, refresh } = ports;
  const [filter, setFilter] = useState<ConnectionFilter>("open");
  const [outsideOnly, setOutsideOnly] = useState(false);
  const [query, setQuery] = useState("");
  const [all, setAll] = useState(false);

  const rows = useMemo(() => (snapshot ? rowsOf(snapshot) : []), [snapshot]);
  const counts = useMemo(() => connectionCounts(rows), [rows]);
  const shown = useMemo(() => sortConnections(filterConnections(rows, filter, outsideOnly, query)), [rows, filter, outsideOnly, query]);
  const visible = all ? shown : shown.slice(0, PAGE);

  if (!snapshot) {
    return (
      <section aria-labelledby="conn-h" className="card">
        <h2 id="conn-h">Connections</h2>
        {error ? <p className="banner error" role="alert">{error}</p> : <p className="dim" role="status">Reading the connection table…</p>}
      </section>
    );
  }

  return (
    <>
    <section aria-labelledby="conn-h" className="card">
      <h2 id="conn-h">Connections</h2>
      <p className="lede">
        <strong>{counts.open}</strong> open connection{counts.open === 1 ? "" : "s"} from <strong>{counts.programs}</strong>{" "}
        program{counts.programs === 1 ? "" : "s"} to <strong>{counts.remotes}</strong> address{counts.remotes === 1 ? "" : "es"}{" "}
        outside this PC, and <strong>{counts.closing}</strong> more closing down.
        {standalone && " This part works without the collector."}
      </p>
      {error && <p className="banner error" role="alert">{error}</p>}
      {snapshot.services_error && (
        <p className="banner error" role="alert">{snapshot.services_error}. Services inside shared processes are not shown.</p>
      )}

      <p className="sortbar">
        {FILTERS.map((f) => (
          <button key={f.id} className={filter === f.id ? "chip on" : "chip"} aria-pressed={filter === f.id} onClick={() => setFilter(f.id)}>
            {f.label}
          </button>
        ))}
        <button className={outsideOnly ? "chip on" : "chip"} aria-pressed={outsideOnly} onClick={() => setOutsideOnly((v) => !v)}>
          Only outside this PC
        </button>
        <input
          type="search"
          className="search"
          aria-label="Find a connection by program, path or port"
          placeholder="Program or port"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <button className="secondary" onClick={refresh}>Refresh</button>
      </p>

      <table className="plain procs ports">
        <thead>
          <tr>
            <th scope="col">Program</th>
            <th scope="col">Local</th>
            <th scope="col">Remote</th>
            <th scope="col">State</th>
          </tr>
        </thead>
        <tbody>
          {visible.map((r, i) => (
            // The position is part of the key: nothing stops Windows listing two identical rows.
            <tr key={`${i}-${r.local_addr}-${r.local_port}-${r.remote_addr}-${r.remote_port}-${r.pid}`}>
              <th scope="row">
                {r.program}
                <span className="pid dim">pid {r.pid}</span>
                {r.path && <span className="sub dim">{r.path}</span>}
                    {serviceLabel(r.services) && (
                      <span className="sub dim" title={serviceLabel(r.services)!.title}>{serviceLabel(r.services)!.text}</span>
                    )}
              </th>
              <td>{hostPort(r.local_addr, r.local_port)}</td>
              <td>
                {r.remote_addr !== null && hostPort(r.remote_addr, r.remote_port)}
                {r.remote_addr !== null && !reach(r.remote_addr).exposed && <span className="sub dim">This PC</span>}
              </td>
              <td>{stateLabel(r.state)}</td>
            </tr>
          ))}
          {shown.length === 0 && (
            <tr>
              <td colSpan={4} className="dim">Nothing matches.</td>
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
        Rows are grouped by program. "Open" connections can still carry data; "Closing" ones are over or ending, and
        Windows keeps them listed for a short while (the program shown as "No program" has already let go of them).
        Remote addresses are shown as Windows reports them: nothing is looked up, and they are never written to the
        activity log (only to the separate connection log below, if you switch it on).
      </p>
    </section>
    <ConnectionLogCard log={connlog} />
    </>
  );
}
