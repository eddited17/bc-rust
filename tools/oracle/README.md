# Ledger oracle

Posts the same scripted documents on bc-rust and on a real Business Central 29.1 (the `bcside` container in the
bc-sidebyside VM) and compares the ledger entries they produce, field by field. A green test run says the assertions
held; this checks that the books match.

Compared tables: G/L Entry (17), VAT Entry (254), Cust. Ledger Entry (21), Detailed Cust. Ledg. Entry (379), Vendor
Ledger Entry (25), Detailed Vendor Ledg. Entry (380), Item Ledger Entry (32), Value Entry (5802). Only Base App
columns are compared (table-extension columns live in `$ext` tables on SQL Server).

Status (2026-11-10, I15):
- D01-D20 ran in I14 (bc-rust 7a23b1c, BC 29.1 container `bcside`): 19 of 20 posted on each, and D12 refused by both
  with the same Vendor Invoice No. error. bc-rust ffd9f0c reproduces that result exactly.
- D21-D43 ran in I15: 23 of 23 on each, and all 252 ledger rows pair up. The one difference is the Currency Factor's
  precision on 2 rows: BC's AL Decimal keeps 18 significant digits (Decimal18).
- Results and diagnosis: `data/capture/oracle-2026-11-09/notes.md` and `data/capture/oracle-2026-11-10/notes.md` (outside git).

## Files

| file | what |
|---|---|
| `documents.json` | the documents: sales invoices/orders/credit memos, purchase invoices/order/credit memo, general journals with VAT, payments applied to invoices. Explicit prices, costs, discounts and one posting date. Every document carries External Document No. `ORC-<key>`. D21-D43 work on two items the script creates (`kind: item`, posting groups copied from `like`), so both systems start with no stock of them: FIFO layers, a sale into negative stock and its cost adjustment, returns, credit memos applied to invoices (`appliesTo`), partial shipments (`partialShip`), and inventory adjustments through API v2.0 `items.inventory` (`kind: inventoryAdjust`; BC posts an item journal line with Posting Date = Today() and Document No. = the item number). |
| `post.mjs` | creates and posts them through API v2.0 (`--nested` for BC's documented line paths; `--bcrust-compat` only for bc-rust before 49adb06) |
| `read-pg.mjs` | bc-rust side: baseline (highest Entry No. per table) and the new rows, from PostgreSQL, renamed to BC's SQL column names |
| `read-sql.ps1` | BC side, runs inside the container: the same from SQL Server (read-only SELECTs) |
| `vm-read.sh` | runs `read-sql.ps1` in the container over ssh and saves its JSON |
| `compare.mjs` | normalizes both sides and writes the difference report |
| `setup.mjs` | reads and compares the setup the documents depend on (posting setups, VAT setup, exchange rates, the customers, vendors, items and accounts used) |
| `tables.mjs` | table list and field names (from the W1 Base App AL sources) |

No credentials live here. The VM's ssh key and the BC password come from `data/vm/` (gitignored) or the environment
(`ORACLE_VM_KEY`, `ORACLE_CRED=user:password`). Database names and containers come from arguments/environment.

## Run

```sh
# bc-rust: a scratch database restored from a CRONUS dump, `bc serve --pg` on a free port
node tools/oracle/read-pg.mjs baseline --db <scratch> --out A/bcrust-base.json
node tools/oracle/post.mjs --api http://127.0.0.1:<port>/api/v2.0 --out A/bcrust-posted.json
node tools/oracle/read-pg.mjs read --db <scratch> --base A/bcrust-base.json --out A/bcrust-rows.json
node tools/oracle/setup.mjs pg --db <pristine copy> --out A/bcrust-setup.json

# Business Central (VM running, container bcside up); call from the repo root
tools/oracle/vm-read.sh setup B/bc-setup.json
tools/oracle/vm-read.sh baseline B/bc-base.json
ORACLE_CRED=admin:<password> node tools/oracle/post.mjs --api http://127.0.0.1:7048/BC/api/v2.0 --nested --out B/bc-posted.json
tools/oracle/vm-read.sh read B/bc-rows.json B/bc-base.json

node tools/oracle/compare.mjs --a A/bcrust-rows.json --a-posted A/bcrust-posted.json \
  --b B/bc-rows.json --b-posted B/bc-posted.json --b-base B/bc-base.json --out-md diff.md --out-json diff.json
node tools/oracle/setup.mjs compare --a A/bcrust-setup.json --b B/bc-setup.json --out-md setup.md
```

Post on a fresh copy each time: the documents are not idempotent (number series, inventory, applied entries).
`--only D21,...` posts a subset. A container that already holds D01-D20 refuses them again: Business Central rejects a
second purchase invoice with the same vendor document number. Run each half with `--only`, and baseline/read around each.

## Normalization (compare.mjs)

- Document numbers come from number series and differ. Each is replaced by a token from the External Document No. its
  entries carry (`ORC-D05#1` is the first document number of D05 in entry order, `#2` the second, e.g. shipment then
  invoice). Order numbers come from the post log.
- Entry-number references (Transaction No., Closed by Entry No., Cust. Ledger Entry No., Item Ledger Entry No., ...)
  become offsets from each side's baseline (`+3`), or `old` for an entry that existed before the run.
- Dimension Set ID becomes the set's content (`AREA=30;CUSTOMERGROUP=LARGE`).
- Decimals lose trailing zeros. Entry No., `$system*` columns, `timestamp`, Last Modified DateTime, register numbers,
  SIFT Bucket No. (Item Register No. mod 5) and DCLE/DVLE Application No. are ignored. Transaction numbers are named
  after the document of their first G/L entry (`ORC-D17~2`).
- A Value Entry's Applies-to Entry is a value entry reference, the entry a cost adjustment adjusts. On the Item Ledger
  Entry it is an item ledger entry reference. Descriptions such as "Invoice 102309" quote the draft number, so the post
  log's draft numbers are used there before ledger document numbers.
- Rows are grouped by document token and paired by a table-specific key (G/L account + amount, item + entry type +
  quantity, ...), then in entry order.

## `--bcrust-compat`

bc-rust's API v2.0 before 49adb06 (I14 S6) ignored or refused some of these inputs: currencyCode/currencyId, sales and
purchase line discounts, line PATCH, journal and payment lines. The flag routes those steps through bc-rust's page API
(`/api/page/<id>/record`, one field per PATCH). From 49adb06 on, run without it, as on Business Central.
