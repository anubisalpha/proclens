// Minimal Chrome DevTools Protocol client for driving/inspecting the running app's WebView2
// (start the app with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222, or another
// port and set CDP_PORT to match: any other WebView2 app on the PC may already be using 9222, and
// this tool drives the first page it finds there).
//   node tools/cdp.mjs eval "<js expression>"
//   node tools/cdp.mjs shot <file.png>
import { writeFileSync } from "node:fs";

const [, , cmd, arg] = process.argv;
const port = process.env.CDP_PORT ?? "9222";
const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const page = targets.find((t) => t.type === "page");
if (!page) {
  console.error(`no page target; is the app running with --remote-debugging-port=${port}?`);
  process.exit(2);
}

const ws = new WebSocket(page.webSocketDebuggerUrl);
let nextId = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
  }
};
const send = (method, params = {}) =>
  new Promise((resolve) => {
    const id = ++nextId;
    pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
await new Promise((r) => (ws.onopen = r));

if (cmd === "eval") {
  const r = await send("Runtime.evaluate", { expression: arg, returnByValue: true, awaitPromise: true });
  const v = r.result?.result;
  console.log(typeof v?.value === "string" ? v.value : JSON.stringify(v?.value ?? r.result ?? r, null, 2));
} else if (cmd === "shot") {
  const r = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(arg, Buffer.from(r.result.data, "base64"));
  console.log("saved", arg);
} else {
  console.error("usage: eval <js> | shot <file>");
}
ws.close();
