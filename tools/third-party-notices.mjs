// Writes a list of the third-party code that ships in the release binaries, with licence
// identifiers taken from the package metadata (not from reading licence files).
//   node tools/third-party-notices.mjs <output file>
//
// Rust: crates reachable from the two shipped binaries through normal (non-dev, non-build)
// dependencies, for the Windows target. JavaScript: the runtime dependencies bundled by Vite.
// This is a list, not the licence texts; each entry names where the full text lives.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const outFile = process.argv[2];
if (!outFile) {
  console.error("usage: node tools/third-party-notices.mjs <output file>");
  process.exit(2);
}

const meta = JSON.parse(
  execFileSync(
    "cargo",
    ["metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc", "--locked"],
    { cwd: root, maxBuffer: 1 << 29, encoding: "utf8" },
  ),
);

const byId = new Map(meta.packages.map((p) => [p.id, p]));
const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
const roots = meta.packages.filter((p) => p.name === "collector" || p.name === "proclens-app").map((p) => p.id);

// Walk normal dependencies only.
const seen = new Set();
const stack = [...roots];
while (stack.length) {
  const id = stack.pop();
  if (seen.has(id)) continue;
  seen.add(id);
  for (const dep of nodes.get(id)?.deps ?? []) {
    if (dep.dep_kinds.some((k) => k.kind === null)) stack.push(dep.pkg);
  }
}

const workspace = new Set(meta.workspace_members);
const rust = [...seen]
  .filter((id) => !workspace.has(id))
  .map((id) => byId.get(id))
  .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));

const missing = rust.filter((p) => !p.license && !p.license_file);

// JavaScript runtime dependencies (what Vite bundles into the UI), resolved from node_modules.
const appDir = path.join(root, "app");
const appPkg = JSON.parse(readFileSync(path.join(appDir, "package.json"), "utf8"));
const jsSeen = new Map();
function addJs(name) {
  if (jsSeen.has(name)) return;
  const file = path.join(appDir, "node_modules", ...name.split("/"), "package.json");
  if (!existsSync(file)) {
    jsSeen.set(name, { name, version: "?", license: "(not installed: run npm install)" });
    return;
  }
  const p = JSON.parse(readFileSync(file, "utf8"));
  jsSeen.set(name, { name, version: p.version, license: typeof p.license === "string" ? p.license : "(see package)", homepage: p.homepage });
  for (const dep of Object.keys(p.dependencies ?? {})) addJs(dep);
}
for (const dep of Object.keys(appPkg.dependencies ?? {})) addJs(dep);
const js = [...jsSeen.values()].sort((a, b) => a.name.localeCompare(b.name));

const lines = [];
lines.push("ProcLens third-party notices");
lines.push("============================");
lines.push("");
lines.push("ProcLens itself is MIT licensed (see LICENSE). The release binaries also contain the");
lines.push("third-party code listed below. Licence identifiers are SPDX expressions copied from each");
lines.push("package's metadata; the full licence text and copyright notice for each is in that");
lines.push("package's source repository or crate archive. This file is a list, not a substitute for");
lines.push("those texts.");
lines.push("");
lines.push("vendor/ferrisetw is a patched copy of ferrisetw (MIT OR Apache-2.0); see vendor/PATCHES.md.");
lines.push("");
lines.push(`Rust crates (${rust.length})`);
lines.push("-".repeat(40));
for (const p of rust) {
  lines.push(`${p.name} ${p.version} | ${p.license ?? "(licence file only: " + p.license_file + ")"} | ${p.repository ?? p.homepage ?? "no repository listed"}`);
}
lines.push("");
lines.push(`JavaScript packages bundled in the UI (${js.length})`);
lines.push("-".repeat(40));
for (const p of js) lines.push(`${p.name} ${p.version} | ${p.license} | ${p.homepage ?? "see package"}`);
lines.push("");
if (missing.length) {
  lines.push("Crates with no licence expression in their metadata (check by hand):");
  for (const p of missing) lines.push(`  ${p.name} ${p.version}`);
  lines.push("");
}
writeFileSync(outFile, lines.join("\n"), "utf8");
console.log(`wrote ${outFile}: ${rust.length} Rust crates, ${js.length} JS packages, ${missing.length} without a licence expression`);
