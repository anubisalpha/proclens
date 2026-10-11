import { useMemo } from "react";
import type { MemoryStats, ProcessList, SystemProfile } from "./api";
import { failedAmong } from "./capabilities";
import { rate } from "./format";
import { ratesOf, totalRate } from "./network";
import { gb, memoryLevel, memoryUse, summaryLines } from "./summary";
import type { NetworkState } from "./useNetwork";

interface Props {
  connected: boolean;
  memory: MemoryStats | null;
  processes: ProcessList | null;
  profile: SystemProfile | null;
  net: NetworkState;
  onStart: () => void;
  onGoTo: (tab: "memory" | "network" | "pc") => void;
}

function perSecond(n: number | null): string {
  if (n === null) return "…";
  const r = rate(n, 1);
  return r === "–" ? "idle" : r;
}

/** The landing page: a few plain sentences, then the headline numbers. */
export function Overview({ connected, memory, processes, profile, net, onStart, onGoTo }: Props) {
  const latest = net.history[net.history.length - 1];
  const rates = useMemo(() => ratesOf(net.history), [net.history]);
  const primary = latest ? latest.adapters.filter((a) => a.primary) : [];
  const total = totalRate(primary, rates);
  const adaptersUp = primary.filter((a) => a.oper_status === 1).length;
  const failed = profile ? failedAmong(profile, profile.capabilities.map((c) => c.id)).length : 0;

  const lines = summaryLines({ connected, memory, net: total, adaptersUp, failedChecks: failed });
  const use = memory ? memoryUse(memory) : null;
  const level = use ? memoryLevel(use.usedPct) : null;
  const biggest = processes ? [...processes.processes].sort((a, b) => b.working_set - a.working_set)[0] : undefined;

  return (
    <>
      <section aria-labelledby="ov-h" className="card">
        <h2 id="ov-h">Overview</h2>
        {lines.length === 0 ? (
          <p className="dim" role="status">Reading your PC…</p>
        ) : (
          <ul className="summary">
            {lines.map((l) => (
              <li key={l}>{l}</li>
            ))}
          </ul>
        )}
      </section>

      <div className="tiles">
        <section aria-labelledby="tile-mem" className="tile">
          <h3 id="tile-mem">Memory</h3>
          {use && level ? (
            <>
              <p className="big">{Math.round(use.usedPct)}<span className="unit">% in use</span></p>
              <div className="meter" role="img" aria-label={`${Math.round(use.usedPct)} percent of memory in use`}>
                <span className={`fill ${level.level}`} style={{ width: `${use.usedPct}%` }} />
              </div>
              <p className="dim">
                <span className={`dot ${level.level === "off" ? "bad" : level.level}`} aria-hidden="true" /> {level.label} · {gb(use.availableBytes)} available of{" "}
                {gb(use.totalBytes)}
              </p>
              <p className="dim">Committed: {Math.round(use.commitPct)}% of the limit</p>
              {biggest && <p className="dim">Biggest program: {biggest.name} ({gb(biggest.working_set)})</p>}
              <button className="linklike" onClick={() => onGoTo("memory")}>Memory details</button>
            </>
          ) : connected ? (
            <p className="dim" role="status">Waiting for the first reading…</p>
          ) : (
            <>
              <p className="dim">Where all your RAM goes needs the collector, which asks Windows for administrator approval once.</p>
              <button onClick={onStart}>Start collector</button>
            </>
          )}
        </section>

        <section aria-labelledby="tile-net" className="tile">
          <h3 id="tile-net">Network</h3>
          {latest ? (
            <>
              <p className="big">{perSecond(total.rx)}<span className="unit"> received</span></p>
              <p className="big small">{perSecond(total.tx)}<span className="unit"> sent</span></p>
              <p className="dim">
                {adaptersUp === 0 ? "No adapter connected" : `${adaptersUp} adapter${adaptersUp === 1 ? "" : "s"} connected`}
              </p>
              <button className="linklike" onClick={() => onGoTo("network")}>Network details</button>
            </>
          ) : (
            <p className="dim" role="status">{net.error ?? "Reading the adapters…"}</p>
          )}
        </section>

        {profile && (
          <section aria-labelledby="tile-pc" className="tile">
            <h3 id="tile-pc">This PC</h3>
            <p className="big small">
              {profile.verification === "exact" ? "Verified build" : profile.verification === "untested" ? "Untested build" : "Same build, other patch"}
            </p>
            <p className="dim">
              Windows build {profile.os.build}.{profile.os.ubr} · {failed === 0 ? "no failed self-checks" : `${failed} self-check${failed === 1 ? "" : "s"} failed`}
            </p>
            <button className="linklike" onClick={() => onGoTo("pc")}>Details</button>
          </section>
        )}
      </div>
    </>
  );
}
