import { useMemo, useState } from "react";
import { bytes, rate } from "./format";
import { isPrimary, linkSpeed, MEDIUM_LABEL, problems, ratesOf, sortAdapters, statusOf, totalRate } from "./network";
import type { NetworkState } from "./useNetwork";

/** "1.2 MB/s", or the word "idle" for nothing at all. */
function perSecond(n: number): string {
  const r = rate(n, 1);
  return r === "–" ? "idle" : r;
}

/**
 * Network adapters and their traffic. Read by the app itself, so it works without the collector
 * and without administrator rights: `standalone` is the collector-not-running case. The readings
 * come from one shared poller (`useNetwork` in App) so the overview and this view agree.
 */
export function NetworkView({ net, standalone }: { net: NetworkState; standalone?: boolean }) {
  const { history, error, check } = net;
  const [showSoftware, setShowSoftware] = useState(false);

  const latest = history[history.length - 1];
  const rates = useMemo(() => ratesOf(history), [history]);

  if (!latest) {
    return (
      <section aria-labelledby="net-h" className="card">
        <h2 id="net-h">Network</h2>
        {error ? <p className="banner error" role="alert">{error}</p> : <p className="dim" role="status">Reading the adapters…</p>}
      </section>
    );
  }

  const all = sortAdapters(latest.adapters);
  const primary = all.filter(isPrimary);
  const hidden = all.length - primary.length;
  const shown = showSoftware ? all : primary;
  const total = totalRate(primary, rates);

  return (
    <section aria-labelledby="net-h" className="card">
      <h2 id="net-h">Network</h2>
      <p className="lede">
        {total.rx === null || total.tx === null ? (
          "Measuring…"
        ) : (
          <>
            Right now: <strong>{perSecond(total.rx)}</strong> received, <strong>{perSecond(total.tx)}</strong> sent,
            across all connected adapters.
          </>
        )}
        {standalone && " This part works without the collector."}
      </p>

      {check && !check.ok && (
        <p className="banner error" role="alert">
          Check failed: {check.detail}. The numbers below may be wrong.
        </p>
      )}
      {error && <p className="banner error" role="alert">{error}</p>}

      <table className="plain procs">
        <thead>
          <tr>
            <th scope="col">Adapter</th>
            <th scope="col">Status</th>
            <th scope="col" className="num">Link speed</th>
            <th scope="col" className="num">Receiving</th>
            <th scope="col" className="num">Sending</th>
            <th scope="col" className="num">Total received</th>
            <th scope="col" className="num">Total sent</th>
          </tr>
        </thead>
        <tbody>
          {shown.map((a) => {
            const st = statusOf(a);
            const r = rates.get(a.luid);
            const trouble = problems(a);
            return (
              <tr key={a.luid}>
                <th scope="row">
                  {a.name}
                  <span className="pid dim"> {MEDIUM_LABEL[a.medium]}</span>
                  <span className="sub dim">{a.description}</span>
                  {trouble && <span className="sub warn">{trouble} (running total)</span>}
                </th>
                <td>
                  <span className={`dot ${st.tone}`} aria-hidden="true" /> {st.label}
                </td>
                <td className="num">{a.oper_status === 1 ? linkSpeed(a.rx_speed_bps) : "–"}</td>
                <td className="num">{r && r.rx !== null ? rate(r.rx, 1) : "…"}</td>
                <td className="num">{r && r.tx !== null ? rate(r.tx, 1) : "…"}</td>
                <td className="num">{bytes(a.rx_bytes)}</td>
                <td className="num">{bytes(a.tx_bytes)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>

      {hidden > 0 && (
        <p className="sortbar">
          <button className={showSoftware ? "chip on" : "chip"} aria-pressed={showSoftware} onClick={() => setShowSoftware((v) => !v)}>
            {showSoftware ? "Hide" : "Show"} {hidden} software adapter{hidden === 1 ? "" : "s"}
          </button>
          <span>VPN, tunnel and "WAN Miniport" entries that Windows lists but that are not network cards.</span>
        </p>
      )}

      <p className="note">
        Totals are Windows' own running counts for each adapter and cover every program together; they can start
        again from zero when an adapter is reset or Windows restarts. Receiving and sending are averaged over the
        last few seconds, in bytes per second; link speed is in bits per second, as Windows quotes it.
        {check?.ok && ` Check passed: ${check.detail}.`}
      </p>
    </section>
  );
}
