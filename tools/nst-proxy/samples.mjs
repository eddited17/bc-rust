// Write pretty-printed samples from a proxy.mjs log.
//
// usage: node tools/nst-proxy/samples.mjs <nst.jsonl> <outDir> <name>=<seq> [<name>=<seq> ...]
//
// <seq> is the proxy record number of a request (JSON-RPC request, HTTP request or WS
// upgrade); its response is added automatically. Each sample becomes <outDir>/<name>.json:
//   { "request": <record>, "response": <record> }
// Inside the bodies, every NavDataSet gets an "x-decoded" member (tools/nst-proxy/navdataset.mjs)
// and every ServerFault/FormOpenExceptionData "exceptionData" gets "x-exceptionData-decoded".
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { annotateDataSets } from "./navdataset.mjs";

const [file, outDir, ...specs] = process.argv.slice(2);
if (!file || !outDir || !specs.length) {
  console.error("usage: samples.mjs <nst.jsonl> <outDir> <name>=<seq> ...");
  process.exit(2);
}
const recs = readFileSync(file, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
const bySeq = new Map(recs.map((r) => [r.seq, r]));

function decodeExceptionData(v) {
  if (Array.isArray(v)) return v.map(decodeExceptionData);
  if (v && typeof v === "object") {
    const o = {};
    for (const [k, x] of Object.entries(v)) {
      o[k] = decodeExceptionData(x);
      if (/exceptionData$/i.test(k) && typeof x === "string" && x.length) {
        try { o[`x-${k}-decoded`] = JSON.parse(Buffer.from(x, "base64").toString("utf8")); } catch {}
      }
    }
    return o;
  }
  return v;
}
const annotate = (r) => (r && "body" in r ? { ...r, body: decodeExceptionData(annotateDataSets(r.body)) } : r);

function responseFor(q) {
  const after = recs.filter((r) => r.seq > q.seq && r.conn === q.conn);
  if (q.channel === "http") return after.find((r) => r.kind === "response");
  if (q.kind === "upgrade") return after.find((r) => r.kind === "upgrade-response");
  if (q.rpc?.kind === "request") return after.find((r) => r.rpc && r.rpc.kind !== "request" && r.rpc.kind !== "notification" && JSON.stringify(r.rpc.id) === JSON.stringify(q.rpc.id) && r.dir !== q.dir);
  return null;
}

mkdirSync(outDir, { recursive: true });
for (const spec of specs) {
  const [name, seqStr] = spec.split("=");
  const q = bySeq.get(Number(seqStr));
  if (!q) { console.error(`${name}: no record ${seqStr}`); continue; }
  const resp = responseFor(q);
  const out = resp ? { request: annotate(q), response: annotate(resp) } : { message: annotate(q) };
  const text = JSON.stringify(out, null, 2);
  writeFileSync(join(outDir, `${name}.json`), text + "\n");
  console.log(`${name}.json  seq ${q.seq}${resp ? ` + ${resp.seq}` : ""}  ${(text.length / 1024).toFixed(0)} KB`);
}
