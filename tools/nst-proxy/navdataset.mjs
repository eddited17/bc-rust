// Decoder for the NavDataSet wire format ({"compressed","count","buffers"}), for reading
// recordings. Layout as in docs/nst-protocol-metadata.md section 3.3.
//
//   import { decodeDataSet } from "./navdataset.mjs";
//   decodeDataSet({ compressed: false, count: 1, buffers: ["..."] })
//   -> { name, lcid, tables: [{ name, lcid, columns: [{ name, type, isLazy, ext }], rows: [{ col: value }] }] }
//
// Cells: DBNull -> null, DBNotChanged -> {"$": "NotChanged"}, zero marker -> {"$": "Zero"},
// DBLazy -> {"$": "Lazy"}. DateTime -> {"ticks", "kind", "iso"}, Decimal -> string,
// Guid -> string, ByteArray -> {"base64", "length"}.
import zlib from "node:zlib";

export const TYPE_NAMES = [
  "Object", "DBNull", "Boolean", "Char", "SByte", "Byte", "Int16", "UInt16", "Int32", "UInt32", "Int64", "UInt64",
  "Single", "Double", "Decimal", "DateTime", "TimeSpan", "String", "Guid", "ByteArray", "Media", "MediaSet", "DBLazy", "DBNotChanged",
];

class Reader {
  constructor(buf) { this.b = buf; this.o = 0; }
  u8() { return this.b[this.o++]; }
  bool() { return this.u8() !== 0; }
  i16() { const v = this.b.readInt16LE(this.o); this.o += 2; return v; }
  u16() { const v = this.b.readUInt16LE(this.o); this.o += 2; return v; }
  i32() { const v = this.b.readInt32LE(this.o); this.o += 4; return v; }
  u32() { const v = this.b.readUInt32LE(this.o); this.o += 4; return v; }
  i64() { const v = this.b.readBigInt64LE(this.o); this.o += 8; return v; }
  u64() { const v = this.b.readBigUInt64LE(this.o); this.o += 8; return v; }
  f32() { const v = this.b.readFloatLE(this.o); this.o += 4; return v; }
  f64() { const v = this.b.readDoubleLE(this.o); this.o += 8; return v; }
  bytes(n) { const v = this.b.subarray(this.o, this.o + n); this.o += n; return v; }
  len7() { let n = 0, s = 0, c; do { c = this.u8(); n |= (c & 0x7f) << s; s += 7; } while (c & 0x80); return n; }
  str() { return this.bytes(this.len7()).toString("utf8"); }
}

const TICKS_EPOCH = 621355968000000000n; // 1970-01-01 in .NET ticks
function dateTime(ticks, kind) {
  const ms = Number((ticks - TICKS_EPOCH) / 10000n);
  let iso = null;
  try { iso = ticks === 0n ? "0001-01-01T00:00:00" : new Date(ms).toISOString().replace("Z", kind === 1 ? "Z" : ""); } catch {}
  return { ticks: ticks.toString(), kind: ["Unspecified", "Utc", "Local"][kind] ?? kind, iso };
}
function decimal(r) {
  const lo = BigInt(r.u32()), mid = BigInt(r.u32()), hi = BigInt(r.u32()), flags = r.u32();
  const scale = (flags >> 16) & 0xff, neg = (flags & 0x80000000) !== 0;
  let s = ((hi << 64n) | (mid << 32n) | lo).toString().padStart(scale + 1, "0");
  if (scale) s = `${s.slice(0, -scale)}.${s.slice(-scale)}`;
  return (neg ? "-" : "") + s;
}
function guid(b) {
  const h = (x) => Buffer.from(x).toString("hex");
  return `${h(Buffer.from(b.subarray(0, 4)).reverse())}-${h(Buffer.from(b.subarray(4, 6)).reverse())}-${h(Buffer.from(b.subarray(6, 8)).reverse())}-${h(b.subarray(8, 10))}-${h(b.subarray(10, 16))}`;
}

