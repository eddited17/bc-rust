#!/usr/bin/env node
// webcheck: click through Microsoft's Business Central web client (served against bc-nst) like a user would,
// and fail on anything that breaks a session. See README.md in this folder.
//
//   WEBCHECK_URL=http://127.0.0.1:8191/BC/ WEBCHECK_USER=admin WEBCHECK_PASSWORD=... \
//   WEBCHECK_WEBLOG=file:/path/to/web.log node tools/webcheck/webcheck.mjs
//
// A step fails on: a "fail:" line in the web server log during the step, a client error dialog, the
// "Something went wrong" page, a session drop or a credential re-prompt, or the step's own check.
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';

const require = createRequire(import.meta.url);
const env = process.env;
const BASE = (env.WEBCHECK_URL || 'http://127.0.0.1:8191/BC/').replace(/\/?$/, '/');
const USER = env.WEBCHECK_USER || 'admin';
const PASSWORD = env.WEBCHECK_PASSWORD;
const OUT = env.WEBCHECK_OUT || path.join('/tmp', 'webcheck-' + new Date().toISOString().replace(/[:.]/g, '-'));
const ONLY = env.WEBCHECK_ONLY ? new RegExp(env.WEBCHECK_ONLY, 'i') : null;
const SKIP = env.WEBCHECK_SKIP ? new RegExp(env.WEBCHECK_SKIP, 'i') : null;
// failures with a known cause someone else owns: `regex=note;...`, the regex tested against the step name
// and the problem lines (a FAIL whose every problem matches becomes KNOWN)
const KNOWN = (env.WEBCHECK_KNOWN || '').split(';').filter(Boolean).map((k) => {
  const i = k.indexOf('=');
  return { re: new RegExp(i < 0 ? k : k.slice(0, i), 'i'), note: i < 0 ? 'known issue' : k.slice(i + 1) };
});
if (!PASSWORD) {
  console.error('webcheck: set WEBCHECK_PASSWORD (and WEBCHECK_USER, default admin)');
  process.exit(2);
}
fs.mkdirSync(OUT, { recursive: true });

function loadPlaywright() {
  const tries = [env.PLAYWRIGHT_CORE, 'playwright-core', 'playwright'].filter(Boolean);
  for (const t of tries) {
    try {
      return require(t);
    } catch {}
  }
  console.error('webcheck: playwright-core not found; npm install playwright-core, or set PLAYWRIGHT_CORE=/path/to/node_modules/playwright-core');
  process.exit(2);
}
const { chromium } = loadPlaywright();

// ------------------------------------------------------------------ logs

/// A log source: `file:/path` (read appended bytes) or `journal:<user unit>` (journalctl --user after a cursor).
function logSource(spec) {
  if (!spec) return null;
  if (spec.startsWith('file:')) {
    const p = spec.slice(5);
    let off = 0;
    const size = () => {
      try {
        return fs.statSync(p).size;
      } catch {
        return 0;
      }
    };
    return {
      spec,
      mark() {
        off = size();
      },
      read() {
        const end = size();
        if (end < off) off = 0; // truncated (restart)
        if (end === off) return [];
        const fd = fs.openSync(p, 'r');
        const buf = Buffer.alloc(end - off);
        fs.readSync(fd, buf, 0, buf.length, off);
        fs.closeSync(fd);
        off = end;
        return buf.toString('utf8').split('\n').filter(Boolean);
      },
    };
  }
  if (spec.startsWith('journal:')) {
    const unit = spec.slice(8);
    let cursor = null;
    const jc = (args) => execFileSync('journalctl', ['--user', '-u', unit, '--no-pager', ...args], { encoding: 'utf8', maxBuffer: 64 << 20 });
    const lastCursor = () => {
      const m = jc(['-n', '1', '-o', 'cat', '--show-cursor']).match(/-- cursor: (\S+)\s*$/);
      return m ? m[1] : null;
    };
    return {
      spec,
      mark() {
        cursor = lastCursor();
      },
      read() {
        const out = jc(cursor ? ['--after-cursor', cursor, '-o', 'cat', '--show-cursor'] : ['-n', '200', '-o', 'cat', '--show-cursor']);
        const lines = out.split('\n');
        const m = out.match(/-- cursor: (\S+)\s*$/);
        if (m) cursor = m[1];
        return lines.filter((l) => l && !l.startsWith('-- cursor:') && !l.startsWith('-- No entries --'));
      },
    };
  }
  throw new Error('WEBCHECK_WEBLOG / WEBCHECK_NSTLOG: use file:/path or journal:<unit>');
}
const webLog = logSource(env.WEBCHECK_WEBLOG);
const nstLog = logSource(env.WEBCHECK_NSTLOG);
if (!webLog) console.warn('webcheck: WEBCHECK_WEBLOG not set, web server "fail:" lines are not checked');
const FAIL_LINE = /\bfail: /;
// bc-nst lines worth listing (not failures by themselves)
const NST_NOTE = /fault|failed:|error:|panic|does not exist|not implemented|not supported|unsupported/i;
// an exception bc-nst sent to the client ("InvokeApplicationMethod fault (0.2 ms): ..."): the user
// gets an error for it even when no dialog element shows up in time for the step to see it
const NST_FAULT = /^bc-nst\[\d+\]: [A-Za-z]+ fault\b/;

// ------------------------------------------------------------------ browser helpers

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const t0 = Date.now();
const log = (...a) => console.log(((Date.now() - t0) / 1000).toFixed(1).padStart(7), ...a);
let browser, ctx, page;
const consoleErrors = [];

