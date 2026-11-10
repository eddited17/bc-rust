// Ledger oracle, step 4: compare the new ledger entries of two systems field by field.
//   node tools/oracle/compare.mjs --a bcrust-rows.json --a-posted bcrust-posted.json \
//        --b bc-rows.json --b-posted bc-posted.json --out-md diff.md --out-json diff.json [--a-name bc-rust] [--b-name BC]
// Rows come from read-pg.mjs (bc-rust) and read-sql.ps1 (Business Central); both use Business Central's SQL
// column names and text values. Normalization, so only real differences remain:
//  - Document numbers come from number series and differ: every document number is replaced by a token from the
//    External Document No. "ORC-Dnn" its entries carry (ORC-D05#1 = that document's first document number in
//    entry order, #2 the second, e.g. shipment then invoice); order numbers from the post log.
//  - Entry-number references (Transaction No., Closed by Entry No., Cust. Ledger Entry No., ...) become offsets
//    from each side's baseline ("+3"), or "old" for an entry that existed before the run.
//  - Dimension Set ID becomes the dimension set's content (DIM=VALUE;...).
//  - Decimals lose trailing zeros. Entry No., $system* columns, timestamps and register numbers are ignored.
import { readFileSync, writeFileSync } from "node:fs";

const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : d; };
const A = { name: opt("--a-name", "bc-rust"), rows: JSON.parse(readFileSync(opt("--a"), "utf8")), posted: JSON.parse(readFileSync(opt("--a-posted"), "utf8")) };
const B = { name: opt("--b-name", "BC 29.1"), rows: JSON.parse(readFileSync(opt("--b"), "utf8")), posted: JSON.parse(readFileSync(opt("--b-posted"), "utf8")) };
// baselines: --a-base / --b-base, else the "base" read-pg.mjs embeds in its rows file
if (opt("--a-base")) A.base = JSON.parse(readFileSync(opt("--a-base"), "utf8"));
if (opt("--b-base")) B.base = JSON.parse(readFileSync(opt("--b-base"), "utf8"));
const TABLE_ORDER = ["17", "254", "21", "379", "25", "380", "32", "5802"];

// SIFT Bucket No. is Item Register No. mod 5 (ItemLedgerEntry/ValueEntry OnInsert), a register counter like the rest
const IGNORE = new Set(["Entry No_", "timestamp", "Last Modified DateTime", "Item Register No_", "G_L Register No_", "Application No_", "SIFT Bucket No_",
  "SystemId", "SystemCreatedAt", "SystemCreatedBy", "SystemModifiedAt", "SystemModifiedBy"]);
const DOC_FIELDS = new Set(["Document No_", "Applies-to Doc_ No_", "Order No_", "Applies-to ID", "Closed by Document No_", "Initial Document No_"]);
// entry-number references: field -> table whose baseline they are relative to ("TX" = Transaction No.)
const REF = {
  "Transaction No_": "TX",
  "Cust_ Ledger Entry No_": "21", "Applied Cust_ Ledger Entry No_": "21",
  "Vendor Ledger Entry No_": "25", "Applied Vend_ Ledger Entry No_": "25",
  "Item Ledger Entry No_": "32", "Applies-to Entry": "32", "Appl_-to Item Entry": "32", "Appl_-from Item Entry": "32",
  "Unrealized VAT Entry No_": "254",
};
// table-specific references: a Value Entry's Applies-to Entry is another value entry (the entry a cost adjustment or
// revaluation adjusts), not an item ledger entry as on table 32
const REF_BY_TABLE = { "5802": { "Applies-to Entry": "5802" } };
const SELF_REF = new Set(["Closed by Entry No_", "Reversed by Entry No_", "Reversed Entry No_", "Unapplied by Entry No_", "Last Dim_ Correction Entry No_"]);
const PAIR_KEYS = {
  "17": [["G_L Account No_", "Amount"], ["G_L Account No_"]],
  "254": [["Type", "Base", "Amount"], ["Type", "VAT Bus_ Posting Group", "VAT Prod_ Posting Group"]],
  "21": [["Customer No_", "Document Type"]], "25": [["Vendor No_", "Document Type"]],
  "379": [["Entry Type", "Amount"], ["Entry Type"]], "380": [["Entry Type", "Amount"], ["Entry Type"]],
  "32": [["Item No_", "Entry Type", "Quantity"], ["Item No_"]],
  "5802": [["Item No_", "Entry Type", "Item Ledger Entry Type", "Valued Quantity", "Cost Amount (Actual)"], ["Item No_", "Entry Type", "Item Ledger Entry Type", "Valued Quantity"], ["Item No_"]],
};

