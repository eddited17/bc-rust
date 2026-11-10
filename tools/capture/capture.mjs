// Drive the real Business Central web client in headless Chromium and record
// the browser <-> web server protocol (/csh WebSocket frames + HTTP calls).
//
// usage: node tools/capture/capture.mjs <baseUrl> <user> <password> <outDir> [scenario]
//   baseUrl e.g. http://127.0.0.1:8180/BC/
//   scenario: comma-separated steps, default "home,page:22,page:21,page:42"
//             (page:N is a full navigation to <baseUrl>?page=N; wait:S sleeps S seconds)
//   env CAPTURE_MAX_MS: per-step timeout (default 120000)
//
// Writes to outDir: frames.jsonl (one record per event: step markers, http requests
// and bodies, /csh WebSocket frames; the client opens /csh from a Web Worker, so
// worker targets are attached too), timings.json (per step: ms to visible content,
// ms to the last /csh frame, frame count and bytes) and a screenshot per step.
// Decode the gzip payloads in frames.jsonl with tools/capture/decode-frames.mjs.
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, appendFileSync, readdirSync, existsSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const [baseUrl, user, password, outDir, scenarioArg] = process.argv.slice(2);
if (!baseUrl || !user || !password || !outDir) {
  console.error("usage: capture.mjs <baseUrl> <user> <password> <outDir> [scenario]");
  process.exit(2);
}
const scenario = (scenarioArg || "home,page:22,page:21,page:42").split(",");
mkdirSync(outDir, { recursive: true });
const framesFile = join(outDir, "frames.jsonl");
writeFileSync(framesFile, "");

function findChrome() {
  const root = join(homedir(), ".cache/ms-playwright");
  for (const d of readdirSync(root)) {
    if (!d.startsWith("chromium_headless_shell")) continue;
    const p = join(root, d, "chrome-headless-shell-linux64", "chrome-headless-shell");
    if (existsSync(p)) return p;
  }
  throw new Error("chrome-headless-shell not found");
}

const port = 9300 + Math.floor(Math.random() * 600);
const chrome = spawn(findChrome(), [
  "--headless", "--no-sandbox", "--disable-gpu", `--remote-debugging-port=${port}`,
  `--user-data-dir=/tmp/bccap-${port}`, "--window-size=1440,900", "--lang=en-US", "about:blank",
], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 50; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      const page = list.find((t) => t.type === "page");
      if (page) return page.webSocketDebuggerUrl;
    } catch {}
    await sleep(200);
  }
  throw new Error("no page target");
}

const ws = new WebSocket(await target());
await new Promise((r) => (ws.onopen = r));
let nextId = 1;
const pending = new Map();
const t0 = Date.now();
const sockets = new Map(); // "<session>:<requestId>" -> WebSocket URL
const bodies = new Map(); // "<session>:<requestId>" -> response info awaiting its body
// The web client opens /csh from a Web Worker, which is its own CDP target: attach to
// child targets (workers, out-of-process frames) and enable Network in each session.
const sessions = new Map(); // CDP sessionId -> target type
// Time of the last WebSocket frame, used to detect when the client has settled.
let lastWsAt = 0;
let wsFrames = 0;
let wsBytes = 0;
const rec = (dir, payload, extra = {}) =>
  appendFileSync(framesFile, JSON.stringify({ t: Date.now() - t0, dir, ...extra, payload }) + "\n");
const send = (method, params = {}, sessionId) =>
  new Promise((res) => {
    const id = nextId++;
    pending.set(id, res);
    ws.send(JSON.stringify(sessionId ? { id, method, params, sessionId } : { id, method, params }));
  });
const autoAttach = (sessionId) =>
  send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, sessionId);