/// A new browser context (no cookies): after a session drop the web server has thrown away the stored credentials.
async function newContext() {
  if (ctx) await ctx.close().catch(() => {});
  ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'en-US', timezoneId: 'UTC' });
  page = await ctx.newPage();
  page.on('console', (m) => {
    if (m.type() === 'error' && !/fabric-icons|Failed to load resource|public path/.test(m.text())) consoleErrors.push(m.text().slice(0, 300));
  });
  page.on('pageerror', (e) => consoleErrors.push('pageerror ' + String(e).slice(0, 300)));
}

function cf() {
  return page.frames().find((f) => f.url().includes('runinframe=1')) || page.mainFrame();
}
async function inFrame(fn, arg) {
  try {
    return await cf().evaluate(fn, arg);
  } catch (e) {
    return { __error: e.message.split('\n')[0] };
  }
}

/// Sign-in form shown again, or the server error page: the session is gone.
async function sessionState() {
  try {
    if (await page.locator('#UserName').count()) return 'credential prompt (sign-in page shown again)';
  } catch {}
  const t = await inFrame(() => (document.body ? document.body.innerText : ''));
  if (typeof t === 'string') {
    if (/Something went wrong/.test(t)) return 'server error page: ' + t.replace(/\s+/g, ' ').match(/Something went wrong.{0,200}/)[0];
    if (/(lost the connection|session (has )?(expired|ended)|connection to the server was lost|try to reconnect)/i.test(t)) return 'session lost: ' + t.replace(/\s+/g, ' ').slice(0, 200);
  }
  return null;
}

const TIP = /Take a tour|Got it|teaching|Skip tour|Next tip/i;

/// Visible dialogs: { text, buttons, tip }
async function dialogs() {
  const r = await inFrame(() => {
    const vis = (e) => e.getClientRects().length > 0;
    return [...document.querySelectorAll('[role=dialog], [role=alertdialog]')]
      .filter(vis)
      .map((d) => ({
        text: d.innerText.replace(/\s+/g, ' ').trim().slice(0, 500),
        cls: String(d.className).slice(0, 120),
        buttons: [...d.querySelectorAll('button, [role=button]')].filter(vis).map((b) => (b.getAttribute('aria-label') || b.title || b.textContent || '').trim()).filter(Boolean).slice(0, 12),
      }));
  });
  if (!Array.isArray(r)) return [];
  return r.map((d) => ({ ...d, tip: TIP.test(d.text) || /teach|callout|coachmark/i.test(d.cls) }));
}

/// Click a button of the topmost visible dialog whose label/title/text equals one of `labels`.
async function dialogButton(labels) {
  const ok = await inFrame((want) => {
    const vis = (e) => e.getClientRects().length > 0;
    document.querySelectorAll('[data-wc]').forEach((e) => e.removeAttribute('data-wc'));
    const dls = [...document.querySelectorAll('[role=dialog], [role=alertdialog]')].filter(vis).reverse();
    for (const dl of dls) {
      const cands = [...dl.querySelectorAll('button, [role=button], [role=radio], [role=option], label, input[type=radio]')].filter(vis);
      for (const w of want) {
        const b = cands.find((b) => [b.getAttribute('aria-label'), b.title, b.textContent, b.value].some((t) => (t || '').trim() === w));
        if (b) {
          b.setAttribute('data-wc', '1');
          return w;
        }
      }
    }
    return null;
  }, labels);
  if (typeof ok !== 'string') return null;
  await cf().locator('[data-wc="1"]').first().click({ force: true, timeout: 15000 });
  return ok;
}

async function closeTips() {
  for (let i = 0; i < 4; i++) {
    const tips = (await dialogs()).filter((d) => d.tip);
    if (!tips.length) return;
    if (!(await dialogButton(['Got it', 'Close', 'Dismiss', 'Skip tour', 'Skip']))) {
      await page.keyboard.press('Escape');
    }
    await sleep(600);
  }
}

/// Wait until the client looks idle: no busy indicators for `quietMs`.
async function waitIdle(maxMs = 30000, quietMs = 1200) {
  const start = Date.now();
  let quietSince = Date.now();
  while (Date.now() - start < maxMs) {
    const busy = await inFrame(() => {
      const vis = (e) => e.getClientRects().length > 0;
      return [...document.querySelectorAll('[aria-busy=true], .ms-nav-busy, .spinner, .ms-Spinner, [class*=loading-indicator], [class*=progress-indicator], .ms-nav-loading')].filter(vis).length;
    });
    if (busy) quietSince = Date.now();
    else if (Date.now() - quietSince >= quietMs) return true;
    await sleep(150);
  }
  return false;
}

async function title() {
  try {
    return await page.title();
  } catch {
    return '';
  }
}
const GENERIC_TITLE = /^(Dynamics 365 Business Central|Sign In|)$/;

/// Wait until the document title differs from `prev` and is a form caption, or a dialog/error shows.
async function waitTitleChange(prev, maxMs = 30000) {
  const start = Date.now();
  while (Date.now() - start < maxMs) {
    const t = await title();
    if (t && t !== prev && !GENERIC_TITLE.test(t)) return { title: t };
    if ((await dialogs()).some((d) => !d.tip)) return { title: t, dialog: true };
    if (await sessionState()) return { title: t, dropped: true };
    await sleep(100);
  }
  return { title: await title(), timeout: true };
}

