// Ledger oracle, step 2: create and post the scripted documents (documents.json) through API v2.0.
// Run it once against bc-rust and once against real Business Central; read-pg.mjs / read-sql.ps1 then read
// the new ledger entries and compare.mjs compares them.
//
// usage: node tools/oracle/post.mjs --api <base ending in /api/v2.0> --out <file.json>
//          [--docs tools/oracle/documents.json] [--only D01,D02] [--nested]
//   credentials (optional): env ORACLE_CRED=user:password, or --cred-file <file with user:password>
//   --nested: create and patch lines as salesInvoices(<id>)/salesInvoiceLines (Business Central's documented
//             form); without it lines go to the top-level line entity set with documentId (all bc-rust serves).
//   --bcrust-compat: bc-rust's API v2.0 can't yet do a few steps (see notes): currency on a document header,
//             patching a document line, inserting journal and payment lines. With this flag those steps go
//             through bc-rust's page API (/api/page/<id>/record, field by field like its web client); headers,
//             lines and posting still use API v2.0. Business Central runs without the flag.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : d; };
const api = opt("--api")?.replace(/\/$/, "");
const out = opt("--out");
if (!api || !out) { console.error("usage: post.mjs --api <.../api/v2.0> --out <file.json> [--docs F] [--only D01,..] [--nested]"); process.exit(2); }
const nested = argv.includes("--nested");
const compat = argv.includes("--bcrust-compat");
const pageBase = api.replace(/\/api\/v2\.0$/, "") + "/api/page";
const docsFile = opt("--docs", join(dirname(fileURLToPath(import.meta.url)), "documents.json"));
const spec = JSON.parse(readFileSync(docsFile, "utf8"));
const only = opt("--only")?.split(",");
let cred = process.env.ORACLE_CRED || (opt("--cred-file") ? readFileSync(opt("--cred-file"), "utf8").trim() : null);
const auth = cred ? "Basic " + Buffer.from(cred).toString("base64") : null;

