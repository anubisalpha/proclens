import type { ConnLogState } from "./useConnLog";

/** Interval choices. Shorter catches shorter connections; a session shorter than the interval can be missed. */
const INTERVALS = [
  { seconds: 5, label: "5 seconds" },
  { seconds: 10, label: "10 seconds" },
  { seconds: 30, label: "30 seconds" },
  { seconds: 60, label: "minute" },
];

/**
 * The switch for the connection log. Separate from the activity log because this one records
 * network addresses; off until the user turns it on, and off again every time the app starts.
 */
export function ConnectionLogCard({ log }: { log: ConnLogState }) {
  const { status, error, set, openFolder } = log;
  if (!status) return null;
  const on = status.enabled;
  const problem = error ?? status.error;
  return (
    <section aria-labelledby="connlog-h" className="card">
      <h2 id="connlog-h">Connection log</h2>
      <p className="lede">
        Records each connection when it ends: which program, which remote address and port, when it started and
        for how long. It is <strong>off</strong> until you switch it on, and it only records while ProcLens is open.
      </p>

      {problem && <p className="banner error" role="alert">{problem}</p>}

      <div className="logrow">
        <button role="switch" aria-checked={on} className={on ? "switch on" : "switch"} onClick={() => set(!on)}>
          <span className="knob" aria-hidden="true" />
          <span className="switch-label">{on ? "Recording connections" : "Not recording"}</span>
        </button>

        <label className="logint">
          Check every{" "}
          <select value={status.interval_s} onChange={(e) => set(on, Number(e.target.value))}>
            {INTERVALS.map((i) => (
              <option key={i.seconds} value={i.seconds}>{i.label}</option>
            ))}
            {!INTERVALS.some((i) => i.seconds === status.interval_s) && <option value={status.interval_s}>{status.interval_s} seconds</option>}
          </select>
        </label>
      </div>

      {on && (
        <p className="recording" role="status">
          <span className="dot rec" aria-hidden="true" /> Recording to <code>{status.file ?? "…"}</code> ·{" "}
          {status.lines.toLocaleString()} line{status.lines === 1 ? "" : "s"} written · {status.open_sessions.toLocaleString()} still open{" "}
          <button className="linklike" onClick={openFolder}>Open the log folder</button>
        </p>
      )}

      <p className="note">
        This log holds <strong>real remote addresses</strong>, so it is kept apart from the activity log, which never
        has any. It stays on this PC in <code>{status.dir}</code> (files named <code>proclens-connections-…</code>, one per
        day in UTC), kept for {status.keep_days} days and no more than {status.cap_mb.toLocaleString()} MB in total.
        Connections to and from this PC itself are left out. Each line also says whether this program has talked to
        that address before ({status.seen_pairs.toLocaleString()} program and address pairs are known so far; on the
        first run everything is new). A connection shorter than the interval can be missed, and times are only as exact
        as the interval. Nothing is looked up. Each check also records how much data the whole PC sent and
        received on each network adapter since the last one (the same counters as the Network tab): it shows how
        busy the network was, not which program was using it.
      </p>
    </section>
  );
}
