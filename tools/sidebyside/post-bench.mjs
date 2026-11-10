// Side-by-side posting benchmark: create a sales order with L lines and post it, N times, through REST API
// v2.0 (salesOrders, salesOrderLines, bound action Microsoft.NAV.shipAndInvoice), against real Business
// Central and bc-rust alike. Prints median and p95 ms per step (create, lines, post, total).
// `pageapi` posts through bc-rust's page API instead (/api/page/42/action/Post), for builds without bound
// actions (before 2026-11-08 I13).
// usage: node tools/sidebyside/post-bench.mjs <name> <apiBase> <user:password|-> <customerNo> <itemNo> <lines> <n> <warmup> <out.json> [pageapi]
//   e.g. node tools/sidebyside/post-bench.mjs bc-rust http://127.0.0.1:8740/api/v2.0 - 10000 1896-S 1 20 3 /tmp/post.json
import { writeFileSync } from "node:fs";

const [name, base0, cred, customer, item, linesS, nS, warmS, out, mode] = process.argv.slice(2);
const L = Number(linesS), N = Number(nS), W = Number(warmS);
const base = base0.replace(/\/$/, "");
const auth = cred && cred !== "-" ? "Basic " + Buffer.from(cred).toString("base64") : null;

async function call(method, url, body) {
  const headers = { "content-type": "application/json", accept: "application/json" };
  if (auth) headers.authorization = auth;
  if (method === "PATCH" || method === "DELETE") headers["if-match"] = "*";
  const s = performance.now();
  const r = await fetch(url, { method, headers, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  const ms = performance.now() - s;
  if (!r.ok) throw new Error(`${name} ${method} ${url}: ${r.status} ${text.slice(0, 400)}`);
  return { ms, json: text ? JSON.parse(text) : null };
}

const companies = (await call("GET", `${base}/companies`)).json.value;
const c = companies.find((x) => /cronus/i.test(x.name)) || companies[0];
const cb = `${base}/companies(${c.id})`;
const pageBase = base.replace(/\/api\/v2\.0$/, "");

async function one() {
  const t0 = performance.now();
  const so = await call("POST", `${cb}/salesOrders`, { customerNumber: customer });
  const id = so.json.id, no = so.json.number;
  let linesMs = 0;
  for (let i = 0; i < L; i++) {
    const r = await call("POST", `${cb}/salesOrderLines`, { documentId: id, lineType: "Item", lineObjectNumber: item, quantity: 1 + (i % 3) });
    linesMs += r.ms;
  }
  let post;
  if (mode === "pageapi") {
    const key = encodeURIComponent(JSON.stringify(["Order", no]));
    post = await call("POST", `${pageBase}/api/page/42/action/Post?key=${key}`);
  } else {
    post = await call("POST", `${cb}/salesOrders(${id})/Microsoft.NAV.shipAndInvoice`);
  }
  return { order: no, create_ms: so.ms, lines_ms: linesMs, post_ms: post.ms, total_ms: performance.now() - t0 };
}

const res = [];
for (let i = 0; i < W + N; i++) {
  const r = await one();
  if (i >= W) res.push(r);
}
const med = (k) => { const s = res.map((r) => r[k]).sort((a, b) => a - b); return +s[Math.floor(s.length / 2)].toFixed(1); };
const p95 = (k) => { const s = res.map((r) => r[k]).sort((a, b) => a - b); return +s[Math.min(s.length - 1, Math.floor(0.95 * s.length))].toFixed(1); };
const summary = { name, lines: L, n: N, warmup: W, posting: mode === "pageapi" ? "page 42 Post action" : "API v2.0 Microsoft.NAV.shipAndInvoice",
  median: { create: med("create_ms"), lines: med("lines_ms"), post: med("post_ms"), total: med("total_ms") },
  p95: { create: p95("create_ms"), lines: p95("lines_ms"), post: p95("post_ms"), total: p95("total_ms") } };
writeFileSync(out, JSON.stringify({ summary, runs: res }, null, 1));
console.log(JSON.stringify(summary));