const normDec = (v) => {
  if (typeof v !== "string" || !/^-?\d+\.\d+$/.test(v)) return v;
  let s = v.replace(/0+$/, "").replace(/\.$/, "");
  return s === "-0" ? "0" : s;
};

function prepare(side) {
  const base = side.base?.entryNo ? side.base : side.rows.base;
  if (!base?.entryNo) throw new Error(`${side.name}: no baseline (pass --a-base/--b-base)`);
  side.baseline = base;
  // document number -> token
  const extDocs = new Map(); // ext -> [docNo...] in first-appearance order
  const docTok = new Map();
  for (const t of TABLE_ORDER) for (const r of side.rows.tables[t]?.rows || []) {
    const ext = r["External Document No_"], doc = r["Document No_"];
    if (!doc || !/^ORC-D\d+$/.test(ext || "")) continue;
    if (docTok.has(doc)) continue;
    const list = extDocs.get(ext) || []; list.push(doc); extDocs.set(ext, list);
    docTok.set(doc, doc === ext ? ext : `${ext}#${list.length}`);
  }
  for (const d of Object.values(side.posted.docs || {})) {
    if (d.draftNumber && !docTok.has(d.draftNumber)) docTok.set(d.draftNumber, `ORC-${d.key}:draft`);
    if (d.postedNumber && !docTok.has(d.postedNumber)) docTok.set(d.postedNumber, `ORC-${d.key}:posted`);
  }
  side.docTok = docTok;
  // Descriptions quote the draft's number ("Invoice 102309" = Posting Description); on one side that number can also be a
  // later document's posted number, so the post log's draft numbers win there
  const draftTok = new Map();
  for (const d of Object.values(side.posted.docs || {})) if (d.draftNumber) draftTok.set(d.draftNumber, `ORC-${d.key}:draft`);
  // entry-number references -> the referenced new row ("ORC-D01#1.2" = 2nd row of that document in that table),
  // transaction numbers -> their rank among the run's transactions ("tx3"); "old" = an entry from before the run
  const rowId = {}, txRank = new Map();
  for (const t of TABLE_ORDER) {
    rowId[t] = new Map();
    const seen = new Map();
    for (const r of side.rows.tables[t]?.rows || []) {
      const tok = docTok.get(r["Document No_"]) ?? r["Document No_"];
      const n = (seen.get(tok) || 0) + 1; seen.set(tok, n);
      rowId[t].set(String(r["Entry No_"]), `${tok}.${n}`);
    }
  }
  // a transaction is named after the document of its first G/L entry ("ORC-D17~2" = D17's second transaction), so an
  // extra transaction on one side (e.g. a cost adjustment of an older document) doesn't shift every later one
  const txPerDoc = new Map();
  for (const r of side.rows.tables["17"]?.rows || []) {
    const tx = String(r["Transaction No_"]);
    if (txRank.has(tx)) continue;
    const tok = docTok.get(r["Document No_"]) ?? r["Document No_"];
    const n = (txPerDoc.get(tok) || 0) + 1; txPerDoc.set(tok, n);
    txRank.set(tx, `${tok}~${n}`);
  }
  const ref = (v, tbl) => {
    const n = Number(v);
    if (!n) return v;
    const b = tbl === "TX" ? base.transactionNo : base.entryNo[tbl];
    if (b !== undefined && n <= b) return "old";
    if (tbl === "TX") return txRank.get(String(v)) ?? `tx?+${n - b}`;
    // CLE/VLE entry numbers come from the G/L entry counter, so look the row up rather than trusting offsets
    return rowId[tbl]?.get(String(v)) ?? (b !== undefined ? `?+${n - b}` : v);
  };
  side.norm = {};
  for (const t of TABLE_ORDER) {
    side.norm[t] = (side.rows.tables[t]?.rows || []).map((r) => {
      const o = {};
      for (const [k, v0] of Object.entries(r)) {
        if (IGNORE.has(k) || k.startsWith("$")) continue;
        let v = normDec(v0);
        if (DOC_FIELDS.has(k) && v) v = docTok.get(v) ?? v;
        else if (k === "Description" && v) v = v.replace(/[A-Za-z0-9+-]+/g, (w) => draftTok.get(w) ?? docTok.get(w) ?? w); // "Invoice 102304"
        else if (REF_BY_TABLE[t]?.[k]) v = ref(v, REF_BY_TABLE[t][k]);
        else if (REF[k]) v = ref(v, REF[k]);
        else if (SELF_REF.has(k)) v = ref(v, t);
        else if (k === "Dimension Set ID") v = v === "0" ? "" : side.rows.dimsets?.[v] ?? `?set ${v}`;
        o[k] = v;
      }
      o.__entry = r["Entry No_"];
      return o;
    });
  }
}
prepare(A); prepare(B);