async function attached({ sessionId, targetInfo, waitingForDebugger }) {
  sessions.set(sessionId, targetInfo.type);
  rec("target", { type: targetInfo.type, url: targetInfo.url });
  await send("Network.enable", {}, sessionId);
  await autoAttach(sessionId);
  if (waitingForDebugger) await send("Runtime.runIfWaitingForDebugger", {}, sessionId);
}
const frame = (m, dir) => {
  const key = `${m.sessionId || ""}:${m.params.requestId}`;
  const r = m.params.response;
  lastWsAt = Date.now();
  wsFrames++;
  wsBytes += r.payloadData.length;
  rec(dir, r.payloadData, { url: sockets.get(key), op: r.opcode, via: sessions.get(m.sessionId) });
};
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
    return;
  }
  const key = `${m.sessionId || ""}:${m.params?.requestId}`;
  switch (m.method) {
    case "Target.attachedToTarget":
      attached(m.params);
      break;
    case "Target.detachedFromTarget":
      sessions.delete(m.params.sessionId);
      break;
    case "Network.webSocketCreated":
      sockets.set(key, m.params.url);
      lastWsAt = Date.now();
      rec("ws-open", m.params.url, { via: sessions.get(m.sessionId) });
      break;
    case "Network.webSocketClosed":
      rec("ws-close", sockets.get(key), { via: sessions.get(m.sessionId) });
      break;
    case "Network.webSocketFrameSent":
      frame(m, "send");
      break;
    case "Network.webSocketFrameReceived":
      frame(m, "recv");
      break;
    case "Network.responseReceived": {
      const r = m.params.response;
      const mt = (r.mimeType || "").toLowerCase();
      if (!/\.(js|css|png|svg|woff2?|ico|gif|jpg|mjs)(\?|$)/i.test(r.url) && (mt.includes("html") || mt.includes("json") || mt.includes("xml") || mt.includes("text/plain")))
        bodies.set(key, { url: r.url, status: r.status, mime: mt, headers: r.headers, sessionId: m.sessionId });
      break;
    }
    case "Network.loadingFinished": {
      const info = bodies.get(key);
      if (info) {
        bodies.delete(key);
        const { sessionId, ...meta } = info;
        send("Network.getResponseBody", { requestId: m.params.requestId }, sessionId).then((res) =>
          rec("http-body", { ...meta, body: res.result?.body?.slice(0, 200000), base64: res.result?.base64Encoded }, { via: sessions.get(sessionId) }));
      }
      break;
    }
    case "Network.requestWillBeSent": {
      const r = m.params.request;
      if (!/^(data|blob):/.test(r.url) && !/\.(js|css|png|svg|woff2?|ttf|ico|gif|jpg|json\.br|mjs)(\?|$)/i.test(r.url))
        rec("http", { method: r.method, url: r.url, body: r.postData?.slice(0, 4000) }, { via: sessions.get(m.sessionId) });
      break;
    }
  }
};
const evaluate = async (expr) => (await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true })).result?.result?.value;
const shot = async (name) => {
  const r = await send("Page.captureScreenshot", { format: "png" });
  if (r.result?.data) writeFileSync(join(outDir, `${name}.png`), Buffer.from(r.result.data, "base64"));
};
const waitFor = async (expr, ms = 60000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await evaluate(expr)) return true;
    await sleep(500);
  }
  return false;
};

await send("Network.enable", {});
await send("Page.enable", {});
await send("Runtime.enable", {});
await autoAttach();

// State of the web client, which renders inside the same-origin iframe .designer-client-frame:
// the top document title becomes the open form's caption once the form is shown.
const STATE = `(() => {
  const f = document.querySelector('iframe.designer-client-frame');
  const d = f && f.contentDocument;
  const vis = (el) => el.getClientRects().length > 0;
  if (!d) return { title: document.title, frame: false, password: !!document.querySelector('input[type=password]') };
  return {
    title: document.title, frame: true,
    headings: [...d.querySelectorAll('[role=heading], h1, h2')].filter(vis).length,
    cells: [...d.querySelectorAll('[role=gridcell], td')].filter((c) => vis(c) && c.textContent.trim()).length,
    dialog: [...d.querySelectorAll('[role=dialog], [role=alertdialog]')].filter(vis).map((x) => x.textContent.trim().slice(0, 160)).join(' | '),
  };
})()`;
const DEFAULT_TITLE = "Dynamics 365 Business Central";

