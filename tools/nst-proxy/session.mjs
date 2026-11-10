// Drive one Business Central web client session in headless Chromium, for recording the
// web server <-> NST traffic with proxy.mjs. Based on tools/capture/capture.mjs (same CDP
// plumbing), plus in-page clicks and typing so a card can be edited.
//
// usage: PASSWORD_FILE=<file> node tools/nst-proxy/session.mjs <baseUrl> <user> <outDir> [steps]
//   steps (comma separated), default:
//     "signin,tip,page:22,tip,drill,tip,edit:Phone No.,edit:Country/Region Code=ZZ,close,page:42,tip"
//     signin       sign in and wait for the role center
//     page:N       full navigation to <baseUrl>?page=N
//     tip          close a teaching tip if one is open
//     drill        click the first row's link in the open list (opens its card)
//     edit:<field>[=<value>]  set <field> on the open card to <value> (default "+1 425 555 0199"),
//                  leave the field, wait, then set it back to the original value and leave it
//                  again. An error dialog after either change is noted and closed with OK.
//     close        close the open page (Back button, else Escape)
//     wait:S       sleep S seconds
//     dump[:name]  save the client document's HTML as dom-<name>.html (for finding selectors)
//   env MARK_URL  marker endpoint of proxy.mjs (default http://127.0.0.1:7286/mark; "off" to skip)
//       NST_LOG   the proxy's nst.jsonl, used to wait until NST traffic is quiet
//       STEP_MAX_MS per-step timeout (default 90000)
//       CHROME_TZ  TZ for the browser process (default: inherited). The web server detects the
//                  session time zone from the browser.
//
// Writes screenshots, driver.jsonl (steps, outcomes, browser console errors) and
// driver-results.json to outDir.
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, appendFileSync, readdirSync, existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const [baseUrl, user, outDir, stepsArg] = process.argv.slice(2);
if (!baseUrl || !user || !outDir || !process.env.PASSWORD_FILE) {
  console.error("usage: PASSWORD_FILE=<file> session.mjs <baseUrl> <user> <outDir> [steps]");
  process.exit(2);
}
const password = readFileSync(process.env.PASSWORD_FILE, "utf8").trim();
const steps = (stepsArg || "signin,tip,page:22,tip,drill,tip,edit:Phone No.,edit:Country/Region Code=ZZ,close,page:42,tip").split(",");
const MARK_URL = process.env.MARK_URL ?? "http://127.0.0.1:7286/mark";
const NST_LOG = process.env.NST_LOG;
const STEP_MAX_MS = Number(process.env.STEP_MAX_MS || 90000);
mkdirSync(outDir, { recursive: true });
const driverLog = join(outDir, "driver.jsonl");
const t0 = Date.now();
const note = (rec) => appendFileSync(driverLog, JSON.stringify({ t: Date.now() - t0, ...rec }) + "\n");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function mark(label, extra = {}) {
  note({ mark: label, ...extra });
  if (MARK_URL === "off") return;
  try {
    await fetch(MARK_URL, { method: "POST", body: JSON.stringify({ label, ...extra }) });
  } catch {}
}

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
  `--user-data-dir=/tmp/bcnst-${port}`, "--window-size=1440,900", "--lang=en-US", "about:blank",
], { stdio: "ignore", env: process.env.CHROME_TZ ? { ...process.env, TZ: process.env.CHROME_TZ } : process.env });

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
const send = (method, params = {}, sessionId) =>
  new Promise((res) => {
    const id = nextId++;
    pending.set(id, res);
    ws.send(JSON.stringify(sessionId ? { id, method, params, sessionId } : { id, method, params }));
  });
let lastCsh = 0; // last /csh frame seen in any target (the client socket lives in a worker)
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
    return;
  }
  switch (m.method) {
    case "Target.attachedToTarget": {
      const { sessionId, waitingForDebugger } = m.params;
      send("Network.enable", {}, sessionId)
        .then(() => send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }, sessionId))
        .then(() => waitingForDebugger && send("Runtime.runIfWaitingForDebugger", {}, sessionId));
      break;
    }
    case "Network.webSocketFrameSent":
    case "Network.webSocketFrameReceived":
      lastCsh = Date.now();
      break;
    case "Runtime.exceptionThrown":
      note({ console: "exception", text: m.params.exceptionDetails?.exception?.description?.slice(0, 500) || m.params.exceptionDetails?.text });
      break;
    case "Runtime.consoleAPICalled":
      if (m.params.type === "error") note({ console: "error", text: m.params.args.map((a) => a.value ?? a.description).join(" ").slice(0, 500) });
      break;
  }
};
await send("Network.enable");
await send("Page.enable");
await send("Runtime.enable");
await send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });

