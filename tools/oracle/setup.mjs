// Ledger oracle: the setup the documents depend on, read on both systems and compared row by row, so the report can
// say how the two starting states differ. Tables, filters and keys use Business Central's SQL names.
//   node tools/oracle/setup.mjs pg --db <name> --out bcrust-setup.json     (bc-rust, read-only)
//   node tools/oracle/setup.mjs compare --a bcrust-setup.json --b bc-setup.json --out-md setup.md
// The BC side comes from read-sql.ps1 mode 'setup' (vm-read.sh setup OUT.json), which reads the same SETUP list
// (passed to it as JSON).
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fieldMaps, sqlName } from "./tables.mjs";

export const SETUP = [
  { id: 98, key: ["Primary Key"] }, { id: 311, key: ["Primary Key"] }, { id: 312, key: ["Primary Key"] }, { id: 313, key: ["Primary Key"] },
  { id: 242, key: ["Primary Key"] },
  { id: 325, key: ["VAT Bus_ Posting Group", "VAT Prod_ Posting Group"] },
  { id: 252, key: ["Gen_ Bus_ Posting Group", "Gen_ Prod_ Posting Group"] },
  { id: 92, key: ["Code"] }, { id: 93, key: ["Code"] },
  { id: 5813, key: ["Location Code", "Invt_ Posting Group Code"] },
  { id: 4, key: ["Code"], where: ["Code", ["EUR", "USD"]] },
  { id: 330, key: ["Currency Code", "Starting Date"], where: ["Currency Code", ["EUR", "USD"]] },
  { id: 3, key: ["Code"] },
  { id: 15, key: ["No_"] },
  { id: 18, key: ["No_"], where: ["No_", ["10000", "20000", "30000", "40000", "50000"]] },
  { id: 23, key: ["No_"], where: ["No_", ["10000", "20000", "30000", "40000", "50000"]] },
  { id: 27, key: ["No_"], where: ["No_", ["1896-S", "1900-S", "1906-S", "1908-S", "1920-S", "1928-S", "1936-S", "1960-S", "1964-S", "1968-S", "2000-S", "SER101", "SER203"]] },
  { id: 80, key: ["Name"] }, { id: 232, key: ["Journal Template Name", "Name"] },
  { id: 91, key: ["User ID"] },
  { id: 50, key: ["Starting Date"] },
];
// ids are SystemId references (differ per database), pictures are blobs, the rest modification stamps
const IGNORE = /^(\$|timestamp$|Last Date Modified$|Last Modified Date Time$|Last Modified DateTime$|Last DateTime Modified$|Last Time Modified$|SystemId$|Picture$|Image$)|(^| )(Id|ID)$|[a-z]Id$/;
// BC stores DateFormula with control characters for the units (1M -> "1\u0005"); compare in AL notation
const DF = { "\u0001": "C", "\u0002": "D", "\u0003": "WD", "\u0004": "W", "\u0005": "M", "\u0006": "Q", "\u0007": "Y" };

const argv = process.argv.slice(2);
const mode = argv[0];
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : d; };

