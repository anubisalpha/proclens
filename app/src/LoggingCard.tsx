import type { LogStatus } from "./api";
import { approxMegabytesPerDay, daysKept, LOG_INTERVALS } from "./summary";

interface Props {
  status: LogStatus | null;
  error: string | null;
  connected: boolean;
  onSet: (enabled: boolean, intervalSeconds?: number) => void;
  onOpenFolder: () => void;
}

/** The switch for the activity log, with what it records and where it goes. Off until the user turns it on. */
export function LoggingCard({ status, error, connected, onSet, onOpenFolder }: Props) {
  if (!status) return null;
  const on = status.enabled;
  const problem = error ?? status.error;
  return (
    <section aria-labelledby="log-h" className="card">
      <h2 id="log-h">Activity log</h2>
      <p className="lede">
        Keeps a record of what this PC was doing, so you can look back at a slow moment afterwards. It is{" "}
        <strong>off</strong> until you switch it on, and it only records while ProcLens is open.
      </p>

      {problem && <p className="banner error" role="alert">{problem}</p>}

      <div className="logrow">
        <button
          role="switch"
          aria-checked={on}
          className={on ? "switch on" : "switch"}
          onClick={() => onSet(!on)}
        >
          <span className="knob" aria-hidden="true" />
          <span className="switch-label">{on ? "Logging is on" : "Logging is off"}</span>
        </button>

        <label className="logint">
          Sample every{" "}
          <select
            value={status.interval_s}
            onChange={(e) => onSet(on, Number(e.target.value))}
            aria-describedby="log-size"
          >
            {LOG_INTERVALS.map((i) => (
              <option key={i.seconds} value={i.seconds}>{i.label}</option>
            ))}
            {!LOG_INTERVALS.some((i) => i.seconds === status.interval_s) && (
              <option value={status.interval_s}>{status.interval_s} seconds</option>
            )}
          </select>
        </label>
        <span id="log-size" className="dim">
          about {approxMegabytesPerDay(status.interval_s)} MB a day, so about{" "}
          {daysKept(status.interval_s, status.keep_days, status.cap_mb)} days are kept
        </span>
      </div>

      {on && (
        <p className="recording" role="status">
          <span className="dot rec" aria-hidden="true" /> Recording to <code>{status.file ?? "…"}</code> ·{" "}
          {status.lines.toLocaleString()} line{status.lines === 1 ? "" : "s"} written{" "}
          <button className="linklike" onClick={onOpenFolder}>Open the log folder</button>
        </p>
      )}

      <p className="note">
        Each line holds memory and network numbers plus the busiest programs, files and drivers, so the file
        contains program names and file paths (never network addresses). It stays on this PC in{" "}
        <code>{status.dir}</code>; one file per day (UTC), kept for {status.keep_days} days and no more than {status.cap_mb.toLocaleString()} MB in total (the oldest go first).{" "}
        {connected
          ? "While logging is on, the kernel trace runs all the time so disk and driver activity can be recorded."
          : "Without the collector the log only has network numbers."}
      </p>
    </section>
  );
}
