// Ledger oracle, bc-rust side: read the ledger tables from bc-rust's PostgreSQL database (schema c_<company>).
//   node tools/oracle/read-pg.mjs baseline --db <name> --out base.json   (highest Entry No. per table, before posting)
//   node tools/oracle/read-pg.mjs read --db <name> --base base.json --out rows.json   (the entries after the baseline)
// psql runs in the bc-rust-postgres container (env ORACLE_PG_CONTAINER, default bc-rust-postgres; user env
// ORACLE_PG_USER, default bc). Columns fN are renamed to Business Central's SQL names (tables.mjs), values
// are written as text in SQL Server's formats so compare.mjs sees one format.
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { TABLES, DIMSET_TABLE, fieldMaps, sqlName } from "./tables.mjs";

const argv = process.argv.slice(2);
const mode = argv[0];
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : d; };
const db = opt("--db"), out = opt("--out"), schema = opt("--schema", "c_cronus");
if (!["baseline", "read"].includes(mode) || !db || !out) { console.error("usage: read-pg.mjs baseline|read --db DB --out F [--base F] [--schema c_cronus]"); process.exit(2); }
if (/^(bc|bc_nst)$/.test(db)) { console.error("refusing a live database"); process.exit(2); }
const container = process.env.ORACLE_PG_CONTAINER || "bc-rust-postgres";
const user = process.env.ORACLE_PG_USER || "bc";

function psql(sql) {
  return execFileSync("docker", ["exec", "-i", container, "psql", "-U", user, "-d", db, "-At", "-v", "ON_ERROR_STOP=1"], { input: sql, maxBuffer: 1 << 30 }).toString();
}
const maps = fieldMaps();
const colTypes = {};
for (const line of psql(`select table_name, column_name, data_type from information_schema.columns where table_schema='${schema}';`).trim().split("\n")) {
  const [t, c, ty] = line.split("|");
  (colTypes[t] ||= {})[c] = ty;
}
const entryCol = (tid) => `f${Object.entries(maps[tid].fields).find(([, f]) => f.name === "Entry No.")[0]}`;
const txCol = `f${Object.entries(maps[17].fields).find(([, f]) => f.name === "Transaction No.")[0]}`;

if (mode === "baseline") {
  const base = { side: "bc-rust", db, takenAt: new Date().toISOString(), entryNo: {} };
  for (const tid of Object.keys(TABLES)) base.entryNo[tid] = Number(psql(`select coalesce(max(${entryCol(tid)}),0) from ${schema}.t${tid};`).trim());
  base.transactionNo = Number(psql(`select coalesce(max(${txCol}),0) from ${schema}.t17;`).trim());
  writeFileSync(out, JSON.stringify(base, null, 1));
  console.log(JSON.stringify(base));
  process.exit(0);
}

const base = JSON.parse(readFileSync(opt("--base"), "utf8"));
function expr(tid, col) {
  const ty = colTypes[`t${tid}`][col];
  if (ty === "boolean") return `case when ${col} then '1' else '0' end`;
  if (ty.startsWith("timestamp")) return `to_char(${col}, 'YYYY-MM-DD HH24:MI:SS.MS')`;
  if (ty === "bytea") return `'<bin>'`;
  return `${col}::text`;
}
const result = { side: "bc-rust", db, readAt: new Date().toISOString(), base, tables: {}, unnamedColumns: {}, dimsets: {} };
const dimIds = new Set();
for (const tid of Object.keys(TABLES)) {
  const cols = Object.keys(colTypes[`t${tid}`]).filter((c) => /^f\d+$/.test(c));
  const named = cols.filter((c) => maps[tid].fields[c.slice(1)]);
  result.unnamedColumns[tid] = cols.filter((c) => !maps[tid].fields[c.slice(1)]); // table extension fields (other apps)
  const sel = named.map((c) => `'${sqlName(maps[tid].fields[c.slice(1)].name).replace(/'/g, "''")}', ${expr(tid, c)}`);
  // json_build_object takes at most 100 arguments: build it in chunks and merge
  const chunks = [];
  for (let i = 0; i < sel.length; i += 50) chunks.push(`jsonb_build_object(${sel.slice(i, i + 50).join(", ")})`);
  const sql = `select coalesce(json_agg(r order by e), '[]') from (select ${entryCol(tid)} e, ${chunks.join(" || ")} r from ${schema}.t${tid} where ${entryCol(tid)} > ${base.entryNo[tid]}) x;`;
  const rows = JSON.parse(psql(sql));
  result.tables[tid] = { name: TABLES[tid], rows };
  for (const r of rows) if (r["Dimension Set ID"] && r["Dimension Set ID"] !== "0") dimIds.add(r["Dimension Set ID"]);
}
if (dimIds.size) {
  const f = maps[DIMSET_TABLE].fields;
  const c = (n) => `f${Object.entries(f).find(([, x]) => x.name === n)[0]}`;
  const rows = psql(`select ${c("Dimension Set ID")}, ${c("Dimension Code")}, ${c("Dimension Value Code")} from ${schema}.t${DIMSET_TABLE} where ${c("Dimension Set ID")} in (${[...dimIds].join(",")}) order by 1, 2;`).trim();
  for (const line of rows ? rows.split("\n") : []) {
    const [id, dim, val] = line.split("|");
    result.dimsets[id] = (result.dimsets[id] ? result.dimsets[id] + ";" : "") + `${dim}=${val}`;
  }
}
writeFileSync(out, JSON.stringify(result));
console.log(`wrote ${out}: ` + Object.entries(result.tables).map(([t, v]) => `${t}:${v.rows.length}`).join(" ") + `, ${Object.keys(result.dimsets).length} dimension sets`);
