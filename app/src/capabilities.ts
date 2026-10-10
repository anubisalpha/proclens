import type { Capability, SystemProfile } from "./api";

/** Plain-language names for the collector's capability ids. */
export const CAPABILITY_NAMES: Record<string, string> = {
  process_list: "Process list",
  memory_counters: "Memory totals",
  memory_lists: "Memory breakdown (in use / cache / free)",
  handle_table_names: "Names of files opened before tracing",
  etw_kernel_session: "Kernel event tracing",
  etw_disk_io: "Disk activity tracing",
  etw_file_io: "File activity tracing",
  etw_dpc_isr: "Driver CPU time tracing",
};

export function capabilityName(id: string): string {
  return CAPABILITY_NAMES[id] ?? id;
}

/**
 * The checks among `ids` that ran on this PC and failed. A check that wasn't run (for example
 * because the collector isn't elevated) is not a failure.
 */
export function failedAmong(profile: SystemProfile | null, ids: string[]): Capability[] {
  if (!profile) return [];
  return profile.capabilities.filter((c) => c.status === "failed" && ids.includes(c.id));
}

/** The sentence shown above a view whose self-test failed, or null if nothing failed. */
export function failedChecksMessage(failed: Capability[]): string | null {
  if (failed.length === 0) return null;
  const names = failed.map((c) => capabilityName(c.id)).join(", ");
  return `On this PC the built-in check for ${names} failed, so the numbers on this tab may be wrong. See the "This PC" tab for details.`;
}
