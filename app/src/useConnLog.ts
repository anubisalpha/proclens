import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ConnLogStatus } from "./api";

/** The connection log's state, kept in step with the app's Rust side (which does the writing). */
export function useConnLog() {
  const [status, setStatus] = useState<ConnLogStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    invoke<ConnLogStatus>("connlog_status")
      .then((s) => !cancelled && setStatus(s))
      .catch((e) => !cancelled && setError(String(e)));
    listen<ConnLogStatus>("connlog-status", (e) => setStatus(e.payload)).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const set = useCallback((enabled: boolean, intervalSeconds?: number) => {
    setError(null);
    invoke<ConnLogStatus>("connlog_set", { enabled, intervalS: intervalSeconds ?? null })
      .then(setStatus)
      .catch((e) => setError(String(e)));
  }, []);

  const openFolder = useCallback(() => {
    // The connection log lives in the same folder as the activity log.
    invoke("log_open_folder").catch((e) => setError(String(e)));
  }, []);

  return { status, error, set, openFolder };
}

export type ConnLogState = ReturnType<typeof useConnLog>;