/// The page's main scroll container (role center body, card body or list).
async function scrollMain(stepPx = null, pauseMs = 700, maxSteps = 400) {
  return inFrame(
    async ([stepPx, pauseMs, maxSteps]) => {
      const vis = (e) => e.getClientRects().length > 0;
      const els = [...document.querySelectorAll('*')].filter((e) => vis(e) && e.scrollHeight > e.clientHeight + 40 && /(auto|scroll)/.test(getComputedStyle(e).overflowY) && !e.closest('[role=dialog]'));
      if (!els.length) return { scrolled: 0, reason: 'nothing to scroll' };
      els.sort((a, b) => b.clientHeight * b.clientWidth - a.clientHeight * a.clientWidth);
      const s = els[0];
      let n = 0;
      for (let guard = 0; guard < maxSteps; guard++) {
        const before = s.scrollTop;
        s.scrollTop = before + (stepPx || Math.max(200, s.clientHeight / 2));
        n++;
        await new Promise((r) => setTimeout(r, pauseMs));
        if (s.scrollTop <= before + 1 && s.scrollTop + s.clientHeight >= s.scrollHeight - 2) break; // bottom, and nothing more loaded
      }
      return { scrolled: n, scrollHeight: s.scrollHeight, clientHeight: s.clientHeight, atBottom: s.scrollTop + s.clientHeight >= s.scrollHeight - 2 };
    },
    [stepPx, pauseMs, maxSteps],
  );
}

/// Scroll every visible grid's own scroll container to its end (document lines, list parts).
async function scrollGrids() {
  return inFrame(async () => {
    const vis = (e) => e.getClientRects().length > 0;
    const out = [];
    for (const g of [...document.querySelectorAll('[role=grid]')].filter(vis)) {
      let s = g;
      while (s && !(s.scrollHeight > s.clientHeight + 20 && /(auto|scroll)/.test(getComputedStyle(s).overflowY))) s = s.parentElement;
      if (!s || s === document.body || s === document.documentElement) continue;
      s.scrollIntoView({ block: 'center' });
      let k = 0;
      for (; k < 30; k++) {
        const b = s.scrollTop;
        s.scrollTop = b + s.clientHeight;
        await new Promise((r) => setTimeout(r, 400));
        if (s.scrollTop <= b + 1) break;
      }
      out.push({ rows: g.querySelectorAll('[role=row]').length, steps: k });
    }
    return out;
  });
}

/// Mark and click the first visible row link of the topmost list (opens its card or document).
async function openFirstRow() {
  const label = await inFrame(() => {
    const vis = (el) => el && el.getClientRects().length;
    document.querySelectorAll('[data-wc]').forEach((e) => e.removeAttribute('data-wc'));
    const rows = [...document.querySelectorAll('[role=grid] [role=row]')].filter((r) => vis(r) && r.querySelector('[role=gridcell]') && !r.closest('[role=dialog]'));
    for (const r of rows) {
      const a = [...r.querySelectorAll('a, [role=button]')].find((a) => vis(a) && a.textContent.trim() && !/Select row|Show more options|More options/i.test(a.title || a.getAttribute('aria-label') || ''));
      if (a) {
        a.setAttribute('data-wc', '1');
        return a.textContent.trim().slice(0, 60);
      }
    }
    return null;
  });
  if (typeof label !== 'string') return null;
  await cf().locator('[data-wc="1"]').first().click({ timeout: 15000, force: true });
  return label;
}

/// Any dialog open now is a problem of the step: an error the client shows while a page opens
/// (closing the page afterwards would hide it from the end-of-step check). Closes it.
async function dialogsNow(r, where) {
  for (let i = 0; i < 3; i++) {
    const dl = (await dialogs()).filter((d) => !d.tip);
    if (!dl.length) return;
    r.problems.push(`dialog ${where}: ` + dl[dl.length - 1].text.slice(0, 300));
    if (!(await dialogButton(['OK', 'Close', 'Cancel', 'No']))) await page.keyboard.press('Escape');
    await sleep(800);
  }
}

/// Leave the current form: the client's Back/Close button, else Escape, else browser back.
async function closeForm(prevTitle) {
  const before = await title();
  const clicked = await inFrame(() => {
    const vis = (e) => e.getClientRects().length > 0;
    document.querySelectorAll('[data-wc]').forEach((e) => e.removeAttribute('data-wc'));
    const b = [...document.querySelectorAll('button, [role=button], a')].find((b) => vis(b) && !b.closest('[role=dialog]') && /^(Back|Close)$/.test((b.getAttribute('aria-label') || b.title || '').trim()));
    if (!b) return false;
    b.setAttribute('data-wc', '1');
    return true;
  });
  if (clicked === true) await cf().locator('[data-wc="1"]').first().click({ force: true, timeout: 15000 });
  else await page.keyboard.press('Escape');
  for (let i = 0; i < 100; i++) {
    const t = await title();
    if (t !== before || (prevTitle && t === prevTitle)) break;
    await sleep(100);
  }
  await waitIdle(15000);
  return (await title()) !== before;
}

const hasCues = async () => (await inFrame(() => document.querySelectorAll('.cue-tile-container a[id^=cuetile]').length)) > 0;

/// Back on the role center with its cue tiles: reload the client if a step left it elsewhere.
async function homeWithCues() {
  if (await hasCues()) return;
  await page.goto(BASE, { waitUntil: 'domcontentloaded', timeout: 60000 });
  for (let i = 0; i < 300 && !(await hasCues()); i++) {
    if (await sessionState()) return;
    await sleep(100);
  }
  await waitIdle(20000);
  await closeTips();
}

