import type { CapabilityStatus, SystemProfile, Verification } from "./api";

const VERIFICATION: Record<Verification, { label: string; detail: string; tone: string }> = {
  exact: {
    label: "Verified build",
    detail: "ProcLens was tested by hand on this exact Windows build and patch level.",
    tone: "ok",
  },
  same_build_other_patch: {
    label: "Same build, different patch level",
    detail:
      "ProcLens was tested on this Windows build at another patch level. It probably works; the live checks below decide.",
    tone: "wait",
  },
  untested: {
    label: "Untested Windows build",
    detail:
      "ProcLens has no profile for this build. Anything that fails its live check is switched off instead of guessing.",
    tone: "wait",
  },
};

const STATUS: Record<CapabilityStatus, string> = {
  ok: "Working",
  failed: "Switched off",
  not_run: "Not checked",
};

const NAMES: Record<string, string> = {
  process_list: "Process list",
  memory_counters: "Memory totals",
  memory_lists: "Memory breakdown (in use / cache / free)",
  handle_table_names: "Names of files opened before tracing",
  etw_kernel_session: "Kernel event tracing",
  etw_disk_io: "Disk activity tracing",
  etw_file_io: "File activity tracing",
  etw_dpc_isr: "Driver CPU time tracing",
};

export function ProfileCard({ profile }: { profile: SystemProfile }) {
  const { os } = profile;
  const v = VERIFICATION[profile.verification];
  return (
    <section aria-labelledby="pc-h" className="card">
      <h2 id="pc-h">This PC</h2>
      <p className="lede">
        Windows {os.major === 10 && os.build >= 22000 ? "11" : os.major} {os.display_version} · build {os.build}.{os.ubr}
        {os.edition ? ` · ${os.edition}` : ""} · {os.arch}
      </p>
      <p className="verify">
        <span className={`dot ${v.tone}`} aria-hidden="true" /> <strong>{v.label}.</strong> {v.detail}
      </p>
      <table className="plain caps">
        <tbody>
          {profile.capabilities.map((c) => (
            <tr key={c.id}>
              <th scope="row">{NAMES[c.id] ?? c.id}</th>
              <td className={`cap cap-${c.status}`}>{STATUS[c.status]}</td>
              <td className="dim hint">
                {c.detail}
                {c.verified_in_profile && c.status !== "ok" ? " (verified on this build earlier)" : ""}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
