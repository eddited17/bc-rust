// Turn a proxy.mjs log (nst.jsonl) into a readable call sequence.
//
// usage: node tools/nst-proxy/summarize.mjs <nst.jsonl> [calls.txt] [--calls-jsonl <calls.jsonl>]
//
// One line per JSON-RPC call (request matched with its response by socket, direction and id), per
// JSON-RPC notification, per HTTP call and per WebSocket open/close, in request order,
// with the driver's step markers as section headers. Columns: proxy seq of the request,
// ms since proxy start, connection, caller (web = web server, nst = service tier
// callback), method or HTTP path, a short detail taken from the params, request bytes,
// response bytes (decoded), duration, outcome.
//
// --calls-jsonl rewrites the per-call file in proxy.mjs's calls.jsonl format, but in request
// order (proxy.mjs writes it live, in completion order).
import { readFileSync, writeFileSync } from "node:fs";

const argv = process.argv.slice(2);
const ci = argv.indexOf("--calls-jsonl");
const callsOut = ci >= 0 ? argv.splice(ci, 2)[1] : null;
const [file, out] = argv;
if (!file) {
  console.error("usage: summarize.mjs <nst.jsonl> [calls.txt] [--calls-jsonl <calls.jsonl>]");
  process.exit(2);
}
const recs = readFileSync(file, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));

const short = (g) => (typeof g === "string" && /^[0-9a-f-]{36}$/i.test(g) ? g.slice(0, 8) : g);
// Short description of a request's params: the ids that tell the calls apart.
function detail(method, params) {
  const p = Array.isArray(params) ? params : params ? [params] : [];
  const bits = [];
  const seen = new Set();
  const visit = (v, depth) => {
    if (!v || typeof v !== "object" || depth > 3) return;
    if (Array.isArray(v)) { v.slice(0, 6).forEach((x) => visit(x, depth + 1)); return; }
    const add = (k, val) => { const s = `${k}=${val}`; if (!seen.has(s)) { seen.add(s); bits.push(s); } };
    if (v.FormId) add("form", v.FormId);
    if (v.TableView?.TableId) add("table", v.TableView.TableId);
    if (v.ServerFormHandle && v.ServerFormHandle !== "00000000-0000-0000-0000-000000000000") add("handle", short(v.ServerFormHandle));
    if (v.ControlId !== undefined) add("control", v.ControlId);
    if (v.FieldId !== undefined) add("field", v.FieldId);
    if (v.SystemAction !== undefined) add("systemAction", v.SystemAction);
    if (v.Bookmark) add("bookmark", String(v.Bookmark).slice(0, 24));
    for (const [k, x] of Object.entries(v)) if (x && typeof x === "object" && !["TableView", "Credentials"].includes(k)) visit(x, depth + 1);
  };
  switch (method) {
    case "OpenConnection":
      return `tz=${p[0]?.TimeZoneId} lcid=${p[0]?.LCID} user=${p[0]?.Credentials?.UserName}`;
    case "OpenCompany":
      return `company=${JSON.stringify(p[0])}`;
    case "SetClientSettings":
      return `lcid=${p[0]?.ClientSettings?.LCID} saveOnly=${p[1]}`;
  }
  p.forEach((x) => visit(x, 0));
  if (!bits.length) {
    const scalars = p.filter((x) => x === null || typeof x !== "object").map((x) => JSON.stringify(x));
    if (scalars.length) return scalars.join(", ").slice(0, 60);
  }
  return bits.join(" ").slice(0, 80);
}
// Short description of a result.
function resultInfo(body) {
  if (body.error) {
    let t = body.error.message;
    const ed = body.error.data?.exceptionData;
    if (ed) {
      try { t = `${JSON.parse(Buffer.from(ed, "base64").toString("utf8"))[0].$type.split(",")[0].split(".").pop()}: ${t}`; } catch {}
    }
    return `ERROR ${body.error.code} ${String(t).split("\n")[0].slice(0, 120)}`;
  }
  const r = body.result;
  if (r === null || typeof r !== "object") return `= ${JSON.stringify(r)}`.slice(0, 60);
  const bits = [];
  if (Array.isArray(r.requests) && r.requests.length) bits.push(`requests[${r.requests.map((q) => (q.$polyType || "?").split(".").pop()).join(",")}]`);
  if (Array.isArray(r.States)) bits.push(`States×${r.States.length}`);
  if (r.dataSet || r.DataSet) bits.push(`dataSet.count=${(r.dataSet || r.DataSet).count}`);
  if (Array.isArray(r)) bits.push(`[${r.length}]`);
  return bits.join(" ");
}