const report = { a: A.name, b: B.name, tables: {}, diffs: [], onlyA: [], onlyB: [], columns: {} };
for (const t of TABLE_ORDER) {
  const ra = A.norm[t], rb = B.norm[t];
  const colsA = new Set(ra.flatMap((r) => Object.keys(r))), colsB = new Set(rb.flatMap((r) => Object.keys(r)));
  report.columns[t] = { onlyA: [...colsA].filter((c) => !colsB.has(c) && c !== "__entry"), onlyB: [...colsB].filter((c) => !colsA.has(c) && c !== "__entry") };
  const groups = new Map();
  const add = (side, r) => { const g = r["Document No_"] || "(blank)"; if (!groups.has(g)) groups.set(g, { a: [], b: [] }); groups.get(g)[side].push(r); };
  ra.forEach((r) => add("a", r)); rb.forEach((r) => add("b", r));
  let paired = 0, withDiff = 0;
  for (const [g, { a, b }] of groups) {
    const usedB = new Set(), pairs = [];
    const freeA = new Set(a.map((_, i) => i));
    for (const keys of [...(PAIR_KEYS[t] || []), []]) {
      for (const i of [...freeA]) {
        const k = keys.map((c) => a[i][c]).join("|");
        const j = b.findIndex((rb2, jj) => !usedB.has(jj) && keys.map((c) => rb2[c]).join("|") === k);
        if (j >= 0) { usedB.add(j); freeA.delete(i); pairs.push([a[i], b[j]]); }
      }
    }
    for (const i of freeA) report.onlyA.push({ table: t, doc: g, entry: a[i].__entry, row: a[i] });
    b.forEach((r, j) => { if (!usedB.has(j)) report.onlyB.push({ table: t, doc: g, entry: r.__entry, row: r }); });
    for (const [x, y] of pairs) {
      paired++;
      let any = false;
      for (const c of Object.keys(x)) {
        if (c === "__entry" || !(c in y)) continue;
        if (x[c] !== y[c]) { any = true; report.diffs.push({ table: t, doc: g, field: c, a: x[c], b: y[c], entryA: x.__entry, entryB: y.__entry }); }
      }
      if (any) withDiff++;
    }
  }
  report.tables[t] = { name: A.rows.tables[t]?.name || B.rows.tables[t]?.name, rowsA: ra.length, rowsB: rb.length, paired, pairsWithDiff: withDiff };
}

