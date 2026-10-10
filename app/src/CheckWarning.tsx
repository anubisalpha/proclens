import type { SystemProfile } from "./api";
import { failedAmong, failedChecksMessage } from "./capabilities";

/** Shown above a view when a built-in check for what the view depends on failed on this PC. */
export function CheckWarning({ profile, ids }: { profile: SystemProfile | null; ids: string[] }) {
  const message = failedChecksMessage(failedAmong(profile, ids));
  return message ? (
    <p className="banner error" role="alert">
      {message}
    </p>
  ) : null;
}