const pending = new Map();
const rows = []; // { seq, line, kind }
const pad = (s, n) => String(s ?? "").padEnd(n).slice(0, Math.max(n, String(s ?? "").length));
const lpad = (s, n) => String(s ?? "").padStart(n);
for (const r of recs) {
  if (r.channel === "marker") { rows.push({ seq: r.seq, text: `\n== ${r.label}${r.result?.error ? `  (${r.result.error})` : ""}${r.result?.dialog ? `  dialog: ${r.result.dialog.slice(0, 160)}` : ""}` }); continue; }
  if (r.channel === "ws" && r.kind === "upgrade") { pending.set(`${r.conn}:upgrade`, r); continue; }
  if (r.channel === "ws" && r.kind === "upgrade-response") {
    const q = pending.get(`${r.conn}:upgrade`);
    rows.push({ seq: q?.seq ?? r.seq, t: q?.t ?? r.t, conn: r.conn, who: "web", call: `WS ${q?.method || "GET"} ${r.path}`, detail: r.permessageDeflate ? "permessage-deflate" : "", ms: r.ms, outcome: String(r.status) });
    continue;
  }
  if (r.channel === "ws" && (r.kind === "close" || r.kind === "socket-end")) {
    rows.push({ seq: r.seq, t: r.t, conn: r.conn, who: r.kind === "close" ? (r.dir === "to-nst" ? "web" : "nst") : r.by, call: r.kind === "close" ? "WS close" : "WS end", detail: r.kind === "close" ? `${r.code ?? ""} ${r.reason ?? ""}`.trim() : (r.unanswered?.length ? `unanswered: ${r.unanswered.map((u) => u.method).join(",")}` : "") });
    continue;
  }
  if (r.channel === "ws" && r.kind === "message" && r.rpc) {
    // each side numbers its own requests, so key by the direction of the request too
    const isReq = r.rpc.kind === "request" || r.rpc.kind === "notification";
    const reqDir = isReq ? r.dir : r.dir === "to-nst" ? "from-nst" : "to-nst";
    const key = `${r.conn}:${reqDir}:${JSON.stringify(r.rpc.id)}`;
    const t = r.tFrame ?? r.t;
    const paramsBytes = isReq ? JSON.stringify(r.body.params ?? null).length : undefined;
    if (r.rpc.kind === "request") {
      const call = { channel: "ws", conn: r.conn, seq: r.seq, respSeq: null, id: r.rpc.id, method: r.rpc.method, caller: r.dir === "to-nst" ? "web" : "nst", tReq: t, paramsBytes, reqBytes: r.bodyBytes, respBytes: null, respWire: null, ms: null, outcome: null };
      const row = { seq: r.seq, t: r.t, conn: r.conn, who: call.caller, call: r.rpc.method, detail: detail(r.rpc.method, r.body.params), req: r.bodyBytes, outcome: "(no response)", jsonl: call };
      rows.push(row);
      pending.set(key, row);
    } else if (r.rpc.kind === "notification") {
      rows.push({ seq: r.seq, t: r.t, conn: r.conn, who: r.dir === "to-nst" ? "web" : "nst", call: `${r.rpc.method} (notification)`, detail: detail(r.rpc.method, r.body.params), req: r.bodyBytes,
        jsonl: { channel: "ws", conn: r.conn, seq: r.seq, tReq: t, dir: r.dir, kind: "notification", method: r.rpc.method, paramsBytes } });
    } else {
      const row = pending.get(key);
      pending.delete(key);
      if (row) {
        Object.assign(row, { resp: r.bodyBytes, ms: t - row.jsonl.tReq, outcome: resultInfo(r.body) || r.rpc.kind });
        const err = r.rpc.kind === "error" ? r.body.error : null;
        Object.assign(row.jsonl, { respSeq: r.seq, respBytes: r.bodyBytes, respWire: r.wireBytes, ms: t - row.jsonl.tReq, outcome: r.rpc.kind,
          ...(err ? { errorCode: err.code, errorMessage: String(err.message ?? "").slice(0, 300) } : {}) });
      } else rows.push({ seq: r.seq, t: r.t, conn: r.conn, who: "?", call: `response to unknown id ${JSON.stringify(r.rpc.id)}`, resp: r.bodyBytes, outcome: resultInfo(r.body) });
    }
    continue;
  }
  if (r.channel === "http" && r.kind === "request") {
    const row = { seq: r.seq, t: r.t, conn: r.conn, who: "web", call: `HTTP ${r.method} ${r.path.replace(/\?.*/, "")}`, detail: (r.path.split("?")[1] || "").slice(0, 60), req: r.bodyBytes ?? r.wireBytes, outcome: "(no response)",
      jsonl: { channel: "http", conn: r.conn, seq: r.seq, tReq: r.t, method: r.method, path: r.path, status: null, reqBytes: r.wireBytes, respBytes: null, ms: null } };
    rows.push(row);
    pending.set(`${r.conn}:http`, row);
    continue;
  }
  if (r.channel === "http" && r.kind === "response") {
    const row = pending.get(`${r.conn}:http`);
    pending.delete(`${r.conn}:http`);
    if (row) {
      Object.assign(row, { resp: r.bodyBytes ?? r.wireBytes, ms: r.ms, outcome: `${r.status}${r.decoded ? ` ${r.decoded}` : ""}${r.bodyEncoding === "base64" ? " binary" : ""}` });
      Object.assign(row.jsonl, { respSeq: r.seq, status: r.status, respBytes: r.wireBytes, ms: r.ms });
    }
    continue;
  }
  if (r.kind === "error") rows.push({ seq: r.seq, t: r.t, conn: r.conn, call: `proxy error (${r.where})`, detail: r.error });
}