const evaluate = async (expr) => {
  const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) note({ evalError: r.result.exceptionDetails.exception?.description?.slice(0, 300) });
  return r.result?.result?.value;
};
let shotN = 0;
const shot = async (name) => {
  const r = await send("Page.captureScreenshot", { format: "png" });
  if (r.result?.data) writeFileSync(join(outDir, `${String(shotN++).padStart(2, "0")}-${name}.png`), Buffer.from(r.result.data, "base64"));
};

// The client renders in the same-origin iframe .designer-client-frame.
const D = `(document.querySelector('iframe.designer-client-frame') || {}).contentDocument`;
const STATE = `(() => {
  const d = ${D};
  const vis = (el) => el.getClientRects().length > 0;
  if (!d) return { title: document.title, frame: false, password: !!document.querySelector('input[type=password]'), text: (document.body && document.body.innerText || '').slice(0, 300) };
  return {
    title: document.title, frame: true,
    headings: [...d.querySelectorAll('[role=heading], h1, h2')].filter(vis).length,
    cells: [...d.querySelectorAll('[role=gridcell], td')].filter((c) => vis(c) && c.textContent.trim()).length,
    dialog: [...d.querySelectorAll('[role=dialog], [role=alertdialog]')].filter(vis).map((x) => x.textContent.trim().slice(0, 300)).join(' | '),
  };
})()`;
const DEFAULT_TITLE = "Dynamics 365 Business Central";

function nstLogSize() {
  try { return statSync(NST_LOG).size; } catch { return 0; }
}
// Wait until ready(state) holds and both the browser socket and the NST log have been quiet.
async function settle(start, ready, quietMs = 2500) {
  let visibleAt = null, state, size = nstLogSize(), sizeAt = Date.now();
  while (Date.now() - start < STEP_MAX_MS) {
    state = await evaluate(STATE);
    const s = nstLogSize();
    if (s !== size) { size = s; sizeAt = Date.now(); }
    if (state && ready(state)) {
      visibleAt ??= Date.now();
      if (Date.now() - Math.max(lastCsh, sizeAt, visibleAt) >= quietMs) break;
    } else visibleAt = null;
    await sleep(150);
  }
  return { visibleMs: visibleAt ? visibleAt - start : null, timedOut: Date.now() - start >= STEP_MAX_MS, state };
}

// Center of an element in the iframe, in top-level page coordinates (after scrolling it into view).
// `find` is JS evaluated with `d` bound to the client document; it must return an element or null.
async function locate(find) {
  return evaluate(`(() => {
    const d = ${D}; if (!d) return null;
    const el = (() => { ${find} })();
    if (!el) return null;
    el.scrollIntoView({ block: 'center' });
    const r = el.getBoundingClientRect(), f = document.querySelector('iframe.designer-client-frame').getBoundingClientRect();
    return { x: f.left + r.left + r.width / 2, y: f.top + r.top + r.height / 2, tag: el.tagName, text: (el.value ?? el.textContent ?? '').trim().slice(0, 80) };
  })()`);
}
async function click(p) {
  await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: p.x, y: p.y });
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x: p.x, y: p.y, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: p.x, y: p.y, button: "left", clickCount: 1 });
}
const KEYS = { Tab: 9, Enter: 13, Escape: 27, Backspace: 8, Delete: 46, End: 35, Home: 36 };
async function key(k, modifiers = 0) {
  const vk = KEYS[k] ?? k.toUpperCase().charCodeAt(0);
  const code = KEYS[k] ? k : `Key${k.toUpperCase()}`;
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", key: k, code, windowsVirtualKeyCode: vk, modifiers });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: k, code, windowsVirtualKeyCode: vk, modifiers });
}
const ctrlA = () => key("a", 2);

const results = [];
async function step(label, fn) {
  await mark(`step start: ${label}`);
  const s = Date.now();
  let r;
  try { r = (await fn(s)) || {}; } catch (e) { r = { error: String(e?.stack || e).slice(0, 500) }; }
  const out = { label, ms: Date.now() - s, ...r };
  if (out.state) { out.title = out.state.title; if (out.state.dialog) out.dialog = out.state.dialog; delete out.state; }
  results.push(out);
  await mark(`step end: ${label}`, { result: out });
  console.log(`${label}: ${out.ms} ms${out.timedOut ? " TIMEOUT" : ""}${out.title ? ` [${out.title}]` : ""}${out.dialog ? ` dialog: ${out.dialog}` : ""}${out.error ? ` ERROR ${out.error}` : ""}${out.info ? ` ${out.info}` : ""}`);
  await shot(label.replace(/[^\w.-]+/g, "_"));
  return out;
}
// A page is up when its caption is the document title (or an error dialog shows).
const pageReady = (x) => x.frame && ((x.title && x.title !== DEFAULT_TITLE) || !!x.dialog);

