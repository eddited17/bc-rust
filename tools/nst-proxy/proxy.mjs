// Logging proxy between Microsoft's BC web server and a BC service tier (NST).
//
// Forwards everything on the client services port, both plain HTTP calls under
// /{instance}/client/ and the JSON-RPC WebSocket at /{instance}/client/ws/connect,
// and writes one JSONL record per HTTP request/response and per WebSocket message.
//
// usage: node tools/nst-proxy/proxy.mjs <outDir>
//   env LISTEN    host:port to listen on        (default 127.0.0.1:7285)
//       UPSTREAM  host:port of the real NST     (default 127.0.0.1:7085)
//       CONTROL   host:port of the marker port  (default 127.0.0.1:7286; "off" disables it)
//       KEEP_HOST=1  forward the Host header unchanged (default: rewrite it to UPSTREAM)
//       REDACT_FILE  file with one secret per line; every occurrence is replaced in the log
//
// Writes to outDir:
//   nst.jsonl   one record per message (see README.md for the fields)
//   calls.jsonl one record per finished JSON-RPC call (request id, method, sizes, duration),
//               written when the response arrives; summarize.mjs sorts them into request order
//
// The proxy never changes a byte of the WebSocket stream: it pipes both directions and
// parses a copy of the frames (including permessage-deflate) for the log.
import http from "node:http";
import net from "node:net";
import zlib from "node:zlib";
import { mkdirSync, createWriteStream, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const outDir = process.argv[2];
if (!outDir) {
  console.error("usage: proxy.mjs <outDir>");
  process.exit(2);
}
const hostPort = (s, def) => {
  const [h, p] = (s || def).split(":");
  return { host: h, port: Number(p) };
};
const LISTEN = hostPort(process.env.LISTEN, "127.0.0.1:7285");
const UP = hostPort(process.env.UPSTREAM, "127.0.0.1:7085");
const CONTROL = process.env.CONTROL === "off" ? null : hostPort(process.env.CONTROL, "127.0.0.1:7286");
const KEEP_HOST = process.env.KEEP_HOST === "1";
const SECRETS = (process.env.REDACT_FILE && existsSync(process.env.REDACT_FILE)
  ? readFileSync(process.env.REDACT_FILE, "utf8").split("\n").map((s) => s.trim()).filter((s) => s.length >= 4)
  : []);

mkdirSync(outDir, { recursive: true });
const log = createWriteStream(join(outDir, "nst.jsonl"), { flags: "a" });
const callsLog = createWriteStream(join(outDir, "calls.jsonl"), { flags: "a" });
const t0 = Date.now();
let seq = 0;
let connSeq = 0;

// ---------------------------------------------------------------------------
// Redaction

const SECRET_KEY = /pass(word|wd)?$|secret|^credentials?$/i;
function redactAuth(v) {
  const m = /^(\w+)\s+(.*)$/.exec(String(v));
  if (!m) return "<redacted>";
  if (m[1].toLowerCase() === "basic") {
    try {
      const user = Buffer.from(m[2], "base64").toString("latin1").split(":")[0];
      return `Basic <redacted; user=${user}>`;
    } catch {}
  }
  return `${m[1]} <redacted>`;
}
function redactCookie(v) {
  // keep the cookie names, drop the values
  return String(v).replace(/=([^;,]*)/g, (s, val) => (val ? "=<redacted>" : s));
}
// Raw header array -> ordered object; duplicates become arrays. Secrets redacted.
function headerObj(raw) {
  const o = {};
  for (let i = 0; i < raw.length; i += 2) {
    let [k, v] = [raw[i], raw[i + 1]];
    const lk = k.toLowerCase();
    if (lk === "authorization" || lk === "proxy-authorization") v = redactAuth(v);
    else if (lk === "cookie" || lk === "set-cookie") v = redactCookie(v);
    if (k in o) o[k] = [].concat(o[k], v);
    else o[k] = v;
  }
  return o;
}
// Replace values of password-like keys inside parsed JSON bodies.
function redactJson(v, key = "") {
  if (Array.isArray(v)) return v.map((x) => redactJson(x));
  if (v && typeof v === "object") {
    const o = {};
    for (const [k, x] of Object.entries(v)) {
      if (/pass(word|wd)?$/i.test(k) && x !== null && typeof x !== "object") o[k] = "<redacted>";
      else o[k] = redactJson(x, k);
    }
    return o;
  }
  return v;
}
function scrub(line) {
  for (const s of SECRETS) if (line.includes(s)) line = line.split(s).join("<redacted>");
  return line;
}

function write(rec) {
  rec = { seq: ++seq, ts: new Date().toISOString(), t: Date.now() - t0, ...rec };
  log.write(scrub(JSON.stringify(rec)) + "\n");
  return rec.seq;
}

// ---------------------------------------------------------------------------
// Body decoding: content-encoding / gzip magic, then JSON, text or base64.

function inflateAny(buf, encoding) {
  const enc = (encoding || "").toLowerCase().trim();
  const steps = [];
  try {
    if (enc === "gzip" || enc === "x-gzip") return { buf: zlib.gunzipSync(buf), steps: ["gzip"] };
    if (enc === "deflate") {
      try { return { buf: zlib.inflateSync(buf), steps: ["deflate"] }; } catch { return { buf: zlib.inflateRawSync(buf), steps: ["deflate-raw"] }; }
    }
    if (enc === "br") return { buf: zlib.brotliDecompressSync(buf), steps: ["br"] };
  } catch (e) {
    return { buf, steps: [`${enc}-failed:${e.code || e.message}`] };
  }
  // no (known) content-encoding: sniff gzip
  if (buf.length > 2 && buf[0] === 0x1f && buf[1] === 0x8b) {
    try { return { buf: zlib.gunzipSync(buf), steps: ["gzip(sniffed)"] }; } catch {}
  }
  return { buf, steps };
}

function isText(buf) {
  if (buf.length === 0) return true;
  const s = buf.toString("utf8");
  if (s.includes("�")) return false;
  let ctl = 0;
  for (let i = 0; i < Math.min(s.length, 4096); i++) {
    const c = s.charCodeAt(i);
    if (c < 9 || (c > 13 && c < 32)) ctl++;
  }
  return ctl === 0;
}

function decodeBody(buf, contentEncoding) {
  if (!buf || buf.length === 0) return { bodyEncoding: "none", wireBytes: 0 };
  const { buf: plain, steps } = inflateAny(buf, contentEncoding);
  const out = { wireBytes: buf.length };
  if (steps.length) out.decoded = steps.join("+");
  if (plain !== buf) out.bodyBytes = plain.length;
  let b = plain;
  if (b.length >= 3 && b[0] === 0xef && b[1] === 0xbb && b[2] === 0xbf) {
    out.bom = true;
    b = b.subarray(3);
  }
  if (isText(b)) {
    const s = b.toString("utf8");
    try {
      out.body = redactJson(JSON.parse(s));
      out.bodyEncoding = "json";
      return out;
    } catch {}
    out.body = s;
    out.bodyEncoding = "text";
    return out;
  }
  out.body = plain.toString("base64");
  out.bodyEncoding = "base64";
  return out;
}

// ---------------------------------------------------------------------------
// HTTP

const agent = new http.Agent({ keepAlive: true, maxSockets: 64 });

function upstreamHeaders(raw) {
  const out = [];
  for (let i = 0; i < raw.length; i += 2) {
    if (!KEEP_HOST && raw[i].toLowerCase() === "host") out.push(raw[i], `${UP.host}:${UP.port}`);
    else out.push(raw[i], raw[i + 1]);
  }
  return out;
}

const server = http.createServer((req, res) => {
  const conn = `h${++connSeq}`;
  const start = Date.now();
  res.sendDate = false; // pass the upstream headers through without additions
  const reqChunks = [];
  const preq = http.request({
    host: UP.host, port: UP.port, method: req.method, path: req.url, agent,
    headers: upstreamHeaders(req.rawHeaders),
  });
  preq.on("socket", (s) => s.setNoDelay(true));
  req.socket.setNoDelay(true);
  req.on("data", (c) => { reqChunks.push(c); preq.write(c); });
  req.on("end", () => {
    preq.end();
    const body = Buffer.concat(reqChunks);
    write({
      conn, channel: "http", dir: "to-nst", kind: "request", method: req.method, path: req.url,
      httpVersion: req.httpVersion, headers: headerObj(req.rawHeaders),
      ...decodeBody(body, req.headers["content-encoding"]),
    });
  });
  preq.on("response", (pres) => {
    const hdr = [];
    for (let i = 0; i < pres.rawHeaders.length; i += 2) {
      // Node re-frames the body itself; keep everything else as is
      hdr.push(pres.rawHeaders[i], pres.rawHeaders[i + 1]);
    }
    res.writeHead(pres.statusCode, pres.statusMessage, hdr);
    const chunks = [];
    pres.on("data", (c) => { chunks.push(c); res.write(c); });
    pres.on("end", () => {
      res.end();
      const body = Buffer.concat(chunks);
      write({
        conn, channel: "http", dir: "from-nst", kind: "response", method: req.method, path: req.url,
        status: pres.statusCode, statusMessage: pres.statusMessage, headers: headerObj(pres.rawHeaders),
        ms: Date.now() - start, ...decodeBody(body, pres.headers["content-encoding"]),
      });
      callsLog.write(scrub(JSON.stringify({
        channel: "http", conn, tReq: start - t0, method: req.method, path: req.url, status: pres.statusCode,
        reqBytes: reqChunks.reduce((n, c) => n + c.length, 0), respBytes: body.length, ms: Date.now() - start,
      })) + "\n");
    });
    pres.on("error", (e) => { write({ conn, channel: "http", kind: "error", where: "upstream-response", error: String(e) }); res.destroy(e); });
  });
  preq.on("error", (e) => {
    write({ conn, channel: "http", kind: "error", where: "upstream-request", method: req.method, path: req.url, error: String(e) });
    if (!res.headersSent) res.writeHead(502, { "content-type": "text/plain" });
    res.end(`nst-proxy: upstream error: ${e.message}`);
  });
});

// ---------------------------------------------------------------------------
// WebSocket: raw pipe both ways, plus a frame parser per direction for the log.

const TAIL = Buffer.from([0x00, 0x00, 0xff, 0xff]);

class Inflater {
  constructor(noContextTakeover) {
    this.reset = noContextTakeover;
    this.inf = null;
    this.chain = Promise.resolve();
  }
  // Serialized: messages must be inflated in order when the context is kept.
  inflate(data) {
    const p = this.chain.then(() => new Promise((resolve, reject) => {
      const inf = this.inf || (this.inf = zlib.createInflateRaw({ windowBits: 15 }));
      const chunks = [];
      const onData = (c) => chunks.push(c);
      const onErr = (e) => { inf.removeListener("data", onData); this.inf = null; reject(e); };
      inf.on("data", onData);
      inf.once("error", onErr);
      inf.write(Buffer.concat([data, TAIL]));
      inf.flush(zlib.constants.Z_SYNC_FLUSH, () => {
        inf.removeListener("data", onData);
        inf.removeListener("error", onErr);
        if (this.reset) { inf.close(); this.inf = null; }
        resolve(Buffer.concat(chunks));
      });
    }));
    this.chain = p.catch(() => {});
    return p;
  }
}

const OPS = { 0: "continuation", 1: "text", 2: "binary", 8: "close", 9: "ping", 10: "pong" };

class FrameParser {
  constructor(onMessage) {
    this.buf = Buffer.alloc(0);
    this.onMessage = onMessage;
    this.frag = null; // { opcode, compressed, parts: [], frames, wire, at }
  }
  push(chunk) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk;
    for (;;) {
      const b = this.buf;
      if (b.length < 2) return;
      const fin = (b[0] & 0x80) !== 0, rsv1 = (b[0] & 0x40) !== 0, opcode = b[0] & 0x0f;
      const masked = (b[1] & 0x80) !== 0;
      let len = b[1] & 0x7f, off = 2;
      if (len === 126) { if (b.length < 4) return; len = b.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (b.length < 10) return; len = Number(b.readBigUInt64BE(2)); off = 10; }
      const maskOff = off;
      if (masked) off += 4;
      if (b.length < off + len) return;
      let payload = Buffer.from(b.subarray(off, off + len));
      if (masked) for (let i = 0; i < payload.length; i++) payload[i] ^= b[maskOff + (i & 3)];
      const wire = off + len;
      this.buf = b.subarray(wire);
      const at = Date.now();
      if (opcode >= 8) {
        this.onMessage({ opcode, compressed: false, payload, frames: 1, wireBytes: wire, at, masked });
        continue;
      }
      if (opcode !== 0) this.frag = { opcode, compressed: rsv1, parts: [], frames: 0, wire: 0, at, masked };
      if (!this.frag) continue; // stray continuation
      this.frag.parts.push(payload);
      this.frag.frames++;
      this.frag.wire += wire;
      if (fin) {
        const f = this.frag;
        this.frag = null;
        this.onMessage({ opcode: f.opcode, compressed: f.compressed, payload: Buffer.concat(f.parts), frames: f.frames, wireBytes: f.wire, at: f.at, masked: f.masked });
      }
    }
  }
}