const log = [];
async function call(method, path, body) {
  const headers = { "content-type": "application/json", accept: "application/json" };
  if (auth) headers.authorization = auth;
  if (method === "PATCH" || method === "DELETE") headers["if-match"] = "*";
  const t0 = performance.now();
  const r = await fetch(path.startsWith("http") ? path : api + path, { method, headers, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  const ms = Math.round(performance.now() - t0);
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { json = { raw: text.slice(0, 2000) }; }
  log.push({ method, path, body, status: r.status, ms, response: r.ok ? undefined : json });
  if (!r.ok) {
    const msg = json?.error?.message || text.slice(0, 500);
    throw new Error(`${method} ${path}: HTTP ${r.status}: ${msg}`);
  }
  return json;
}

// bc-rust page API (compat only): {record} or {error}
const pkey = (arr) => encodeURIComponent(JSON.stringify(arr.map(String)));
async function page(method, pid, key, body) {
  const url = `${pageBase}/${pid}/record` + (key ? `?key=${pkey(key)}` : "");
  const r = await call(method, url, body);
  if (r?.error) throw new Error(`page ${pid} ${method}: ${r.error}`);
  return r.record;
}
// set fields one at a time, in the given order (each PATCH validates one field, as a user would)
async function pageFields(pid, key, fields) {
  let rec;
  for (const [k, v] of fields) rec = await page("PATCH", pid, key, { [k]: v });
  return rec;
}

const companies = (await call("GET", "/companies")).value;
const company = companies.find((c) => /cronus/i.test(c.name)) || companies[0];
const cb = `/companies(${company.id})`;
const date = spec.postingDate;
const result = { api, company: company.name, postingDate: date, startedAt: new Date().toISOString(), docs: {} };

const ext = (d) => `ORC-${d.key}`;
const kinds = {
  salesInvoice: { set: "salesInvoices", lines: "salesInvoiceLines", party: "customerNumber", dateField: "invoiceDate", action: "Post", docType: "Invoice", hdrPage: 43, linePage: 47 },
  salesOrder: { set: "salesOrders", lines: "salesOrderLines", party: "customerNumber", dateField: "orderDate", action: "ShipAndInvoice", docType: "Order", hdrPage: 42, linePage: 46 },
  salesCreditMemo: { set: "salesCreditMemos", lines: "salesCreditMemoLines", party: "customerNumber", dateField: "creditMemoDate", action: "Post", docType: "Credit Memo", hdrPage: 44, linePage: 96 },
  purchaseInvoice: { set: "purchaseInvoices", lines: "purchaseInvoiceLines", party: "vendorNumber", dateField: "invoiceDate", action: "Post", extField: "vendorInvoiceNumber", costField: "unitCost", docType: "Invoice", hdrPage: 51, linePage: 55 },
  purchaseOrder: { set: "purchaseOrders", lines: "purchaseOrderLines", party: "vendorNumber", dateField: "orderDate", action: "ReceiveAndInvoice", extField: null, costField: "directUnitCost", docType: "Order", hdrPage: 50, linePage: 54 },
  purchaseCreditMemo: { set: "purchaseCreditMemos", lines: "purchaseCreditMemoLines", party: "vendorNumber", dateField: "creditMemoDate", action: "Post", extField: "vendorCreditMemoNumber", costField: "unitCost", docType: "Credit Memo", hdrPage: 52, linePage: 98 },
};

const purchase = (d) => d.kind.startsWith("purchase");
async function lookups() {
  if (lookups.v) return lookups.v;
  const acc = (await call("GET", `${cb}/accounts?$filter=number eq '2910'`)).value.find((x) => x.number === "2910");
  const cur = {};
  for (const c of spec.documents.map((d) => d.currency).filter(Boolean)) {
    cur[c] = (await call("GET", `${cb}/currencies?$filter=code eq '${c}'`)).value.find((x) => x.code === c)?.id;
  }
  return (lookups.v = { cash: acc?.id, cur });
}

async function postDocument(d, done) {
  const k = kinds[d.kind];
  const L = await lookups();
  const hdr = { [k.party]: d.customer || d.vendor, postingDate: date, [k.dateField]: date };
  const extField = k.extField === undefined ? "externalDocumentNumber" : k.extField;
  if (extField) hdr[extField] = ext(d);
  if (d.currency && !compat) hdr.currencyId = L.cur[d.currency];
  // credit memo applied to an earlier posted invoice of this run (Applies-to Doc. No. through invoiceNumber)
  if (d.appliesTo) {
    const target = done[d.appliesTo];
    if (!target?.postedNumber) throw new Error(`${d.key}: ${d.appliesTo} has no posted number`);
    hdr.invoiceNumber = target.postedNumber;
  }
  const h = await call("POST", `${cb}/${k.set}`, hdr);
  const r = { key: d.key, kind: d.kind, draftId: h.id, draftNumber: h.number, lines: [] };
  if (d.currency && compat) await page("PATCH", k.hdrPage, [k.docType, h.number], { "Currency Code": d.currency });
  const linePath = (id) => nested ? `${cb}/${k.set}(${h.id})/${k.lines}${id ? `(${id})` : ""}` : `${cb}/${k.lines}${id ? `(${id})` : ""}`;
  for (const l of d.lines) {
    // purchase lines: discount (and a purchase order's cost) in a second step; bc-rust's API rejects them on insert
    // ("No. must have a value"), Business Central takes them as a PATCH of the line
    const line = { lineType: l.type, lineObjectNumber: l.no, quantity: l.qty };
    if (l.price !== undefined) line.unitPrice = l.price;
    const later = {};
    // sales: discount on insert (bc-rust's API accepts but ignores it there, so compat sets it on the page)
    if (!purchase(d) && l.discountPercent !== undefined) (compat ? later : line).discountPercent = l.discountPercent;
    if (l.cost !== undefined) (d.kind === "purchaseOrder" ? later : line)[k.costField] = l.cost;
    if (purchase(d) && l.discountPercent !== undefined) later.discountPercent = l.discountPercent;
    const lr = await call("POST", linePath(), nested ? line : { documentId: h.id, ...line });
    if (Object.keys(later).length) {
      if (compat) {
        const f = [];
        if (later.directUnitCost !== undefined) f.push(["Direct Unit Cost", later.directUnitCost]);
        if (later.discountPercent !== undefined) f.push(["Line Discount %", later.discountPercent]);
        await pageFields(k.linePage, [k.docType, h.number, lr.sequence], f);
      } else {
        await call("PATCH", linePath(lr.id), later);
      }
    }
    r.lines.push({ sequence: lr.sequence });
  }
  if (d.invoiceDiscountAmount !== undefined) await call("PATCH", `${cb}/${k.set}(${h.id})`, { discountAmount: d.invoiceDiscountAmount });
  const before = await call("GET", `${cb}/${k.set}(${h.id})`);
  r.totals = { excl: before.totalAmountExcludingTax, tax: before.totalTaxAmount, incl: before.totalAmountIncludingTax, discount: before.discountAmount };
  // partial shipment: the first post ships and invoices partialShip[i] of line i, the second the rest
  if (d.partialShip) {
    const lines = (await call("GET", nested ? linePath() : `${linePath()}?$filter=documentId eq ${h.id}`)).value.filter((x) => x.lineType !== "Comment");
    for (let i = 0; i < d.partialShip.length && i < lines.length; i++)
      await call("PATCH", linePath(lines[i].id), { shipQuantity: d.partialShip[i] });
    await call("POST", `${cb}/${k.set}(${h.id})/Microsoft.NAV.${k.action}`);
    r.partial = true;
  }
  await call("POST", `${cb}/${k.set}(${h.id})/Microsoft.NAV.${k.action}`);
  // posted invoices and credit memos keep the draft's id (BC's aggregate tables)
  if (d.kind !== "salesOrder" && d.kind !== "purchaseOrder") {
    try {
      const p = await call("GET", `${cb}/${k.set}(${h.id})`);
      r.postedNumber = p.number; r.postedStatus = p.status; r.postedTotalIncl = p.totalAmountIncludingTax;
    } catch (e) { r.postedLookupError = e.message; }
  }
  return r;
}

const JNL = {
  journal: { set: "journals", code: "ORACLEGJ", template: "GENERAL", page: 39, source: "GENJNL" },
  customerPayment: { set: "customerPaymentJournals", code: "ORACLECP", template: "CASHRCPT", page: 255, source: "CASHRECJNL" },
  vendorPayment: { set: "vendorPaymentJournals", code: "ORACLEVP", template: "PAYMENT", page: 256, source: "PAYMENTJNL" },
};
async function journalBatch(j) {
  // our own batch on both systems: balancing account 2910 (payments), no number series
  const found = (await call("GET", `${cb}/${j.set}?$filter=code eq '${j.code}'`)).value.filter((b) => b.code === j.code);
  if (found.length) return found[0];
  const body = { code: j.code, displayName: "Ledger oracle" };
  if (j.set !== "journals") body.balancingAccountId = (await lookups()).cash;
  return call("POST", `${cb}/${j.set}`, body);
}

// one journal line, through API v2.0 (BC) or bc-rust's journal page (compat)
let lineNo = 0;
async function journalLine(j, b, f) {
  if (!compat) {
    if (j.set === "journals") return call("POST", nested ? `${cb}/journals(${b.id})/journalLines` : `${cb}/journalLines`, { journalId: b.id, accountType: "G/L Account", accountNumber: f.account, postingDate: date,
      documentNumber: f.doc, externalDocumentNumber: f.doc, amount: f.amount, description: f.description, balanceAccountType: "G/L Account", balancingAccountNumber: f.bal });
    const cust = j.set === "customerPaymentJournals";
    const set = cust ? "customerPayments" : "vendorPayments";
    return call("POST", nested ? `${cb}/${j.set}(${b.id})/${set}` : `${cb}/${set}`, { journalId: b.id, [cust ? "customerNumber" : "vendorNumber"]: f.party,
      postingDate: date, documentNumber: f.doc, externalDocumentNumber: f.doc, amount: f.amount, appliesToInvoiceNumber: f.appliesTo, description: f.description });
  }
  lineNo += 10000;
  const key = [j.template, j.code, lineNo];
  await page("POST", j.page, null, { "Journal Template Name": j.template, "Journal Batch Name": j.code, "Line No.": lineNo });
  const fields = [["Posting Date", date]];
  if (f.party) fields.push(["Document Type", "Payment"]);
  fields.push(["Document No.", f.doc], ["External Document No.", f.doc]);
  if (f.party) fields.push(["Account Type", j.set === "customerPaymentJournals" ? "Customer" : "Vendor"], ["Account No.", f.party]);
  else fields.push(["Account Type", "G/L Account"], ["Account No.", f.account]);
  fields.push(["Description", f.description], ["Amount", f.amount]);
  if (f.appliesTo) fields.push(["Applies-to Doc. Type", "Invoice"], ["Applies-to Doc. No.", f.appliesTo]);
  fields.push(["Bal. Account Type", "G/L Account"], ["Bal. Account No.", f.bal ?? "2910"], ["Source Code", j.source]);
  return pageFields(j.page, key, fields);
}

async function postJournal(d) {
  const j = JNL.journal, b = await journalBatch(j);
  for (const l of d.lines) await journalLine(j, b, { account: l.account, amount: l.amount, bal: l.bal, doc: ext(d), description: `Ledger oracle ${d.key}` });
  await call("POST", `${cb}/journals(${b.id})/Microsoft.NAV.post`);
  return { key: d.key, kind: d.kind, batchId: b.id };
}

async function postPayment(d, done) {
  const target = done[d.appliesTo];
  if (!target?.postedNumber) throw new Error(`${d.key}: ${d.appliesTo} has no posted number`);
  const j = JNL[d.kind], b = await journalBatch(j);
  const total = Number(target.postedTotalIncl ?? target.totals.incl);
  let amount = total;
  if (d.amount !== undefined) amount = d.amount;
  else if (d.payWithDiscountPercent) amount = +(total - Math.round(total * d.payWithDiscountPercent) / 100).toFixed(2);
  const signed = d.kind === "customerPayment" ? -amount : amount;
  await journalLine(j, b, { party: d.customer || d.vendor, amount: signed, appliesTo: target.postedNumber, doc: ext(d), description: `Ledger oracle ${d.key}` });
  await call("POST", `${cb}/journals(${b.id})/Microsoft.NAV.post`);
  return { key: d.key, kind: d.kind, batchId: b.id, amount: signed, appliesTo: target.postedNumber };
}

// a new item, so both systems start with the same (empty) stock; posting groups and unit of measure copied from `like`
const itemByNo = async (no) => (await call("GET", `${cb}/items?$filter=number eq '${no}'`)).value.find((x) => x.number === no);
async function createItem(d) {
  const like = await itemByNo(d.like);
  if (!like) throw new Error(`${d.key}: item ${d.like} not found`);
  const body = { number: d.no, displayName: `Ledger oracle ${d.key}`, type: "Inventory",
    generalProductPostingGroupCode: like.generalProductPostingGroupCode, inventoryPostingGroupCode: like.inventoryPostingGroupCode };
  if (d.unitCost !== undefined) body.unitCost = d.unitCost;
  if (d.unitPrice !== undefined) body.unitPrice = d.unitPrice;
  const it0 = await call("POST", `${cb}/items`, body);
  // base unit of measure in a second step: bc-rust refuses it on insert (its CurrentClientType() is always Web, so Item's
  // OnValidate doesn't insert the item first as Business Central does for API sessions; see the I15 notes)
  const it = await call("PATCH", `${cb}/items(${it0.id})`, { baseUnitOfMeasureCode: like.baseUnitOfMeasureCode });
  return { key: d.key, kind: d.kind, itemId: it.id, item: it };
}
// API v2.0 items.inventory: a PATCH posts an item journal line (positive/negative adjustment, Posting Date = Today(),
// Document No. = the item number) through Item Jnl.-Post Line (APIV2Items UpdateInventory)
async function adjustInventory(d) {
  const it = await itemByNo(d.item);
  if (!it) throw new Error(`${d.key}: item ${d.item} not found`);
  const before = Number(it.inventory), target = before + d.delta;
  const after = await call("PATCH", `${cb}/items(${it.id})`, { inventory: target });
  return { key: d.key, kind: d.kind, item: d.item, before, target, after: Number(after.inventory) };
}

for (const d of spec.documents) {
  if (only && !only.includes(d.key)) continue;
  const t0 = performance.now();
  try {
    let r;
    if (kinds[d.kind]) r = await postDocument(d, result.docs);
    else if (d.kind === "item") r = await createItem(d);
    else if (d.kind === "inventoryAdjust") r = await adjustInventory(d);
    else if (d.kind === "journal") r = await postJournal(d);
    else if (d.kind.endsWith("Payment")) r = await postPayment(d, result.docs);
    else throw new Error(`unknown kind ${d.kind}`);
    r.ok = true; r.ms = Math.round(performance.now() - t0);
    result.docs[d.key] = r;
    console.log(`${d.key} ${d.kind} ok ${r.postedNumber ?? ""} ${r.ms} ms`);
  } catch (e) {
    result.docs[d.key] = { key: d.key, kind: d.kind, ok: false, error: e.message, ms: Math.round(performance.now() - t0) };
    console.log(`${d.key} ${d.kind} FAILED: ${e.message}`);
  }
}
result.finishedAt = new Date().toISOString();
result.calls = log;
writeFileSync(out, JSON.stringify(result, null, 1));
console.log(`wrote ${out}: ${Object.values(result.docs).filter((d) => d.ok).length}/${Object.keys(result.docs).length} documents posted`);