async function shot(name) {
  const f = path.join(OUT, name.replace(/[^\w.-]+/g, '_').slice(0, 80) + '.png');
  try {
    await page.screenshot({ path: f });
    return f;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ steps

const results = [];
const nstNotes = new Map();
let needSignin = true;

async function signin() {
  await page.goto(BASE, { waitUntil: 'domcontentloaded', timeout: 60000 });
  if (await page.locator('#UserName').count()) {
    await page.fill('#UserName', USER);
    await page.fill('#Password', PASSWORD);
    await Promise.all([page.waitForNavigation({ timeout: 120000 }).catch(() => {}), page.click('button[type=submit], input[type=submit]')]);
  }
  for (let i = 0; i < 600; i++) {
    const n = await inFrame(() => document.querySelectorAll('.ms-nav-layout-rolecenter, .cue-tile-container, [role=menuitem]').length);
    if (typeof n === 'number' && n > 3) break;
    if (await sessionState()) break;
    await sleep(100);
  }
  await waitIdle(30000);
  await closeTips();
  needSignin = false;
}

/// Navigate to a page by id through the client URL (one client load per page).
async function openPage(query) {
  await page.goto(BASE + '?' + query, { waitUntil: 'domcontentloaded', timeout: 60000 });
  const v = await waitTitleChange('__none__', 45000);
  await waitIdle(20000);
  await closeTips();
  return v;
}

/// Run one step: mark the logs, run, then collect log lines, dialogs and the session state.
async function step(name, fn) {
  if ((ONLY && !ONLY.test(name)) || (SKIP && SKIP.test(name))) return;
  if (needSignin && name !== 'sign in') {
    try {
      await newContext();
      await signin();
    } catch (e) {
      log('re-sign-in failed', e.message.split('\n')[0]);
    }
  }
  webLog?.mark();
  nstLog?.mark();
  consoleErrors.length = 0;
  const s0 = Date.now();
  const r = { step: name, status: 'PASS', ms: 0, detail: '', problems: [], expectFail: [] };
  try {
    const d = await fn(r);
    if (d) r.detail = typeof d === 'string' ? d : JSON.stringify(d);
  } catch (e) {
    r.problems.push('step error: ' + e.message.split('\n')[0].slice(0, 300));
  }
  r.ms = Date.now() - s0;
  await sleep(800); // let the web server flush its log
  if (consoleErrors.length) r.console = [...consoleErrors]; // browser console errors (results.json only)
  const dlg = (await dialogs()).filter((d) => !d.tip);
  for (const d of dlg) r.problems.push('dialog: ' + d.text.slice(0, 300));
  const st = await sessionState();
  if (st) {
    r.problems.push(st);
    needSignin = true;
  }
  const web = webLog ? webLog.read() : [];
  const fails = web.filter((l) => FAIL_LINE.test(l) && !(r.expectFail || []).some((re) => re.test(l)));
  for (const l of fails) r.problems.push('web log: ' + l.replace(/\s+/g, ' ').slice(0, 400));
  if (web.some((l) => /NavCancelCredentialPrompt|PromptForCredentials/.test(l))) needSignin = true;
  for (const l of nstLog ? nstLog.read() : []) {
    if (NST_FAULT.test(l) && !(r.expectFail || []).some((re) => re.test(l))) r.problems.push('bc-nst: ' + l.replace(/^bc-nst\[\d+\]: /, '').slice(0, 300));
    if (!NST_NOTE.test(l)) continue;
    const k = l.replace(/^bc-nst\[\d+\]: /, '').replace(/\(\d+(\.\d+)? ms\)/, '').slice(0, 300);
    if (!nstNotes.has(k)) nstNotes.set(k, new Set());
    nstNotes.get(k).add(name);
  }
  if (r.problems.length) {
    r.status = 'FAIL';
    r.screenshot = await shot(name);
    // get rid of error dialogs so the next step starts clean
    for (let i = 0; i < 3 && (await dialogs()).some((d) => !d.tip); i++) {
      if (!(await dialogButton(['OK', 'Close', 'Cancel', 'No']))) await page.keyboard.press('Escape');
      await sleep(800);
    }
  }
  // a session drop the known error causes brings credential-prompt and antiforgery lines along
  const known = KNOWN.find((k) => k.re.test(name) || (r.problems.some((p) => k.re.test(p)) && r.problems.every((p) => k.re.test(p) || /NavCancelCredentialPrompt|antiforgery/i.test(p))));
  if (known) {
    r.known = known.note;
    if (r.status === 'FAIL') r.status = 'KNOWN';
  }
  results.push(r);
  log(r.status.padEnd(5), name, (r.ms / 1000).toFixed(1) + 's', r.detail.slice(0, 160), r.problems.length ? '\n          ' + r.problems.join('\n          ').slice(0, 1200) : '');
}

// lists: [step name, page id, open the first record?]
const LISTS = [
  ['Customers', 22, true],
  ['Vendors', 27, true],
  ['Items', 31, true],
  ['Sales Quotes', 9300, true],
  ['Sales Orders', 9305, true],
  ['Sales Invoices', 9301, true],
  ['Sales Credit Memos', 9302, true],
  ['Posted Sales Invoices', 143, true],
  ['Purchase Orders', 9307, true],
  ['Posted Purchase Invoices', 146, true],
  ['Chart of Accounts', 16, true],
  ['G/L Entries', 20, false],
  ['Customer Ledger Entries', 25, false],
  ['Vendor Ledger Entries', 29, false],
  ['Item Ledger Entries', 38, false],
  ['Bank Accounts', 371, true],
  ['General Journals', 251, false],
  ['General Journal', 39, false],
  ['Dimension Set Entries', 479, false],
  ['Dimension Set Entries FactBox', 699, false],
];

// WEBCHECK_DEEP=1: the largest lists, scrolled to the last row and back in steps, page-down
// bursts and fast wheel bursts
const DEEP_LISTS = [
  ['G/L Entries', 20],
  ['Item Ledger Entries', 38],
  ['Customer Ledger Entries', 25],
  ['Value Entries', 5802],
];

/// Install `window.__wcScroller()` in the client frame: the list's own scroll container, the
/// largest scrollable element inside a grid outside the factboxes (the client's virtualized
/// list body; its grid's aria-rowcount grows as rows are fetched).
async function installScroller() {
  return inFrame(() => {
    window.__wcScroller = () => {
      const vis = (e) => e.getClientRects().length > 0;
      const els = [...document.querySelectorAll('[role=grid] *')].filter(
        (e) => vis(e) && e.scrollHeight > e.clientHeight + 40 && /(auto|scroll)/.test(getComputedStyle(e).overflowY) && !e.closest('[role=dialog], .ms-nav-layout-factbox-content-area'),
      );
      els.sort((a, b) => b.clientHeight * b.clientWidth - a.clientHeight * a.clientWidth);
      return els[0] || null;
    };
    return true;
  });
}

async function scrollerState() {
  return inFrame(() => {
    const s = window.__wcScroller && window.__wcScroller();
    if (!s) return null;
    const g = s.closest('[role=grid]');
    const rows = [...g.querySelectorAll('[role=row]')].filter((r) => r.getClientRects().length && r.querySelector('[role=gridcell]'));
    const txt = (r) => (r ? r.innerText.replace(/\s+/g, ' ').trim().slice(0, 80) : '');
    return { top: Math.round(s.scrollTop), height: s.scrollHeight, client: s.clientHeight, rowCount: Number(g.getAttribute('aria-rowcount')) || null, first: txt(rows[0]), last: txt(rows[rows.length - 1]) };
  });
}

async function scrollBy(dy) {
  return inFrame((dy) => {
    const s = window.__wcScroller && window.__wcScroller();
    if (!s) return null;
    s.scrollTop = s.scrollTop + dy * s.clientHeight;
    return Math.round(s.scrollTop);
  }, dy);
}

/// Mouse wheel over the list (main-page coordinates of the client frame's scroller).
async function wheelBurst(dy, n) {
  const fe = await cf().frameElement().catch(() => null);
  const off = fe ? await fe.boundingBox() : { x: 0, y: 0 };
  const box = await inFrame(() => {
    const s = window.__wcScroller && window.__wcScroller();
    if (!s) return null;
    const b = s.getBoundingClientRect();
    return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
  });
  if (!box || box.__error) return;
  await page.mouse.move((off?.x || 0) + box.x, (off?.y || 0) + box.y);
  for (let i = 0; i < n; i++) await page.mouse.wheel(0, dy);
}

/// Scroll a list to its last row and back: steps (half a screen, a short pause, until the
/// end stops growing or maxSteps), then page-down bursts on a focused cell with Ctrl+End /
/// Ctrl+Home, then fast wheel bursts down and up. Stops early when the session drops.
async function deepScroll(r, maxSteps) {
  const out = { steps: 0 };
  const dropped = async () => !!(await sessionState()) || (await dialogs()).some((d) => !d.tip);
  let still = 0;
  await installScroller();
  let start = null;
  for (let i = 0; i < 100 && !(start && !start.__error); i++) {
    start = await scrollerState();
    if (!start) await sleep(200);
  }
  if (!start || start.__error) throw new Error('no list scroller');
  out.start = start;
  for (; out.steps < maxSteps; out.steps++) {
    await scrollBy(0.5);
    await sleep(60);
    if (out.steps % 25 === 24 && (await dropped())) break;
    const st = await scrollerState();
    if (!st || st.__error) {
      out.lost = st;
      break;
    }
    if (st.top + st.client >= st.height - 2) {
      // at the bottom: wait for the next page of rows; three waits without growth = the end
      await waitIdle(5000, 600);
      const st2 = await scrollerState();
      if (st2 && st2.height <= st.height) {
        if (++still >= 3) {
          out.end = st2;
          break;
        }
      } else still = 0;
    } else still = 0;
  }
  out.reachedEnd = !!out.end;
  if (!out.end) out.end = await scrollerState();
  if (await dropped()) return out;
  // back to the top in big jumps
  for (let i = 0; i < 200; i++) {
    const t = await scrollBy(-3);
    await sleep(30);
    if (t === 0) break;
  }
  await waitIdle(10000);
  out.backAtTop = (await scrollerState())?.top === 0;
  // page-down bursts on a focused cell, then Ctrl+End / Ctrl+Home
  const focused = await inFrame(() => {
    const c = [...document.querySelectorAll('[role=grid] [role=gridcell]')].find((c) => c.getClientRects().length && !c.querySelector('a'));
    if (!c) return false;
    c.setAttribute('data-wc', '1');
    return true;
  });
  if (focused === true) {
    await cf().locator('[data-wc="1"]').first().click({ force: true, timeout: 15000 });
    for (let b = 0; b < 3; b++) {
      for (let i = 0; i < 40; i++) await page.keyboard.press('PageDown', { delay: 10 });
      await sleep(300);
    }
    await page.keyboard.press('Control+End');
    await waitIdle(15000);
    out.ctrlEnd = await scrollerState();
    if (await dropped()) return out;
    await page.keyboard.press('Control+Home');
    await waitIdle(15000);
    for (let i = 0; i < 40; i++) await page.keyboard.press('PageDown', { delay: 5 });
    for (let i = 0; i < 40; i++) await page.keyboard.press('PageUp', { delay: 5 });
    await waitIdle(15000);
    if (await dropped()) return out;
  }
  // fast wheel bursts
  await wheelBurst(4000, 150);
  await sleep(500);
  await wheelBurst(-4000, 150);
  await wheelBurst(4000, 300);
  await waitIdle(15000);
  out.afterWheel = await scrollerState();
  await wheelBurst(-6000, 300);
  await waitIdle(15000);
  if (!out.reachedEnd) r.problems.push(`did not reach the last row in ${out.steps} steps (${JSON.stringify(out.end)})`);
  return out;
}

const found = { customer: env.WEBCHECK_CUSTOMER || null, item: env.WEBCHECK_ITEM || null };

(async () => {
  browser = await chromium.launch(env.WEBCHECK_CHROME ? { executablePath: env.WEBCHECK_CHROME } : {});
  await newContext();
  log('webcheck', BASE, 'as', USER, '->', OUT);

  await step('sign in', async () => {
    await signin();
    return 'title ' + JSON.stringify(await title());
  });

  await step('role center: scroll to bottom', async (r) => {
    const s = await scrollMain(null, 900);
    if (s.__error) throw new Error(s.__error);
    await waitIdle(30000);
    const bottom = await inFrame(() => [...document.querySelectorAll('a, button, [role=button]')].some((b) => /^Back to top$/.test(b.textContent.trim()) && b.getClientRects().length));
    if (!s.atBottom) r.problems.push('did not reach the bottom: ' + JSON.stringify(s));
    await inFrame(() => {
      const s = document.querySelector('.ms-nav-layout-body.ms-nav-scrollable');
      if (s) s.scrollTop = 0;
    });
    return `${s.scrolled} scroll steps, height ${s.scrollHeight}, "Back to top" ${bottom ? 'visible' : 'not found'}`;
  });

  // cue tiles: open each drill-down and come back
  let cues = [];
  await step('role center: list cue tiles', async () => {
    cues = await inFrame(() =>
      [...document.querySelectorAll('.cue-tile-container a[id^=cuetile]')].map((a) => ({
        id: a.id,
        caption: [...a.querySelectorAll('[class*=caption] span')].map((s) => s.textContent.trim()).join(' ').replace(/\s+/g, ' ') || a.title,
        value: (a.querySelector('.value') || {}).textContent || '',
      })),
    );
    if (!Array.isArray(cues)) throw new Error('cannot read cue tiles');
    return `${cues.length} cue tiles`;
  });
  const maxCues = parseInt(env.WEBCHECK_MAX_CUES || '1000');
  for (const [i, c] of cues.slice(0, maxCues).entries()) {
    await step(`cue ${i + 1}: ${c.caption} (${c.value.trim()})`, async (r) => {
      await homeWithCues();
      const home = await title();
      // the tile ids are per client load; find the tile again by caption
      const ok = await inFrame(
        ([caption, idx]) => {
          document.querySelectorAll('[data-wc]').forEach((e) => e.removeAttribute('data-wc'));
          const all = [...document.querySelectorAll('.cue-tile-container a[id^=cuetile]')];
          const a = all.filter((a) => ([...a.querySelectorAll('[class*=caption] span')].map((s) => s.textContent.trim()).join(' ').replace(/\s+/g, ' ') || a.title) === caption)[0] || all[idx];
          if (!a) return false;
          a.scrollIntoView({ block: 'center' });
          a.setAttribute('data-wc', '1');
          return true;
        },
        [c.caption, i],
      );
      if (ok !== true) throw new Error('tile not found');
      await cf().locator('[data-wc="1"]').first().click({ force: true, timeout: 15000 });
      const v = await waitTitleChange(home, 12000);
      await waitIdle(20000);
      // a cue without DrillDownPageId / OnDrillDown does nothing in BC either
      if (v.timeout) return 'no drill-down page (nothing opened within 12 s)';
      const opened = v.title;
      await dialogsNow(r, 'on open');
      if (!v.dropped && !(await dialogs()).some((d) => !d.tip)) {
        await scrollMain(null, 400);
        if (!(await closeForm(home))) r.problems.push('could not close ' + JSON.stringify(opened));
      }
      return 'opened ' + JSON.stringify(opened);
    });
  }

  for (const [name, id, openFirst] of LISTS) {
    await step(`list ${id} ${name}`, async (r) => {
      const v = await openPage(`page=${id}`);
      if (v.timeout) r.problems.push('page did not open within 45 s');
      if (v.dropped || v.dialog) return 'title ' + JSON.stringify(v.title);
      const listTitle = v.title;
      await dialogsNow(r, 'on open');
      // long lists load more rows while scrolling; 25 steps fetch several pages
      const s = await scrollMain(null, 500, 25);
      await waitIdle(15000);
      let detail = `title ${JSON.stringify(listTitle)}, scrolled ${s.scrolled ?? 0}`;
      if (!openFirst) return detail;
      await inFrame(() => {
        const vis = (e) => e.getClientRects().length > 0;
        const els = [...document.querySelectorAll('*')].filter((e) => vis(e) && e.scrollHeight > e.clientHeight + 40 && /(auto|scroll)/.test(getComputedStyle(e).overflowY));
        els.forEach((e) => (e.scrollTop = 0));
      });
      await sleep(500);
      const label = await openFirstRow();
      if (!label) return detail + ', no row link to open';
      // the first row's link is its No. on Customers and Items: the sales order uses them
      if (id === 22 && !found.customer) found.customer = label;
      if (id === 31 && !found.item) found.item = label;
      const c = await waitTitleChange(listTitle, 30000);
      await waitIdle(20000);
      if (c.timeout) r.problems.push(`clicking "${label}" opened nothing within 30 s`);
      if (c.dropped || c.dialog) return detail + `, opened ${JSON.stringify(c.title)}`;
      await closeTips();
      await dialogsNow(r, 'on opening the record');
      const m = await scrollMain(null, 500, 40);
      const g = await scrollGrids();
      await waitIdle(15000);
      detail += `, opened ${JSON.stringify(c.title)} (scrolled ${m.scrolled ?? 0}, grids ${Array.isArray(g) ? g.map((x) => x.rows).join('/') : '?'})`;
      await closeForm(listTitle);
      return detail;
    });
  }

  if (/^(1|true|yes)$/i.test(env.WEBCHECK_DEEP || '')) {
    const maxSteps = parseInt(env.WEBCHECK_DEEP_STEPS || '600');
    for (const [name, id] of DEEP_LISTS) {
      await step(`deep scroll ${id} ${name}`, async (r) => {
        const v = await openPage(`page=${id}`);
        if (v.timeout || v.dropped || v.dialog) throw new Error('page did not open: ' + JSON.stringify(v));
        return deepScroll(r, maxSteps);
      });
    }
  }

  // a page id that does not exist: the client shows an error and keeps the session (real NST)
  await step('missing page 9176: error shown, session kept', async (r) => {
    // the web server logs the page build error with the real NST too (data/capture/nst-real/webserver.log)
    r.expectFail = [/NavPageBuildErrorException: Cannot build the page 9176\./, /BeaconController.*Antiforgery/];
    await page.goto(BASE + '?page=9176', { waitUntil: 'domcontentloaded', timeout: 60000 });
    let shown = null;
    for (let i = 0; i < 200 && !shown; i++) {
      shown = (await dialogs()).find((d) => !d.tip)?.text || null;
      if (!shown && (await sessionState())) break;
      await sleep(100);
    }
    if (!shown) r.problems.push('no error shown for page 9176');
    else if (!(await dialogButton(['OK', 'Close']))) await page.keyboard.press('Escape');
    await sleep(1000);
    const v = await openPage('page=22');
    if (v.timeout || v.dropped) r.problems.push('session not usable afterwards: ' + JSON.stringify(v));
    return `error: ${JSON.stringify((shown || '').slice(0, 120))}; then ${JSON.stringify(v.title)}`;
  });

  if (!/^(0|false|no)$/i.test(env.WEBCHECK_POST || '1')) {
    let orderTitle = '';
    await step('sales order: create with one item line', async (r) => {
      if (!found.customer || !found.item) throw new Error(`no customer/item number (customer ${found.customer}, item ${found.item}); set WEBCHECK_CUSTOMER / WEBCHECK_ITEM`);
      const v = await openPage('page=42&mode=create');
      if (v.timeout || v.dropped || v.dialog) throw new Error('page 42 did not open: ' + JSON.stringify(v));
      await fill('Sell-to Customer Name', found.customer, r);
      await waitIdle(15000);
      await answerDialogs(r, ['Yes', 'OK']);
      await lineCell('No.', found.item, r);
      await answerDialogs(r, ['Yes', 'OK']);
      await lineCell('Quantity', '1', r);
      await answerDialogs(r, ['Yes', 'OK']);
      const qty = await inFrame(() => {
        const g = [...document.querySelectorAll('[role=grid]')].find((g) => [...g.querySelectorAll('[role=columnheader]')].some((h) => h.textContent.trim() === 'Quantity'));
        return g ? g.innerText.replace(/\s+/g, ' ').slice(0, 200) : null;
      });
      if (!r.problems.length) orderTitle = await title();
      return `customer ${found.customer}, item ${found.item}, title ${JSON.stringify(await title())}; lines: ${qty}`;
    });
    await step('sales order: post (Ship and Invoice)', async (r) => {
      if (!orderTitle || !/Sales Order/.test(await title())) throw new Error('no sales order open');
      await clickAction(['Post...', 'Post']);
      const seen = [];
      let posted = null;
      const t0 = Date.now();
      // until the "posted as number" question, or the order page closes (the order is gone)
      while (Date.now() - t0 < 90000 && !posted) {
        await sleep(500);
        const dl = (await dialogs()).filter((d) => !d.tip);
        if (dl.length) {
          const d = dl[dl.length - 1];
          seen.push(d.text.slice(0, 160));
          if (/posted as number|open the posted/i.test(d.text)) {
            posted = d.text.match(/posted as number \S+/i)?.[0] || 'confirmed';
            await dialogButton(['No']);
          } else if (/Ship and Invoice/.test(d.text) && (await dialogButton(['Ship and Invoice']))) {
            await sleep(500);
            await dialogButton(['OK']);
          } else if (/Do you want|Are you sure/i.test(d.text) && (await dialogButton(['Yes']))) {
          } else {
            r.problems.push('unexpected dialog: ' + d.text.slice(0, 300));
            break;
          }
          await sleep(1000);
          await waitIdle(30000);
        } else if (seen.length && (await title()) !== orderTitle) {
          posted = 'order page closed (' + JSON.stringify(await title()) + ')';
        }
      }
      if (!posted && !r.problems.length) r.problems.push('no sign that the order was posted within 90 s');
      await waitIdle(30000);
      return `posted: ${posted}; dialogs: ` + seen.join(' | ');
    });
  }

  await browser.close();
  report();
})().catch((e) => {
  console.error('webcheck crashed:', e);
  report();
  process.exit(3);
});

async function fill(controlname, value, r) {
  const ok = await inFrame((want) => {
    const vis = (e) => e && e.getClientRects().length > 0;
    document.querySelectorAll('[data-wc]').forEach((e) => e.removeAttribute('data-wc'));
    let el = [...document.querySelectorAll('[controlname]')].filter((c) => c.getAttribute('controlname') === want).map((c) => c.querySelector('input, textarea')).find(vis);
    if (!el)
      for (const i of document.querySelectorAll('input, textarea')) {
        const ids = (i.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
        if (vis(i) && (ids.some((id) => (document.getElementById(id)?.textContent || '').trim() === want) || i.getAttribute('aria-label') === want)) {
          el = i;
          break;
        }
      }
    if (!el) return false;
    el.setAttribute('data-wc', '1');
    return true;
  }, controlname);
  if (ok !== true) throw new Error(`field ${controlname} not found`);
  const loc = cf().locator('[data-wc="1"]');
  await loc.click({ force: true, timeout: 15000 });
  await loc.fill(value, { force: true });
  await loc.press('Tab');
  await sleep(2000);
  await waitIdle(15000);
}

/// Type into a cell of the last visible grid (document lines): the row being edited, else the first row.
async function lineCell(column, value, r) {
  const ok = await inFrame((want) => {
    const vis = (e) => e && e.getClientRects().length > 0;
    document.querySelectorAll('[data-wc]').forEach((e) => e.removeAttribute('data-wc'));
    // the lines part: the first visible grid (outside dialogs and factboxes) with that column
    const grids = [...document.querySelectorAll('[role=grid]')].filter((g) => vis(g) && !g.closest('[role=dialog]'));
    const colIx = (g) => [...g.querySelectorAll('[role=columnheader]')].findIndex((h) => h.textContent.trim() === want && vis(h));
    const g = grids.find((g) => colIx(g) >= 0);
    if (!g) return 'no grid with column ' + want;
    g.scrollIntoView({ block: 'center' });
    const hdr = [...g.querySelectorAll('[role=columnheader]')].find((h) => h.textContent.trim() === want && vis(h));
    const rows = [...g.querySelectorAll('[role=row]')].filter((r) => vis(r) && r.querySelector('[role=gridcell]'));
    const row = rows.find((r) => r.getAttribute('aria-selected') === 'true' || r.classList.contains('selected')) || rows[0];
    if (!row) return 'no row';
    // the row has marker cells the header row lacks: take the cell under the header's center
    const hb = hdr.getBoundingClientRect();
    const x = hb.left + hb.width / 2;
    const cell = [...row.querySelectorAll('[role=gridcell], td')].filter(vis).find((c) => {
      const b = c.getBoundingClientRect();
      return b.left <= x && x <= b.right;
    });
    if (!cell) return 'no cell under ' + want;
    // the cell itself: its input stays hidden until the cell is active
    cell.setAttribute('data-wc', '1');
    return 'ok';
  }, column);
  if (ok !== 'ok') throw new Error(`line ${column}: ${ok}`);
  const loc = cf().locator('[data-wc="1"]').first();
  await loc.click({ force: true, timeout: 15000 });
  await sleep(400);
  await page.keyboard.press('Control+A');
  await page.keyboard.type(value);
  await page.keyboard.press('Tab');
  await sleep(2000);
  await waitIdle(15000);
}

/// Answer confirm dialogs that a step expects (e.g. "Do you want to ...?") with the first matching button.
async function answerDialogs(r, labels) {
  for (let i = 0; i < 3; i++) {
    const dl = (await dialogs()).filter((d) => !d.tip);
    if (!dl.length) return;
    const d = dl[dl.length - 1];
    if (!/\?\s*$|Do you want|Are you sure/i.test(d.text)) return; // not a question: leave it for the step's check
    const b = await dialogButton(labels);
    if (!b) return;
    r.detail = (r.detail ? r.detail + '; ' : '') + `answered "${b}" to: ${d.text.slice(0, 120)}`;
    await sleep(1500);
    await waitIdle(15000);
  }
}

async function clickAction(labels) {
  const ok = await inFrame((want) => {
    const vis = (e) => e.getClientRects().length > 0;
    document.querySelectorAll('[data-wc]').forEach((e) => e.removeAttribute('data-wc'));
    const all = [...document.querySelectorAll('button, [role=button], [role=menuitem], a')].filter((b) => vis(b) && !b.closest('[role=dialog]'));
    for (const w of want) {
      const el = all.find((b) => [b.getAttribute('aria-label'), b.title, b.textContent].some((t) => (t || '').trim() === w));
      if (el) {
        el.setAttribute('data-wc', '1');
        return w;
      }
    }
    return null;
  }, labels);
  if (typeof ok !== 'string') throw new Error(`no action ${labels.join(' / ')}`);
  await cf().locator('[data-wc="1"]').first().click({ force: true, timeout: 15000 });
}

function report() {
  const w = Math.min(70, Math.max(10, ...results.map((r) => r.step.length)));
  const lines = [];
  lines.push('');
  lines.push(`${'step'.padEnd(w)}  result   secs  detail / problems`);
  lines.push('-'.repeat(w + 40));
  for (const r of results) {
    lines.push(`${r.step.slice(0, w).padEnd(w)}  ${r.status.padEnd(6)} ${(r.ms / 1000).toFixed(1).padStart(5)}  ${r.detail.slice(0, 140)}`);
    for (const p of r.problems) lines.push(`${''.padEnd(w)}    ! ${p.slice(0, 400)}`);
    if (r.known) lines.push(`${''.padEnd(w)}    known: ${r.known}`);
  }
  const n = (s) => results.filter((r) => r.status === s).length;
  lines.push('-'.repeat(w + 40));
  lines.push(`${results.length} steps: ${n('PASS')} PASS, ${n('FAIL')} FAIL, ${n('KNOWN')} KNOWN`);
  if (nstNotes.size) {
    lines.push('');
    lines.push('bc-nst log lines during the run (not failures by themselves):');
    for (const [k, steps] of nstNotes) lines.push(`  ${k}   [${[...steps].slice(0, 3).join('; ')}${steps.size > 3 ? '; ...' : ''}]`);
  }
  const text = lines.join('\n');
  console.log(text);
  fs.writeFileSync(path.join(OUT, 'results.json'), JSON.stringify({ base: BASE, at: new Date().toISOString(), results, nstNotes: [...nstNotes].map(([k, s]) => ({ line: k, steps: [...s] })) }, null, 1));
  fs.writeFileSync(path.join(OUT, 'report.txt'), text + '\n');
  process.exitCode = n('FAIL') ? 1 : 0;
}