if (mode === "spec") {
  // for vm-read.sh: the table names and filters, compact
  const maps = fieldMaps(SETUP.map((s) => s.id));
  process.stdout.write(JSON.stringify(SETUP.map((s) => ({ id: String(s.id), name: maps[s.id].name, wc: s.where ? s.where[0] : "", wv: s.where ? s.where[1] : [] }))));
} else if (mode === "pg") {
  const db = opt("--db"), schema = opt("--schema", "c_cronus");
  if (!db || /^(bc|bc_nst)$/.test(db)) { console.error("usage: setup.mjs pg --db <scratch db> --out F"); process.exit(2); }
  const psql = (sql) => execFileSync("docker", ["exec", "-i", process.env.ORACLE_PG_CONTAINER || "bc-rust-postgres", "psql", "-U", process.env.ORACLE_PG_USER || "bc", "-d", db, "-At", "-v", "ON_ERROR_STOP=1"], { input: sql, maxBuffer: 1 << 30 }).toString();
  const maps = fieldMaps(SETUP.map((s) => s.id));
  const types = {};
  for (const line of psql(`select table_name, column_name, data_type from information_schema.columns where table_schema='${schema}';`).trim().split("\n")) {
    const [t, c, ty] = line.split("|"); (types[t] ||= {})[c] = ty;
  }
  const out = { side: "bc-rust", db, tables: {} };
  for (const s of SETUP) {
    const fm = maps[s.id].fields, ty = types[`t${s.id}`] || {};
    const cols = Object.keys(ty).filter((c) => /^f\d+$/.test(c) && fm[c.slice(1)]);
    const ex = (c) => ty[c] === "boolean" ? `case when ${c} then '1' else '0' end` : ty[c].startsWith("timestamp") ? `to_char(${c}, 'YYYY-MM-DD HH24:MI:SS.MS')` : ty[c] === "bytea" ? `'<bin>'` : `${c}::text`;
    const sel = cols.map((c) => `'${sqlName(fm[c.slice(1)].name).replace(/'/g, "''")}', ${ex(c)}`);
    const chunks = []; for (let i = 0; i < sel.length; i += 50) chunks.push(`jsonb_build_object(${sel.slice(i, i + 50).join(", ")})`);
    let where = "true";
    if (s.where) {
      const col = cols.find((c) => sqlName(fm[c.slice(1)].name) === s.where[0]);
      where = `${col} in (${s.where[1].map((v) => `'${v}'`).join(",")})`;
    }
    out.tables[s.id] = { name: maps[s.id].name, rows: JSON.parse(psql(`select coalesce(json_agg(r), '[]') from (select ${chunks.join(" || ")} r from ${schema}.t${s.id} where ${where}) x;`)) };
  }
  writeFileSync(opt("--out"), JSON.stringify(out));
  console.log("wrote", opt("--out"), Object.entries(out.tables).map(([k, v]) => `${k}:${v.rows.length}`).join(" "));
} else if (mode === "compare") {
  const a = JSON.parse(readFileSync(opt("--a"), "utf8")), b = JSON.parse(readFileSync(opt("--b"), "utf8"));
  const md = ["# Setup differences (starting state)", "", `A = ${opt("--a-name", "bc-rust")}, B = ${opt("--b-name", "BC 29.1")}. Decimals compared without trailing zeros; ids, pictures and modification stamps ignored.`, ""];
  const nd = (v) => {
    if (typeof v !== "string") return v;
    if (/^-?\d+\.\d+$/.test(v)) return v.replace(/0+$/, "").replace(/\.$/, "");
    return /[\u0001-\u0007]/.test(v) ? v.replace(/[\u0001-\u0007]/g, (c) => DF[c]) : v;
  };
  const esc = (s) => String(s ?? "").replace(/\|/g, "\\|").slice(0, 70);
  const rows = [];
  let onlyCols = [];
  for (const s of SETUP) {
    const ta = a.tables[s.id]?.rows || [], tb = b.tables[s.id]?.rows || [];
    const k = (r) => s.key.map((c) => nd(r[c])).join(" / ");
    const mb = new Map(tb.map((r) => [k(r), r])), ma = new Map(ta.map((r) => [k(r), r]));
    for (const [key, ra] of ma) {
      const rb = mb.get(key);
      if (!rb) { rows.push([s.id, key, "(row)", "present", "missing"]); continue; }
      for (const c of Object.keys(ra)) {
        if (IGNORE.test(c)) continue;
        if (!(c in rb)) { onlyCols.push(`${s.id}.${c}`); continue; }
        if (nd(ra[c]) !== nd(rb[c])) rows.push([s.id, key, c, ra[c], rb[c]]);
      }
    }
    for (const key of mb.keys()) if (!ma.has(key)) rows.push([s.id, key, "(row)", "missing", "present"]);
  }
  md.push(`| table | key | field | A | B |`, "|---|---|---|---|---|");
  for (const r of rows) md.push(`| ${r[0]} ${a.tables[r[0]]?.name ?? ""} | ${esc(r[1])} | ${esc(r[2])} | ${esc(r[3])} | ${esc(r[4])} |`);
  md.push("", `Columns only in A (not compared): ${[...new Set(onlyCols)].join(", ") || "-"}`);
  writeFileSync(opt("--out-md"), md.join("\n") + "\n");
  console.log(`${rows.length} setup differences -> ${opt("--out-md")}`);
} else {
  console.error("usage: setup.mjs pg|compare ..."); process.exit(2);
}
