import type { ActivityWindow } from "./activityWindow";
import { micros, percent } from "./format";

const SHOWN = 15;

export function DriversView({ win }: { win: ActivityWindow | null }) {
  if (!win) {
    return <p className="dim" role="status">Starting the trace… the first reading takes a couple of seconds.</p>;
  }
  const windowUs = win.seconds * 1_000_000;
  // Share of the machine's total CPU capacity (all logical CPUs together).
  const capacityUs = windowUs * win.logicalCpus;
  const busy = (d: { dpc_time_us: number; isr_time_us: number }) => d.dpc_time_us + d.isr_time_us;

  const drivers = [...win.drivers].sort((a, b) => busy(b) - busy(a));
  const total = drivers.reduce((n, d) => n + busy(d), 0);
  const perSec = (n: number) => {
    const v = n / win.seconds;
    return v < 10 ? v.toFixed(1) : Math.round(v).toLocaleString();
  };

  return (
    <section aria-labelledby="drv-h" className="card">
      <h2 id="drv-h">Driver CPU time</h2>
      <p className="lede">
        Over the last {Math.round(win.seconds)} seconds, drivers spent <strong>{percent(total, capacityUs)}</strong> of
        the PC's total CPU capacity ({win.logicalCpus} logical CPUs) answering hardware. Task Manager counts this
        under "System".
      </p>

      <table className="plain procs">
        <thead>
          <tr>
            <th scope="col">Driver</th>
            <th scope="col" className="num">CPU share</th>
            <th scope="col" className="num">Deferred work (DPC)</th>
            <th scope="col" className="num">Longest DPC</th>
            <th scope="col" className="num">Interrupts (ISR)</th>
            <th scope="col" className="num">Longest ISR</th>
          </tr>
        </thead>
        <tbody>
          {drivers.slice(0, SHOWN).map((d) => (
            <tr key={d.name}>
              <th scope="row">{d.name === "?" ? "(unidentified code)" : d.name}</th>
              <td className="num">{percent(busy(d), capacityUs)}</td>
              <td className="num">{perSec(d.dpc_calls)}/s</td>
              <td className="num dim">{micros(d.dpc_max_us)}</td>
              <td className="num">{perSec(d.isr_calls)}/s</td>
              <td className="num dim">{micros(d.isr_max_us)}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <p className="note">
        A <strong>DPC</strong> is work a driver defers until after an interrupt; an <strong>ISR</strong> is the
        interrupt handler itself. A long single call can stall other work on that CPU, so the "longest" columns
        matter as much as the share. These times are estimates: on the verified Windows build they landed within
        roughly 30% of Windows' own counters.
      </p>
      {win.health.events_lost > 0 && (
        <p className="banner error" role="alert">
          Windows dropped {win.health.events_lost.toLocaleString()} trace events, so these figures may be too low.
        </p>
      )}
      {win.health.durations_dropped > 0 && (
        <p className="note">{win.health.durations_dropped} readings were discarded as implausible.</p>
      )}
    </section>
  );
}