function payload(r, type) {
  switch (TYPE_NAMES[type]) {
    case "Boolean": return r.bool();
    case "Char": return String.fromCharCode(r.u8());
    case "SByte": return (r.u8() << 24) >> 24;
    case "Byte": return r.u8();
    case "Int16": return r.i16();
    case "UInt16": return r.u16();
    case "Int32": return r.i32();
    case "UInt32": return r.u32();
    case "Int64": return r.i64().toString();
    case "UInt64": return r.u64().toString();
    case "Single": return r.f32();
    case "Double": return r.f64();
    case "Decimal": return decimal(r);
    case "DateTime": { const t = r.i64(); return dateTime(t, r.i32()); }
    case "TimeSpan": return { ticks: r.i64().toString() };
    case "String": return r.str();
    case "Guid": return guid(r.bytes(16));
    case "ByteArray": { const n = r.i32(); const b = r.bytes(n); return { base64: b.toString("base64"), length: n }; }
    case "Media": case "MediaSet": { const wrapped = r.bool(); const n = r.i32(); return { wrapped, base64: r.bytes(n).toString("base64") }; }
    default: throw new Error(`cannot read a cell of type ${type} (${TYPE_NAMES[type]}) at offset ${r.o}`);
  }
}
function cell(r, type) {
  const m = r.u8();
  switch (m) {
    case 0: return null;
    case 1: return { $: "NotChanged" };
    case 2: return payload(r, type);
    case 3: return { $: "Zero" };
    case 4: return { $: "Lazy" };
    default: throw new Error(`unknown cell marker ${m} at offset ${r.o - 1}`);
  }
}
// Extended property value: a Byte cell with the type code, then a cell of that type.
function typed(r) {
  const t = cell(r, 5);
  return t === null ? null : { type: TYPE_NAMES[t] ?? t, value: cell(r, t) };
}

export function decodeDataSet(ds) {
  if (!ds || !Array.isArray(ds.buffers)) return null;
  let buf = Buffer.concat(ds.buffers.map((b) => Buffer.from(b, "base64")));
  const wireBytes = buf.length;
  if (ds.compressed) buf = zlib.inflateRawSync(buf);
  const r = new Reader(buf);
  const out = { name: r.str(), lcid: r.i32(), wireBytes, bytes: buf.length, tables: [] };
  const n = r.i32();
  for (let i = 0; i < n; i++) {
    const t = { name: r.str(), lcid: r.i32(), columns: [] };
    const nc = r.i32();
    for (let c = 0; c < nc; c++) {
      const col = { name: r.str(), type: r.u8(), isLazy: r.bool() };
      const ne = r.i32();
      if (ne) { col.ext = []; for (let e = 0; e < ne; e++) col.ext.push({ key: typed(r), value: typed(r) }); }
      col.typeName = TYPE_NAMES[col.type] ?? String(col.type);
      t.columns.push(col);
    }
    out.tables.push(t);
  }
  for (const t of out.tables) {
    const nr = r.i32();
    t.rows = [];
    for (let i = 0; i < nr; i++) {
      const row = {};
      for (const c of t.columns) row[c.name] = cell(r, c.type);
      t.rows.push(row);
    }
  }
  if (r.o !== buf.length) out.trailingBytes = buf.length - r.o;
  // compact column list for reading
  for (const t of out.tables) t.columns = t.columns.map((c) => `${c.name}:${c.typeName}${c.isLazy ? ":lazy" : ""}${c.ext ? `:ext${JSON.stringify(c.ext)}` : ""}`);
  return out;
}

// Replace every NavDataSet-shaped object inside a JSON value with
// {...original, "x-decoded": decodeDataSet(...)}; errors land in "x-decode-error".
export function annotateDataSets(v) {
  if (Array.isArray(v)) return v.map(annotateDataSets);
  if (v && typeof v === "object") {
    if (typeof v.compressed === "boolean" && Array.isArray(v.buffers) && "count" in v) {
      try { return { ...v, "x-decoded": decodeDataSet(v) }; } catch (e) { return { ...v, "x-decode-error": e.message }; }
    }
    const o = {};
    for (const [k, x] of Object.entries(v)) o[k] = annotateDataSets(x);
    return o;
  }
  return v;
}