// JSON-RPC bookkeeping per socket: id -> request info, for calls.jsonl
function rpcInfo(msg) {
  if (!msg || typeof msg !== "object" || Array.isArray(msg)) return null;
  if (typeof msg.method === "string") return { kind: "id" in msg ? "request" : "notification", id: msg.id, method: msg.method };
  if ("result" in msg) return { kind: "result", id: msg.id };
  if ("error" in msg) return { kind: "error", id: msg.id };
  return null;
}

function parseExtensions(h) {
  const out = { deflate: false, clientNoCtx: false, serverNoCtx: false };
  if (!h) return out;
  for (const ext of String(h).split(",")) {
    const parts = ext.split(";").map((s) => s.trim().toLowerCase());
    if (parts[0] !== "permessage-deflate") continue;
    out.deflate = true;
    out.clientNoCtx = parts.includes("client_no_context_takeover");
    out.serverNoCtx = parts.includes("server_no_context_takeover");
  }
  return out;
}

server.on("upgrade", (req, csock, head) => {
  const conn = `ws${++connSeq}`;
  const start = Date.now();
  write({
    conn, channel: "ws", dir: "to-nst", kind: "upgrade", method: req.method, path: req.url,
    httpVersion: req.httpVersion, headers: headerObj(req.rawHeaders),
  });
  // No Nagle on either side: both peers send large messages as several frames, and with Nagle the
  // proxy would hold each later frame until the previous one is ACKed (~40 ms with delayed ACKs).
  csock.setNoDelay(true);
  const usock = net.connect(UP.port, UP.host);
  usock.setNoDelay(true);
  const lines = [`${req.method} ${req.url} HTTP/${req.httpVersion}`];
  const uh = upstreamHeaders(req.rawHeaders);
  for (let i = 0; i < uh.length; i += 2) lines.push(`${uh[i]}: ${uh[i + 1]}`);
  usock.write(lines.join("\r\n") + "\r\n\r\n");
  if (head && head.length) usock.write(head);

  // "<direction of the request>:<id>" -> request info. Each side numbers its own requests (the NST's
  // callbacks start again at 1), so the same id can be in flight in both directions.
  const pending = new Map();
  let c2s = null, s2c = null; // frame parsers, created after the 101
  let upgraded = false;
  let respHead = Buffer.alloc(0);
  let nonUpgradeBody = [];
  let nonUpgradeRec = null;

  const handle = (dir, inflater) => async (m) => {
    const base = { conn, channel: "ws", dir, opcode: OPS[m.opcode] || m.opcode, frames: m.frames, wireBytes: m.wireBytes };
    if (m.opcode === 8) {
      const code = m.payload.length >= 2 ? m.payload.readUInt16BE(0) : null;
      write({ ...base, kind: "close", code, reason: m.payload.subarray(2).toString("utf8") });
      return;
    }
    if (m.opcode === 9 || m.opcode === 10) {
      write({ ...base, kind: m.opcode === 9 ? "ping" : "pong", body: m.payload.toString("base64") });
      return;
    }
    let data = m.payload;
    if (m.compressed) {
      try { data = await inflater.inflate(data); base.compressed = "permessage-deflate"; }
      catch (e) { write({ ...base, kind: "message", error: `inflate failed: ${e.message}`, body: m.payload.toString("base64"), bodyEncoding: "base64" }); return; }
    }
    const rec = { ...base, kind: "message", bodyBytes: data.length };
    if (m.opcode === 1 || isText(data)) {
      const s = data.toString("utf8");
      try { rec.body = redactJson(JSON.parse(s)); rec.bodyEncoding = "json"; }
      catch { rec.body = s; rec.bodyEncoding = "text"; }
    } else {
      rec.body = data.toString("base64");
      rec.bodyEncoding = "base64";
    }
    const info = rec.bodyEncoding === "json" ? rpcInfo(rec.body) : null;
    if (info) rec.rpc = info;
    // time of arrival of the first frame, not of the (async) inflate
    const n = write({ ...rec, tFrame: m.at - t0 });
    if (!info) return;
    const reqDir = info.kind === "request" || info.kind === "notification" ? dir : dir === "to-nst" ? "from-nst" : "to-nst";
    const key = `${reqDir}:${JSON.stringify(info.id)}`;
    if (info.kind === "request") {
      pending.set(key, { seq: n, tReq: m.at, method: info.method, dir, paramsBytes: JSON.stringify(rec.body.params ?? null).length, reqBytes: data.length, reqWire: m.wireBytes });
    } else if (info.kind === "notification") {
      callsLog.write(scrub(JSON.stringify({ channel: "ws", conn, seq: n, tReq: m.at - t0, dir, kind: "notification", method: info.method, paramsBytes: JSON.stringify(rec.body.params ?? null).length })) + "\n");
    } else {
      const p = pending.get(key);
      pending.delete(key);
      const err = info.kind === "error" ? rec.body.error : null;
      callsLog.write(scrub(JSON.stringify({
        channel: "ws", conn, seq: p?.seq ?? null, respSeq: n, id: info.id, method: p?.method ?? null,
        caller: p ? (p.dir === "to-nst" ? "web" : "nst") : null,
        tReq: p ? p.tReq - t0 : null, paramsBytes: p?.paramsBytes ?? null, reqBytes: p?.reqBytes ?? null,
        respBytes: data.length, respWire: m.wireBytes, ms: p ? m.at - p.tReq : null,
        outcome: info.kind, ...(err ? { errorCode: err.code, errorMessage: String(err.message ?? "").slice(0, 300) } : {}),
      })) + "\n");
    }
  };

  usock.on("data", (chunk) => {
    csock.write(chunk);
    if (upgraded) { s2c.push(chunk); return; }
    if (nonUpgradeRec) { nonUpgradeBody.push(chunk); return; }
    respHead = Buffer.concat([respHead, chunk]);
    const end = respHead.indexOf("\r\n\r\n");
    if (end < 0) return;
    const headText = respHead.subarray(0, end).toString("latin1").split("\r\n");
    const rest = respHead.subarray(end + 4);
    const [, status, ...msg] = headText[0].split(" ");
    const raw = [];
    for (const l of headText.slice(1)) {
      const i = l.indexOf(":");
      raw.push(l.slice(0, i), l.slice(i + 1).trim());
    }
    const rec = { conn, channel: "ws", dir: "from-nst", kind: "upgrade-response", path: req.url, status: Number(status), statusMessage: msg.join(" "), headers: headerObj(raw), ms: Date.now() - start };
    if (Number(status) === 101) {
      upgraded = true;
      const ext = parseExtensions(raw.filter((_, i) => i % 2 === 1 && raw[i - 1].toLowerCase() === "sec-websocket-extensions").join(","));
      rec.permessageDeflate = ext.deflate;
      write(rec);
      // client = web server (frames masked), server = NST
      c2s = new FrameParser(handle("to-nst", new Inflater(ext.clientNoCtx)));
      s2c = new FrameParser(handle("from-nst", new Inflater(ext.serverNoCtx)));
      if (pendingC2S.length) { for (const c of pendingC2S) c2s.push(c); pendingC2S.length = 0; }
      if (rest.length) s2c.push(rest);
    } else {
      nonUpgradeRec = rec;
      if (rest.length) nonUpgradeBody.push(rest);
    }
  });
  const pendingC2S = [];
  csock.on("data", (chunk) => {
    usock.write(chunk);
    if (c2s) c2s.push(chunk);
    else pendingC2S.push(chunk);
  });
  const finish = (who) => () => {
    if (nonUpgradeRec) {
      const body = Buffer.concat(nonUpgradeBody);
      write({ ...nonUpgradeRec, ...decodeBody(body, nonUpgradeRec.headers["Content-Encoding"] || nonUpgradeRec.headers["content-encoding"]) });
      nonUpgradeRec = null;
    }
    write({ conn, channel: "ws", kind: "socket-end", by: who, ms: Date.now() - start, unanswered: [...pending.values()].map((p) => ({ seq: p.seq, method: p.method })) });
    csock.end();
    usock.end();
  };
  let ended = false;
  const once = (fn) => () => { if (!ended) { ended = true; fn(); } };
  usock.on("end", once(finish("nst")));
  csock.on("end", once(finish("web")));
  usock.on("error", (e) => { write({ conn, channel: "ws", kind: "error", where: "upstream", error: String(e) }); csock.destroy(); });
  csock.on("error", (e) => { write({ conn, channel: "ws", kind: "error", where: "client", error: String(e) }); usock.destroy(); });
});

