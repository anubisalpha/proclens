import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ExcludedRanges, PortsSnapshot } from "./api";

const POLL_MS = 3000;
const EXCLUDED_MS = 30000;

/**
 * Ports in use, read by the app itself (no collector needed). Polls only while `active`: the
 * reserved ranges come from running netsh, so they are only read (when the view opens, then every
 * half minute) while `withExcluded`, which only the Ports view needs.
 */
export function usePorts(active: boolean, withExcluded: boolean) {
  const [snapshot, setSnapshot] = useState<PortsSnapshot | null>(null);
  const [excluded, setExcluded] = useState<ExcludedRanges | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    if (!active) return;
    let stopped = false;

    const readPorts = async () => {
      try {
        const s = await invoke<PortsSnapshot>("ports_snapshot");
        if (stopped) return;
        setSnapshot(s);
        setError(null);
      } catch (e) {
        if (!stopped) setError(String(e));
      }
    };
    const readExcluded = async () => {
      try {
        const x = await invoke<ExcludedRanges>("ports_excluded");
        if (!stopped) setExcluded(x);
      } catch (e) {
        if (!stopped) setExcluded({ ranges: [], error: String(e) });
      }
    };

    readPorts();
    const a = setInterval(readPorts, POLL_MS);
    let b: ReturnType<typeof setInterval> | undefined;
    if (withExcluded) {
      readExcluded();
      b = setInterval(readExcluded, EXCLUDED_MS);
    }
    return () => {
      stopped = true;
      clearInterval(a);
      if (b !== undefined) clearInterval(b);
    };
    // `tick` re-runs the effect, which is how "Refresh" forces an immediate reading.
  }, [active, withExcluded, tick]);

  const refresh = useCallback(() => setTick((t) => t + 1), []);
  return { snapshot, excluded, error, refresh };
}

export type PortsState = ReturnType<typeof usePorts>;
