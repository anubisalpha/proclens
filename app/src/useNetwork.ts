import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { NetworkSelfTest, NetworkSnapshot } from "./api";
import { RATE_WINDOW } from "./network";

const POLL_MS = 1000;

/**
 * Adapter counters, read by the app itself (no collector needed). Polls once a second while
 * `active`, keeps the last few readings so the view can average a rate, and forgets them when
 * inactive: a rate across a gap of minutes would be meaningless.
 */
export function useNetwork(active: boolean) {
  const [history, setHistory] = useState<NetworkSnapshot[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [check, setCheck] = useState<NetworkSelfTest | null>(null);

  useEffect(() => {
    if (!active) return;
    let stopped = false;

    invoke<NetworkSelfTest>("network_selftest")
      .then((t) => !stopped && setCheck(t))
      .catch((e) => !stopped && setCheck({ ok: false, detail: String(e) }));

    const tick = async () => {
      try {
        const s = await invoke<NetworkSnapshot>("network_snapshot");
        if (stopped) return;
        setHistory((h) => [...h.slice(-(RATE_WINDOW - 1)), s]);
        setError(null);
      } catch (e) {
        if (!stopped) setError(String(e));
      }
    };
    tick();
    const id = setInterval(tick, POLL_MS);
    return () => {
      stopped = true;
      clearInterval(id);
      setHistory([]);
    };
  }, [active]);

  return { history, error, check };
}

export type NetworkState = ReturnType<typeof useNetwork>;
