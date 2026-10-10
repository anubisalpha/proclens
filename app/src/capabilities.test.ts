import { describe, expect, it } from "vitest";
import type { Capability, SystemProfile } from "./api";
import { failedAmong, failedChecksMessage } from "./capabilities";

const cap = (id: string, status: Capability["status"]): Capability => ({ id, status, detail: "", verified_in_profile: true });
const profile = (caps: Capability[]): SystemProfile => ({
  os: { major: 10, minor: 0, build: 19045, ubr: 7725, arch: "x86_64", display_version: "22H2", edition: "Core" },
  verification: "exact",
  profile_title: "Windows 10 22H2 (build 19045)",
  capabilities: caps,
});

describe("failedAmong", () => {
  it("is empty before the profile arrives", () => {
    expect(failedAmong(null, ["etw_file_io"])).toEqual([]);
  });

  it("returns only failed checks, and only the ones asked about", () => {
    const p = profile([cap("etw_file_io", "failed"), cap("etw_dpc_isr", "failed"), cap("etw_disk_io", "ok")]);
    expect(failedAmong(p, ["etw_file_io", "etw_disk_io"]).map((c) => c.id)).toEqual(["etw_file_io"]);
  });

  it("does not treat a check that didn't run as a failure", () => {
    const p = profile([cap("etw_kernel_session", "not_run"), cap("memory_lists", "not_run")]);
    expect(failedAmong(p, ["etw_kernel_session", "memory_lists"])).toEqual([]);
  });
});

describe("failedChecksMessage", () => {
  it("says nothing when nothing failed", () => {
    expect(failedChecksMessage([])).toBeNull();
  });

  it("names the failed checks in plain language", () => {
    const msg = failedChecksMessage([cap("etw_file_io", "failed"), cap("etw_disk_io", "failed")]);
    expect(msg).toContain("File activity tracing, Disk activity tracing");
    expect(msg).toContain("This PC");
  });

  it("falls back to the raw id for an unknown capability", () => {
    expect(failedChecksMessage([cap("future_thing", "failed")])).toContain("future_thing");
  });
});
