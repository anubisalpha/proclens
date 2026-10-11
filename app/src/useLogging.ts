import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { LogStatus } from "./api";

/** The activity log's state, kept in step with the app's Rust side (which does the writing). */
export function useLogging() {
  const [status, setStatus] = useState<LogStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    invoke<LogStatus>("log_status")
      .then((s) => !cancelled && setStatus(s))
      .catch((e) => !cancelled && setError(String(e)));
    listen<LogStatus>("log-status", (e) => setStatus(e.payload)).then((u) => {
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
    invoke<LogStatus>("log_set", { enabled, intervalS: intervalSeconds ?? null })
      .then(setStatus)
      .catch((e) => setError(String(e)));
  }, []);

  const openFolder = useCallback(() => {
    invoke("log_open_folder").catch((e) => setError(String(e)));
  }, []);

  return { status, error, set, openFolder };
}