for (const st of steps) {
  const [kind, ...rest] = st.split(":");
  const arg = rest.join(":");
  if (kind === "signin") {
    await step("login page", async (s) => {
      await send("Page.navigate", { url: baseUrl });
      for (let i = 0; i < 200 && !(await evaluate(`!!document.querySelector('input[type=password]')`)); i++) await sleep(300);
      return { visibleMs: Date.now() - s };
    });
    await step("sign in + role center", async (s) => {
      await evaluate(`(() => {
        const u = document.querySelector('input[name=UserName], #UserName, input[type=text]');
        const p = document.querySelector('input[type=password]');
        const set = (el, v) => { el.focus(); el.value = v; el.dispatchEvent(new Event('input', {bubbles:true})); el.dispatchEvent(new Event('change', {bubbles:true})); };
        set(u, ${JSON.stringify(user)}); set(p, ${JSON.stringify(password)});
        (document.querySelector('button[type=submit], input[type=submit], #submitButton') || p.form).click?.() ?? p.form.submit();
        return true; })()`);
      // the role center is up once its parts have rendered headings
      return settle(s, (x) => (x.frame && x.headings >= 3) || !!x.dialog);
    });
  } else if (kind === "page") {
    await step(`open page ${arg}`, async (s) => {
      await send("Page.navigate", { url: `${baseUrl}${baseUrl.includes("?") ? "&" : "?"}page=${arg}` });
      await sleep(300);
      return settle(s, pageReady);
    });
  } else if (kind === "drill") {
    await step("drill into first row", async (s) => {
      const before = await evaluate(`document.title`);
      // The first visible link or button with text inside a data row of the list: the No. link
      // in the table layout (<tr role=row><td role=gridcell><a>), the tile's name in the tile
      // layout (<ul role=row><li role=gridcell>... <div role=button>).
      const p = await locate(`
        const vis = (e) => e.getClientRects().length > 0;
        const rows = [...d.querySelectorAll('[role=grid] [role=row]')].filter((r) => vis(r) && r.querySelector('[role=gridcell]') && !r.closest('[role=dialog]'));
        for (const r of rows) {
          const a = [...r.querySelectorAll('a, [role=button]')].find((a) => vis(a) && a.textContent.trim() && !/Show more|Select row/i.test(a.title || ''));
          if (a) return a;
        }
        return null;`);
      if (!p) return { error: "no row link found" };
      await click(p);
      const r = await settle(s, (x) => (x.frame && x.title !== before && x.title !== DEFAULT_TITLE) || !!x.dialog);
      return { ...r, info: `clicked ${p.tag} "${p.text}"` };
    });
  } else if (kind === "edit") {
    // edit:<field> or edit:<field>=<test value>
    const eq = arg.indexOf("=");
    const field = (eq < 0 ? arg : arg.slice(0, eq)) || "Phone No.";
    const testValue = eq < 0 ? "+1 425 555 0199" : arg.slice(eq + 1);
    // The input for a caption: the BC client's <div controlname="Phone No."><a>caption</a><div><input
    // aria-labelledby=...>, else a caption element referenced by aria-labelledby, <label for=id>, aria-label.
    const FIND = `
      const want = ${JSON.stringify(field)};
      const vis = (el) => el && el.getClientRects().length;
      const byName = [...d.querySelectorAll('[controlname]')].filter((c) => c.getAttribute('controlname') === want)
        .map((c) => c.querySelector('input, textarea')).find(vis);
      if (byName) return byName;
      for (const i of d.querySelectorAll('input, textarea')) {
        const ids = (i.getAttribute('aria-labelledby') || '').split(/\\s+/).filter(Boolean);
        if (vis(i) && ids.some((id) => (d.getElementById(id)?.textContent || '').trim() === want)) return i;
      }
      for (const l of d.querySelectorAll('label')) {
        if (l.textContent.trim() === want) {
          const el = l.htmlFor && d.getElementById(l.htmlFor);
          if (vis(el)) return el;
        }
      }
      return [...d.querySelectorAll('input, textarea')].find((i) => vis(i) && (i.getAttribute('aria-label') || '').trim() === want) || null;`;
    let original = null;
    const setValue = async (label, value) =>
      step(label, async (s) => {
        let p = await locate(FIND);
        if (!p) return { error: `field "${field}" not found` };
        const ro = await evaluate(`(() => { const d = ${D}; const el = (() => { ${FIND} })(); return el.readOnly || el.disabled; })()`);
        if (ro) {
          // card opened in view mode: switch to edit mode with the Edit action (pencil)
          const e = await locate(`return [...d.querySelectorAll('button, [role=button], [role=menuitem]')].find((b) => b.getClientRects().length && /^(Edit|Bearbeiten)$/.test((b.getAttribute('aria-label') || b.title || b.textContent).trim())) || null;`);
          if (!e) return { error: "field is read-only and no Edit action found" };
          await click(e);
          await settle(Date.now(), () => true, 1500);
          p = await locate(FIND);
        }
        original ??= p.text;
        await click(p);
        await sleep(200);
        await ctrlA();
        if (value === "") await key("Delete");
        else await send("Input.insertText", { text: value });
        await sleep(200);
        await key("Tab");
        const r = await settle(s, (x) => x.frame, 3000);
        // an error dialog (e.g. a table relation error): note its text, then press its OK button
        let error = null;
        if (r.state?.dialog) {
          error = r.state.dialog;
          await shot(`${label.replace(/[^\w.-]+/g, "_")}-dialog`);
          const ok = await locate(`
            const vis = (e) => e.getClientRects().length > 0;
            for (const dlg of [...d.querySelectorAll('[role=dialog], [role=alertdialog]')].filter(vis)) {
              const b = [...dlg.querySelectorAll('button, [role=button]')].find((b) => vis(b) && /^(OK|Close|Dismiss)$/i.test((b.textContent || b.getAttribute('aria-label') || b.title).trim()));
              if (b) return b;
            }
            return null;`);
          if (ok) await click(ok);
          else await key("Escape");
          Object.assign(r, await settle(Date.now(), (x) => x.frame && !x.dialog, 2000));
        }
        const now = await locate(FIND);
        return { ...r, info: `"${p.text}" -> "${now?.text}"${error ? `; error dialog: ${error.slice(0, 300)}` : ""}` };
      });
    await setValue(`edit ${field}`, testValue);
    await setValue(`revert ${field}`, original ?? "");
  } else if (kind === "close") {
    await step("close page", async (s) => {
      const before = await evaluate(`document.title`);
      const b = await locate(`return [...d.querySelectorAll('button, [role=button]')].find((b) => b.getClientRects().length && /^(Back|Close|Zurück|Schließen)$/i.test((b.getAttribute('aria-label') || b.title || '').trim())) || null;`);
      if (b) await click(b);
      else await key("Escape");
      const r = await settle(s, (x) => (x.frame && x.title !== before) || !!x.dialog);
      return { ...r, info: b ? `clicked ${b.tag}` : "Escape" };
    });
  } else if (kind === "wait") {
    await mark(`wait ${arg}s`);
    await sleep(Number(arg) * 1000);
  } else if (kind === "tip") {
    // close a teaching tip ("About customers ... Take a tour") if one is open
    await step("close teaching tip", async (s) => {
      const b = await locate(`
        const vis = (e) => e.getClientRects().length > 0;
        for (const dlg of [...d.querySelectorAll('[role=dialog], [role=alertdialog]')].filter(vis)) {
          const c = [...dlg.querySelectorAll('button, [role=button]')].find((b) => vis(b) && /^(Close|Schließen|Got it|Dismiss)$/i.test((b.getAttribute('aria-label') || b.title || b.textContent).trim()));
          if (c) return c;
        }
        return null;`);
      if (!b) return { info: "no teaching tip" };
      await click(b);
      const r = await settle(s, (x) => x.frame && !x.dialog, 2000);
      return { ...r, info: `clicked ${b.tag} "${b.text}"` };
    });
  } else if (kind === "dump") {
    // the client document's HTML, for finding selectors
    const html = await evaluate(`(${D} || document).documentElement.outerHTML`);
    writeFileSync(join(outDir, `dom-${arg || shotN}.html`), html || "");
  }
}
writeFileSync(join(outDir, "driver-results.json"), JSON.stringify(results, null, 1));
await mark("driver done");
ws.close();
chrome.kill();
process.exit(0);