server.on("clientError", (e, sock) => {
  write({ channel: "http", kind: "error", where: "client", error: String(e) });
  sock.destroy();
});
server.keepAliveTimeout = 120000;
server.headersTimeout = 125000;
server.requestTimeout = 0;
server.listen(LISTEN.port, LISTEN.host, () => {
  console.log(`nst-proxy: ${LISTEN.host}:${LISTEN.port} -> ${UP.host}:${UP.port}, log ${join(outDir, "nst.jsonl")}`);
  write({ channel: "proxy", kind: "start", listen: `${LISTEN.host}:${LISTEN.port}`, upstream: `${UP.host}:${UP.port}`, keepHost: KEEP_HOST });
});

// Marker port: POST /mark with a JSON body ({"label": ...}) adds a "marker" record, so a
// browser driver can put its steps into the same sequence.
if (CONTROL) {
  http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      let body = Buffer.concat(chunks).toString("utf8");
      try { body = JSON.parse(body); } catch {}
      const n = write({ channel: "marker", kind: "marker", ...(typeof body === "object" ? body : { label: body }) });
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ seq: n }));
    });
  }).listen(CONTROL.port, CONTROL.host);
}

const stop = () => {
  write({ channel: "proxy", kind: "stop" });
  log.end(() => callsLog.end(() => process.exit(0)));
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
