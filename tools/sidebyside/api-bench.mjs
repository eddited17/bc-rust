// Side-by-side REST API v2.0 benchmark: identical calls against real Business
// Central and bc-rust, reporting median/p95 latency per operation.
//
// usage: node tools/sidebyside/api-bench.mjs <name>=<apiBase>[@user:password] ... [--n 30]
//   apiBase: URL ending in /api/v2.0  (companies are discovered)
import { writeFileSync } from "node:fs";

const args = process.argv.slice(2);
const nIdx = args.indexOf("--n");
const N = nIdx >= 0 ? Number(args[nIdx + 1]) : 30;
const targets = args
  .filter((a, i) => a.includes("=") && i !== nIdx + 1)
  .map((a) => {
    const [name, rest] = a.split("=");
    const [url, cred] = rest.split("@");
    return { name, url: url.replace(/\/$/, ""), auth: cred ? "Basic " + Buffer.from(cred).toString("base64") : null };
  });

async function call(t, method, path, body) {
  const headers = { "content-type": "application/json", accept: "application/json" };
  if (t.auth) headers.authorization = t.auth;
  if (method === "PATCH" || method === "DELETE") headers["if-match"] = "*";
  const s = performance.now();
  const r = await fetch(t.url + path, { method, headers, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  const ms = performance.now() - s;
  if (!r.ok) throw new Error(`${t.name} ${method} ${path}: ${r.status} ${text.slice(0, 300)}`);
  return { ms, json: text ? JSON.parse(text) : null };
}

const stats = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  const q = (p) => s[Math.min(s.length - 1, Math.floor(p * s.length))];
  return { median: +q(0.5).toFixed(2), p95: +q(0.95).toFixed(2), n: s.length };
};

const results = {};
for (const t of targets) {
  const companies = (await call(t, "GET", "/companies")).json.value;
  const c = companies.find((x) => /cronus/i.test(x.name)) || companies[0];
  const base = `/companies(${c.id})`;
  const ops = { list20: [], filter: [], get: [], create: [], update: [], delete: [] };
  // warm up
  await call(t, "GET", `${base}/customers?$top=5`);
  const first = (await call(t, "GET", `${base}/customers?$top=1`)).json.value[0];
  for (let i = 0; i < N; i++) {
    ops.list20.push((await call(t, "GET", `${base}/customers?$top=20`)).ms);
    ops.filter.push((await call(t, "GET", `${base}/customers?$filter=number eq '${first.number}'`)).ms);
    ops.get.push((await call(t, "GET", `${base}/customers(${first.id})`)).ms);
    const cr = await call(t, "POST", `${base}/customers`, { displayName: `Bench ${t.name} ${Date.now()} ${i}`, addressLine1: "1 Bench Street", city: "Seattle" });
    ops.create.push(cr.ms);
    ops.update.push((await call(t, "PATCH", `${base}/customers(${cr.json.id})`, { phoneNumber: "555-0199" })).ms);
    ops.delete.push((await call(t, "DELETE", `${base}/customers(${cr.json.id})`)).ms);
  }
  results[t.name] = Object.fromEntries(Object.entries(ops).map(([k, v]) => [k, stats(v)]));
  console.log(t.name, JSON.stringify(results[t.name]));
}
writeFileSync("/tmp/api-bench.json", JSON.stringify(results, null, 1));
const names = Object.keys(results);
console.log("\noperation        " + names.map((n) => n.padStart(22)).join(""));
for (const op of Object.keys(results[names[0]])) {
  console.log(op.padEnd(17) + names.map((n) => `${results[n][op].median} / ${results[n][op].p95} ms`.padStart(22)).join(""));
}
console.log("(median / p95, " + N + " iterations)");
