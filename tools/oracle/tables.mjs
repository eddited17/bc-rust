// Ledger oracle: the tables it compares and their field names, read from the W1 Base App AL sources
// (bc-rust stores field N as column fN; Business Central's SQL Server columns are the field names with
// the characters . / \ ' " % [ ] replaced by _).
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

// the repo root (for vendor/bcapps): two levels above this file, or env ORACLE_REPO
export const REPO = process.env.ORACLE_REPO || join(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const BASE_APP_ID = "437dbf0e-84ff-417a-965d-ed2bb9650972";

// table id -> name; all are keyed by "Entry No."
export const TABLES = {
  17: "G/L Entry",
  254: "VAT Entry",
  21: "Cust. Ledger Entry",
  379: "Detailed Cust. Ledg. Entry",
  25: "Vendor Ledger Entry",
  380: "Detailed Vendor Ledg. Entry",
  32: "Item Ledger Entry",
  5802: "Value Entry",
};
export const DIMSET_TABLE = 480; // Dimension Set Entry

export const sqlName = (s) => s.replace(/[./\\'"%\[\]]/g, "_");

function* alFiles(dir) {
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    const st = statSync(p);
    if (st.isDirectory()) yield* alFiles(p);
    else if (e.endsWith(".Table.al")) yield p;
  }
}

// { tableId: { name, fields: { no: { name, type } } } } for the wanted table ids
export function fieldMaps(ids = [...Object.keys(TABLES).map(Number), DIMSET_TABLE]) {
  const want = new Set(ids.map(Number));
  const root = join(REPO, "vendor", "bcapps", "src", "Layers", "W1", "BaseApp");
  const out = {};
  for (const f of alFiles(root)) {
    const src = readFileSync(f, "utf8").replace(/^﻿/, "");
    const m = src.match(/^table (\d+) ("[^"]+"|\S+)/m);
    if (!m || !want.has(Number(m[1]))) continue;
    const fields = {};
    for (const fm of src.matchAll(/^\s*field\((\d+);\s*("[^"]+"|[^;]+);\s*([^)]+?)\)\s*$/gm)) {
      fields[Number(fm[1])] = { name: fm[2].replace(/^"|"$/g, ""), type: fm[3].trim() };
    }
    out[Number(m[1])] = { name: m[2].replace(/^"|"$/g, ""), fields };
  }
  for (const id of want) if (!out[id]) throw new Error(`table ${id} not found under ${root}`);
  return out;
}