// Wait until ready(state) holds and /csh has been quiet for quietMs. Returns ms from start
// to visible content and to the last /csh frame of the step.
async function settle(start, ready, { quietMs = 2000, maxMs = Number(process.env.CAPTURE_MAX_MS || 120000) } = {}) {
  let visibleAt = null;
  let state;
  while (Date.now() - start < maxMs) {
    state = await evaluate(STATE);
    if (state && ready(state)) {
      visibleAt ??= Date.now();
      if (Date.now() - Math.max(lastWsAt, visibleAt) >= quietMs) break;
    } else visibleAt = null;
    await sleep(100);
  }
  return {
    visibleMs: visibleAt ? visibleAt - start : null,
    lastFrameMs: lastWsAt > start ? lastWsAt - start : null,
    timedOut: Date.now() - start >= maxMs,
    title: state?.title,
    // a dialog without a form caption is an error; with one it is e.g. a teaching tip
    ...(state?.dialog && state.title === DEFAULT_TITLE ? { dialog: state.dialog } : {}),
  };
}

const timings = [];
const step = async (label, fn) => {
  const s = Date.now();
  const f0 = wsFrames, b0 = wsBytes;
  rec("step", { label, phase: "start" });
  const r = (await fn(s)) || {};
  const t = { label, ms: Date.now() - s, ...r, frames: wsFrames - f0, frameBytes: wsBytes - b0 };
  timings.push(t);
  rec("step", { ...t, phase: "end" });
  console.log(`${label}: visible ${r.visibleMs ?? "-"} ms, last /csh frame ${r.lastFrameMs ?? "-"} ms, ${t.frames} frames${r.timedOut ? " (TIMEOUT)" : ""}${r.title ? `  [${r.title}]` : ""}${r.dialog ? `  dialog: ${r.dialog}` : ""}`);
};

await step("login page", async (s) => {
  await send("Page.navigate", { url: baseUrl });
  await waitFor(`!!document.querySelector('input[type=password]')`, 90000);
  return { visibleMs: Date.now() - s };
});
await shot("00-login");
await step("sign in + role center", async (s) => {
  await evaluate(`(() => {
    const u = document.querySelector('input[name=UserName], #UserName, input[type=text]');
    const p = document.querySelector('input[type=password]');
    const set = (el, v) => { el.focus(); el.value = v; el.dispatchEvent(new Event('input', {bubbles:true})); el.dispatchEvent(new Event('change', {bubbles:true})); };
    set(u, ${JSON.stringify(user)}); set(p, ${JSON.stringify(password)});
    (document.querySelector('button[type=submit], input[type=submit], #submitButton') || p.form).click?.() ?? p.form.submit();
    return true; })()`);
  // The role center is up once its parts (Activities, ...) have rendered headings.
  return settle(s, (st) => st.frame && st.headings >= 3);
});
await shot("01-rolecenter");

let n = 2;
for (const st of scenario) {
  if (st === "home") continue;
  const [kind, arg] = st.split(":");
  if (kind === "page") {
    // A full navigation: reloads the client shell and opens a new /csh session.
    const url = `${baseUrl}${baseUrl.includes("?") ? "&" : "?"}page=${arg}`;
    await step(`open page ${arg}`, async (s) => {
      await send("Page.navigate", { url });
      await sleep(300); // let the old document go away before reading the title
      // Ready when the form caption is the title, or a dialog (e.g. an error) is shown.
      return settle(s, (x) => x.frame && ((x.title && x.title !== DEFAULT_TITLE) || !!x.dialog));
    });
    await shot(String(n++).padStart(2, "0") + `-page-${arg}`);
  } else if (kind === "wait") {
    await sleep(Number(arg) * 1000);
  }
}
writeFileSync(join(outDir, "timings.json"), JSON.stringify(timings, null, 1));
ws.close();
chrome.kill();
console.log(`frames: ${framesFile}`);
process.exit(0);
