// Writes the third-party notices for the release binaries: a list of the code that ships (licence
// identifiers from the package metadata) followed by the full licence texts, read from each
// package's own licence/copying/notice files and grouped so identical texts appear once.
//   node tools/third-party-notices.mjs <output file>
//
// Rust: crates reachable from the two shipped binaries through normal (non-dev, non-build)
// dependencies, for the Windows target. JavaScript: the runtime dependencies bundled by Vite.
// A crate whose archive has no licence file falls back to docs/licenses (see its README). If any
// package still has no licence text the script exits 1, so a release can't ship with a gap.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
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

// Licence texts. Files named like licence/copying/notice in the package directory; for crates
// that ship none, the pinned fallback copies in docs/licenses.
const LICENCE_NAME = /^(licen[sc]e|copying|unlicense|notice|copyright)/i;
const FALLBACKS = {
  "webview2-com": "webview2-rs",
  "webview2-com-sys": "webview2-rs",
  "webview2-com-macros": "webview2-rs",
  "defmt-parser": "defmt",
  "alloc-stdlib": "rust-alloc-no-stdlib",
  selectors: "stylo-selectors",
};
function readLicenceFiles(dir) {
  return readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isFile() && LICENCE_NAME.test(e.name))
    .map((e) => ({ name: e.name, text: readFileSync(path.join(dir, e.name), "utf8") }));
}
const texts = new Map(); // normalised text -> { text, users: [] }
const noText = [];
function addTexts(label, files) {
  if (!files.length) noText.push(label);
  for (const f of files) {
    const text = f.text.replace(/\r\n/g, "\n").replace(/^﻿/, "").trim();
    if (!text) continue;
    const key = createHash("sha256").update(text).digest("hex");
    if (!texts.has(key)) texts.set(key, { text, users: [] });
    texts.get(key).users.push(`${label} (${f.name})`);
  }
}
for (const p of rust) {
  let files = readLicenceFiles(path.dirname(p.manifest_path));
  if (!files.length && FALLBACKS[p.name]) files = readLicenceFiles(path.join(root, "docs", "licenses", FALLBACKS[p.name]));
  addTexts(`${p.name} ${p.version}`, files);
}

// JavaScript runtime dependencies (what Vite bundles into the UI), resolved from node_modules.
const appDir = path.join(root, "app");
const appPkg = JSON.parse(readFileSync(path.join(appDir, "package.json"), "utf8"));
const jsSeen = new Map();
const jsDirs = new Map();
function addJs(name) {
  if (jsSeen.has(name)) return;
  const file = path.join(appDir, "node_modules", ...name.split("/"), "package.json");
  if (!existsSync(file)) {
    jsSeen.set(name, { name, version: "?", license: "(not installed: run npm install)" });
    return;
  }
  const p = JSON.parse(readFileSync(file, "utf8"));
  jsDirs.set(name, path.dirname(file));
  jsSeen.set(name, { name, version: p.version, license: typeof p.license === "string" ? p.license : "(see package)", homepage: p.homepage });
  for (const dep of Object.keys(p.dependencies ?? {})) addJs(dep);
}
for (const dep of Object.keys(appPkg.dependencies ?? {})) addJs(dep);
const js = [...jsSeen.values()].sort((a, b) => a.name.localeCompare(b.name));
for (const p of js) {
  if (jsDirs.has(p.name)) addTexts(`${p.name} ${p.version}`, readLicenceFiles(jsDirs.get(p.name)));
  else noText.push(`${p.name} (not installed)`);
}

const lines = [];
lines.push("ProcLens third-party notices");
lines.push("============================");
lines.push("");
lines.push("ProcLens itself is MIT licensed (see LICENSE). The release binaries also contain the");
lines.push("third-party code listed below. Licence identifiers are SPDX expressions copied from each");
lines.push("package's metadata. The full licence texts follow the lists, each with the packages it");
lines.push("applies to; packages that ship identical text are grouped under one copy.");
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
lines.push(`Licence texts (${texts.size} distinct)`);
lines.push("=".repeat(40));
lines.push("");
const groups = [...texts.values()].sort((a, b) => a.users[0].localeCompare(b.users[0]));
for (const g of groups) {
  lines.push("-".repeat(70));
  lines.push("Applies to:");
  for (const u of g.users.sort()) lines.push("  " + u);
  lines.push("-".repeat(70));
  lines.push(g.text);
  lines.push("");
}
if (missing.length) {
  lines.push("Crates with no licence expression in their metadata (check by hand):");
  for (const p of missing) lines.push(`  ${p.name} ${p.version}`);
  lines.push("");
}
writeFileSync(outFile, lines.join("\n"), "utf8");
console.log(`wrote ${outFile}: ${rust.length} Rust crates, ${js.length} JS packages, ${texts.size} distinct licence texts, ${missing.length} without a licence expression`);
if (noText.length) {
  console.error(`ERROR: no licence text found for ${noText.length} package(s):\n  ${noText.join("\n  ")}`);
  process.exit(1);
}
