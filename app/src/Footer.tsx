import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type UpdateResult =
  | { state: "up_to_date"; current: string }
  | { state: "update_available"; current: string; latest: string; tag: string };

type Check =
  | { phase: "idle" }
  | { phase: "checking" }
  | { phase: "done"; result: UpdateResult }
  | { phase: "error"; message: string };

/** Bottom line of the window: who made it, a link to the project, and a manual update check.
 *  The check only runs when the button is clicked; nothing contacts the network before that. */
export function Footer() {
  const [check, setCheck] = useState<Check>({ phase: "idle" });

  const run = useCallback(() => {
    setCheck({ phase: "checking" });
    invoke<UpdateResult>("check_for_update")
      .then((result) => setCheck({ phase: "done", result }))
      .catch((e) => setCheck({ phase: "error", message: String(e) }));
  }, []);

  // Links go through Rust so the webview never navigates away from the app.
  const openProject = useCallback(() => void invoke("open_project_page").catch(() => {}), []);
  const openRelease = useCallback((tag: string) => void invoke("open_release_page", { tag }).catch(() => {}), []);

  return (
    <footer>
      <span>
        Anubis Productions ·{" "}
        <button className="linklike" onClick={openProject} title="https://github.com/anubisalpha/proclens">
          GitHub
        </button>
      </span>
      <span className="update" role="status" aria-live="polite">
        {check.phase === "checking" && <span className="dim">Checking…</span>}
        {check.phase === "done" && check.result.state === "up_to_date" && (
          <span className="dim">Up to date ({check.result.current})</span>
        )}
        {check.phase === "done" && check.result.state === "update_available" && (
          <>
            Update available: {check.result.latest}{" "}
            <button className="linklike" onClick={() => openRelease((check.result as { tag: string }).tag)}>
              View release
            </button>
          </>
        )}
        {check.phase === "error" && <span className="err">Couldn't check: {check.message}</span>}{" "}
        <button className="linklike" onClick={run} disabled={check.phase === "checking"}>
          {check.phase === "idle" ? "Check for updates" : "Check again"}
        </button>
      </span>
    </footer>
  );
}
