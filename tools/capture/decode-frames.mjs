// Decode a frames.jsonl written by capture.mjs: parse the /csh JSON-RPC frames and
// gunzip their compressed payloads (compressedResult on responses, compressedData on
// server-pushed "Message" notifications).
//
// usage: node tools/capture/decode-frames.mjs <frames.jsonl> timeline [stepLabel]
//        node tools/capture/decode-frames.mjs <frames.jsonl> dump <index> [<index> ...]
//   timeline: one line per frame (index, ms since start, direction, size, method,
//             interactions sent, handlers received), grouped by capture step
//   dump:     the given frames (indexes from the timeline), decoded, as a JSON array
import { readFileSync } from "node:fs";
import { gunzipSync } from "node:zlib";

export function loadFrames(file) {
  const out = [];
  let step = null;
  const unzip = (b64) => JSON.parse(gunzipSync(Buffer.from(b64, "base64")).toString("utf8"));
  for (const line of readFileSync(file, "utf8").split("\n")) {
    if (!line) continue;
    const f = JSON.parse(line);
    if (f.dir === "step" && f.payload.phase === "start") step = f.payload.label;
    f.step = step;
    if (f.dir === "send" || f.dir === "recv") {
      f.wireBytes = f.payload.length;
      try {
        const msg = JSON.parse(f.payload);
        if (msg.compressedResult) {
          msg.result = unzip(msg.compressedResult);
          msg.compressedResult = `<base64 gzip of "result", ${msg.compressedResult.length} chars>`;
        }
        for (const p of msg.params || []) {
          if (p && p.compressedData) {
            p.data = unzip(p.compressedData);
            p.compressedData = `<base64 gzip of "data", ${p.compressedData.length} chars>`;
          }
        }
        f.msg = msg;
      } catch {
        // not JSON: keep the raw payload
      }
    }
    out.push(f);
  }
  return out;
}

function timelineLine(f, i) {
  if (f.dir === "step") return `== ${f.t} ms  step "${f.payload.label}" ${f.payload.phase}`;
  if (f.dir === "http") return `      ${f.t} http ${f.payload.method} ${f.payload.url.replace(/^https?:\/\/[^/]+/, "").slice(0, 100)}`;
  if (f.dir === "ws-open") return `      ${f.t} ws-open ${String(f.payload).split("?")[0]}`;
  if (!f.msg) return null;
  const m = f.msg;
  const sent = (m.params || []).flatMap((p) =>
    (p?.interactionsToInvoke || []).map((x) => `${x.interactionName}${x.formId ? `@${x.formId}` : ""} ${x.namedParameters || ""}`.slice(0, 100)));
  const got = [m.result, ...(m.params || []).map((p) => p?.data)]
    .filter(Boolean)
    .flatMap((r) => (Array.isArray(r) ? r : [r]).map((h) => (h.handlerType || "?").replace(/^DN\./, "")));
  const what = m.error ? `ERROR ${m.error.errorType}: ${String(m.error.message).split("\n")[0]}` : [...sent, ...got].join(", ");
  return `#${i} ${f.t} ${f.dir} ${f.wireBytes}B ${m.method || "result"} ${what}`.slice(0, 260);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [file, mode = "timeline", ...rest] = process.argv.slice(2);
  if (!file) {
    console.error("usage: decode-frames.mjs <frames.jsonl> timeline [stepLabel] | dump <index> ...");
    process.exit(2);
  }
  const frames = loadFrames(file);
  if (mode === "timeline") {
    const only = rest.join(" ");
    frames.forEach((f, i) => {
      if (only && f.step !== only) return;
      const line = timelineLine(f, i);
      if (line) console.log(line);
    });
  } else if (mode === "dump") {
    const picked = rest.map(Number).map((i) => {
      const f = frames[i];
      return { index: i, t: f.t, step: f.step, dir: f.dir, wireBytes: f.wireBytes, frame: f.msg ?? f.payload };
    });
    console.log(JSON.stringify(picked, null, 2));
  } else {
    console.error(`unknown mode ${mode}`);
    process.exit(2);
  }
}