rows.sort((a, b) => a.seq - b.seq);
const lines = [
  `# NST call sequence from ${file}`,
  `# seq = proxy record number of the request; t = ms since proxy start; who = caller (web server or NST); bytes are decoded JSON sizes`,
  `${lpad("seq", 6)} ${lpad("t", 7)} ${pad("conn", 5)} ${pad("who", 4)} ${pad("call", 38)} ${pad("detail", 44)} ${lpad("req B", 7)} ${lpad("resp B", 8)} ${lpad("ms", 6)}  outcome`,
];
for (const r of rows) {
  if (r.text !== undefined) { lines.push(r.text); continue; }
  lines.push(`${lpad(r.seq, 6)} ${lpad(r.t, 7)} ${pad(r.conn, 5)} ${pad(r.who, 4)} ${pad(r.call, 38)} ${pad(r.detail, 44)} ${lpad(r.req, 7)} ${lpad(r.resp, 8)} ${lpad(r.ms, 6)}  ${r.outcome ?? ""}`);
}
const text = lines.join("\n") + "\n";
if (out) writeFileSync(out, text);
else process.stdout.write(text);
if (callsOut) writeFileSync(callsOut, rows.filter((r) => r.jsonl).map((r) => JSON.stringify(r.jsonl)).join("\n") + "\n");
