import { useEffect, useMemo, useState } from "react";
import { useCollector } from "./api";
import { windowOf } from "./activityWindow";
import { DiskView } from "./DiskView";
import { DriversView } from "./DriversView";
import { Footer } from "./Footer";
import { MemoryView } from "./MemoryView";
import { NetworkView } from "./NetworkView";
import { ProcessTable } from "./ProcessTable";
import { ProfileCard } from "./ProfileCard";
import { useNetwork } from "./useNetwork";

type Tab = "memory" | "disk" | "drivers" | "network" | "pc";

const TABS: { id: Tab; label: string }[] = [
  { id: "memory", label: "Memory" },
  { id: "disk", label: "Disk & files" },
  { id: "drivers", label: "Drivers" },
  { id: "network", label: "Network" },
  { id: "pc", label: "This PC" },
];

export default function App() {
  const { status, memory, processes, profile, activity, error, start, stop, setActivityEnabled } = useCollector();
  const [tab, setTab] = useState<Tab>("memory");
  const connected = status.state === "connected";

  // Adapter counters are read by the app itself; poll them only while a view shows them.
  const net = useNetwork(!connected || tab === "network");

  // The kernel trace is costly, so it only runs while a view that needs it is on screen.
  const wantTrace = connected && (tab === "disk" || tab === "drivers");
  useEffect(() => {
    setActivityEnabled(wantTrace);
  }, [wantTrace, setActivityEnabled]);

  const win = useMemo(() => windowOf(activity), [activity]);

  return (
    <main>
      <header>
        <h1>ProcLens</h1>
        <div className="status" role="status" aria-live="polite">
          {status.state === "connected" && (
            <>
              <span className="dot ok" aria-hidden="true" />
              Collector running{status.elevated ? " as administrator" : " (not elevated)"}
              <button className="secondary" onClick={stop}>Stop collector</button>
            </>
          )}
          {status.state === "connecting" && (
            <>
              <span className="dot wait" aria-hidden="true" />
              Waiting for administrator approval…
            </>
          )}
          {status.state === "disconnected" && (
            <>
              <span className="dot off" aria-hidden="true" />
              Collector not running
              <button onClick={start}>Start collector</button>
            </>
          )}
        </div>
      </header>

      {error && <p className="banner error" role="alert">{error}</p>}
      {status.state === "disconnected" && status.reason && (
        <p className="banner error" role="alert">{status.reason}</p>
      )}

      {!connected && (
        <section className="card intro">
          <h2>See what your PC is really doing</h2>
          <p>
            Windows hides a lot from Task Manager: where all the memory goes, which drivers burn CPU, which
            files a program is hammering. ProcLens reads that from the system itself.
          </p>
          <p>
            That needs administrator rights, so the reading is done by a separate small <strong>collector</strong>{" "}
            process that only reports data. It can't change or end other programs. This window stays at normal
            privilege and just displays what the collector sends. Starting the collector shows one Windows
            approval prompt.
          </p>
        </section>
      )}

      {/* Adapter counters need no administrator rights, so they are there even before the collector starts. */}
      {!connected && <NetworkView net={net} standalone />}

      {connected && (
        <>
          <div className="tabs" role="tablist" aria-label="Views">
            {TABS.map((t) => (
              <button
                key={t.id}
                role="tab"
                id={`tab-${t.id}`}
                aria-selected={tab === t.id}
                aria-controls="panel"
                className={tab === t.id ? "tab active" : "tab"}
                onClick={() => setTab(t.id)}
              >
                {t.label}
              </button>
            ))}
          </div>
          <div role="tabpanel" id="panel" aria-labelledby={`tab-${tab}`}>
            {tab === "memory" && (
              <>
                {memory ? <MemoryView memory={memory} processes={processes} /> : <p className="dim">Waiting for the first reading…</p>}
                {processes && <ProcessTable list={processes} />}
              </>
            )}
            {tab === "disk" && <DiskView win={win} profile={profile} />}
            {tab === "drivers" && <DriversView win={win} profile={profile} />}
            {tab === "network" && <NetworkView net={net} />}
            {tab === "pc" && (profile ? <ProfileCard profile={profile} /> : <p className="dim">Waiting for the first reading…</p>)}
          </div>
        </>
      )}

      <Footer />
    </main>
  );
}