// markdown
const md = [];
const esc = (s) => String(s ?? "").replace(/\|/g, "\\|").slice(0, 80);
md.push(`# Ledger oracle: ${A.name} vs ${B.name}`, "");
md.push(`Posted: ${A.name} ${Object.values(A.posted.docs).filter((d) => d.ok).length}/${Object.keys(A.posted.docs).length}, ${B.name} ${Object.values(B.posted.docs).filter((d) => d.ok).length}/${Object.keys(B.posted.docs).length} documents.`, "");
const failed = [...new Set([...Object.keys(A.posted.docs), ...Object.keys(B.posted.docs)])].filter((k) => !A.posted.docs[k]?.ok || !B.posted.docs[k]?.ok);
if (failed.length) {
  md.push("## Documents that did not post on both", "", "| doc | " + A.name + " | " + B.name + " |", "|---|---|---|");
  for (const k of failed) md.push(`| ${k} | ${esc(A.posted.docs[k]?.ok ? "ok" : A.posted.docs[k]?.error)} | ${esc(B.posted.docs[k]?.ok ? "ok" : B.posted.docs[k]?.error)} |`);
  md.push("");
}
md.push("## Tables", "", `| table | rows ${A.name} | rows ${B.name} | paired | pairs with differences | only ${A.name} | only ${B.name} |`, "|---|---:|---:|---:|---:|---:|---:|");
for (const t of TABLE_ORDER) {
  const s = report.tables[t];
  md.push(`| ${t} ${s.name} | ${s.rowsA} | ${s.rowsB} | ${s.paired} | ${s.pairsWithDiff} | ${report.onlyA.filter((x) => x.table === t).length} | ${report.onlyB.filter((x) => x.table === t).length} |`);
}
md.push("", "## Differences by field (table, field: count, documents, first example)", "", `| table | field | n | documents | ${A.name} | ${B.name} |`, "|---|---|---:|---|---|---|");
const byField = new Map();
for (const d of report.diffs) { const k = `${d.table}|${d.field}`; if (!byField.has(k)) byField.set(k, []); byField.get(k).push(d); }
for (const [k, ds] of [...byField].sort((x, y) => y[1].length - x[1].length)) {
  const [t, f] = k.split("|");
  const docs = [...new Set(ds.map((d) => d.doc.replace(/^ORC-/, "")))];
  md.push(`| ${t} | ${esc(f)} | ${ds.length} | ${esc(docs.join(" "))} | ${esc(ds[0].a)} | ${esc(ds[0].b)} |`);
}
md.push("", "## Every difference", "", `| table | doc | field | ${A.name} | ${B.name} | entry ${A.name} / ${B.name} |`, "|---|---|---|---|---|---|");
for (const d of report.diffs) md.push(`| ${d.table} | ${esc(d.doc)} | ${esc(d.field)} | ${esc(d.a)} | ${esc(d.b)} | ${d.entryA} / ${d.entryB} |`);
const brief = (r) => Object.entries(r).filter(([k, v]) => v && v !== "0" && !k.startsWith("__") && /No_|Amount|Type|Quantity|Account|Item|Posting Group|Description/.test(k)).slice(0, 14).map(([k, v]) => `${k}=${String(v).replace(/\|/g, "/")}`).join(", ");
md.push("", `## Rows only in ${A.name}`, "");
for (const x of report.onlyA) md.push(`- ${x.table} ${x.doc} entry ${x.entry}: ${brief(x.row).slice(0, 500)}`);
md.push("", `## Rows only in ${B.name}`, "");
for (const x of report.onlyB) md.push(`- ${x.table} ${x.doc} entry ${x.entry}: ${brief(x.row).slice(0, 500)}`);
md.push("", "## Columns present on one side only (not compared)", "");
for (const t of TABLE_ORDER) {
  const c = report.columns[t];
  if (c.onlyA.length || c.onlyB.length) md.push(`- ${t}: only ${A.name}: ${c.onlyA.join(", ") || "-"}; only ${B.name}: ${c.onlyB.join(", ") || "-"}`);
}
if (opt("--out-md")) writeFileSync(opt("--out-md"), md.join("\n") + "\n");
if (opt("--out-json")) writeFileSync(opt("--out-json"), JSON.stringify(report, null, 1));
console.log(md.slice(0, 40).join("\n"));
