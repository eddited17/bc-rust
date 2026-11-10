# Report rendering: the layout engine (RDLC to Excel first)

## Where we are

`crates/al-runtime/src/report.rs` runs reports with BC's semantics: triggers, data items, request
pages, and the dataset BC builds. That covers the flat TestRequestPage.SaveAsXml rows, the SaveAs(Xml)
tree, parent rows after children (`LoopChildDataItemsAsync`), FlowFilter links, auto-calculated
FlowFields and the empty-report error.

For PDF, Word and HTML it writes a structurally valid file of the format whose content is the
dataset as text, one line per row. Excel output of an RDLC layout goes through the layout engine
described here (since 2026-11-08 12:01 UTC); where the engine has no answer, the dataset output
stays. Tests that read the document as BC lays it out still fail where the engine doesn't reach
yet, for example:

- "There is no Excel Buffer within the filter" (LibraryReportValidation reads a fixed cell such as
  AK45);
- "Row not found for Column … in report";
- "Incorrect value in cell R# C#";
- FA Derogatory's "column hidden" check.

This document is the design of the engine that replaces the dataset dump. Excel export comes first,
because it is what those tests read.

## What BC does

BC renders RDLC layouts with Microsoft ReportViewer. `Microsoft.ReportViewer.Common.dll` ships in
platform.zip. The pipeline
has three stages:

1. **Processing.** The dataset rows are bound to the layout: tablix groups, details, filters and
   sorting, and expressions in VB.NET with the layout's `<Code>` block. Visibility/Hidden expressions
   decide which items exist.
2. **Pagination.** Soft-page processing produces a page layout (RPL) with each item's absolute
   position and size in millimeters.
3. **Rendering.** For Excel this is
   `Microsoft.ReportingServices.Rendering.ExcelRenderer.Layout.LayoutEngine`:
   - Every item of a page adds its left and right edge as **column edges** and its top and bottom
     edge as **row edges** (`AddRowItemStruct`).
   - Edges are in 20ths of a point (`LayoutConvert.ConvertMMTo20thPoints`:
     `(int)Math.Round(mm / 25.4 * 72 * 20 + 0.00051, 3)`).
   - Edge 0 is always present. The sorted distinct edges are the sheet's columns and rows
     (`CompletePage`).
   - An item covers the cells from its left/top edge up to its right/bottom edge, merged when it
     spans more than one.
   - Row height is capped at 8180 (409 pt).

What the engine's A/B runs (2026-11-08, w1demo and contoso) established about BC's Excel grid:

- **The page header is in the grid**, above the body: ReportViewer passes
  `headerInBody = !SimplePageHeaders`, and BC doesn't set SimplePageHeaders. The SPBIF reader adds
  the header's height to the body's top. ERM Fixed Assets Reports' Book Value 02 tests read the
  header's title and group captions (`Code.GetData` textboxes that exist only in the page header)
  from cells: 7 of them pass only with the header in the grid. The Archived Sales Quote totals
  also land on row 46 with the header (BC: 45) and on row 7 without it.
- **The header's expressions run after the body's**, and **hidden items still evaluate**: BC's
  layouts call `Code.SetData` from the Hidden expressions of textboxes in a hidden table (Archived
  Sales Quote's `table3`), and the page header's `Code.GetData` reads what they stored.
- **Only items present at run time add edges.** Items whose Hidden is true (static or evaluated),
  and empty tablix cells, add none; textboxes with an empty value do. A static model that counted
  every item the layout defines (`rdlc::column_edges`, retired for I12) put the Archived Sales
  Quote totals in AK without the header. That match was a coincidence (with the header it gives BM).
- **Edges follow the SPBIF reader**: each child's `ConvertMMTo20thPoints(Left)` is added to its
  parent's 20ths, and a tablix's column and row edges are `Math.Round` of the unrounded running sums
  (`ReadTablixStructure`).
- **A tablix row has one TablixCell per column.** A cell with `ColSpan` N is followed by N−1 empty
  placeholder cells, so a cell's index is its column. Counting spans cumulatively put two items at
  one address (Job Actual To Budget's alternative columns).
- **One cell per address.** Two items starting at the same cell give one cell, never two (the tests'
  Excel Buffer rejects a second one).
- **Dataset types.** report.rs's rows are text; Boolean columns (`True`/`False`) reach expressions
  as Booleans, and `Max`/`Min` of Booleans order False < True (VB's numeric True is −1). Book Value
  02 hides its depreciation sections with `iif(Max(Fields!ShowSectionN.Value) = FALSE, …)`.

- **A data region's Hidden runs in the data region's own scope** (`ReportItem.EvaluateStartHidden`
  sets up the data region instance: `DataRegionInstance.SetupEnvironment`): its fields are its first
  row after its own filters, and with no rows every field is Nothing. Evaluated on the enclosing
  list's first row instead, the VAT specification tables of Archived Sales Quote and the purchase
  order reports stay hidden (ERM Excel Reports: "VAT spec line not found").
- **Zero-size items are dropped**: the SPBIF reader skips an item of zero width or height (a line
  only when both are zero) and a tablix cell of zero width or height, so they add no edges. With
  this rule the Archived Sales Quote totals land on BC's row (45).
- **A group's page break starts a page, and each page is a worksheet** with its own page header,
  bound after that page's body (so `Code.GetData` reads that page's `Code.SetData`), and
  `Globals!PageNumber`/`TotalPages` per page. Sales - Shipment breaks between documents; with one
  sheet the header showed only one document's data (ERM Sales Report III's
  `SalesShipmentReportHasExternalDocNoAndOrderNo` expects each document's on its own sheet).

Still open: the Archived Sales Quote totals land in AH45 where BC's tests read AK45 (three column
edges short), and the real BC export that would show which items BC has there is still to do (see
"Real BC exports" below).

**Real BC exports (2026-11-08 window).** The tooling is in place: an extension (`P9 RDLC Export`,
codeunit web service `ExportReport(reportKey, format)` returning the Excel export or the dataset
XML), a harness that renders BC's own dataset with the engine, and a cell-by-cell comparison. The
window produced no exports: the host's port forward truncates large OData responses, and the
`Report.SaveAs` web service calls then never returned inside BC (the stuck requests held the
user's OData slots until the container was restarted). The next window should render through a
background session (job queue) that stores the files in a table, starting with one small report.

## Design

The engine has one layout model and three stages. The model and the page layout are independent of
the output format, so the same stages can later drive PDF and HTML (absolute positioning) without
rewriting them. Excel is the first consumer.

| Stage | Module | Input → output | Status |
|---|---|---|---|
| 1. Layout model | `rdlc.rs` | RDLC XML → sections, items (textbox, rectangle, tablix with columns/rows/cells, image, line), geometry in mm, Hidden expressions, Format | **landed** (2026-11-08) |
| 2. Expressions | `rdlc_expr.rs`, `rdlc_vb.rs` | VB subset → values; the layout's `<Code>` block interpreted | **landed** (2026-11-08 12:01 UTC; `<Code>` interpreter for I12) |
| 3. Binding | `rdlc_bind.rs` | layout + dataset rows → bound items (tablix members expanded per group/detail row, Hidden evaluated) | **landed** |
| 4. Page layout | `rdlc_xlsx.rs` | bound items → positioned boxes (page header above the body; one page, no page breaks yet) | **landed** |
| 5. Excel writer | `rdlc_xlsx.rs` | boxes → edge grid → cells (numbers as numbers with Excel number formats, merges, column widths, row heights) → workbook (existing zip writer) | **landed** (number formats and row heights for I12) |

1. **Layout model** (landed). `rdlc::parse` reads the 2008–2016 schemas. Its output is
   `Report/Body` or `ReportSections/ReportSection/{Body,Page/PageHeader,Page/PageFooter}`, with items
   positioned relative to their container.
   - A tablix row has one TablixCell per column; a cell spanning N columns is followed by N−1
     empty placeholders.
   - The grid comes from the bound items (stage 4), not from the layout: hidden items and empty
     cells add no edges, the page header does. `column_letters` gives the cell letters.
2. **Expressions.** These are RDLC's VB.NET expressions as BC's layouts use them:
   - `Fields!X.Value`, `Parameters!X.Value`, `Globals!…`, `ReportItems!X.Value`;
   - `First`, `Last`, `Sum`, `Count`, `RunningValue`, `IIf`, `Format`, `CStr`, `Len`;
   - `&`, arithmetic and comparisons (VB's `+`: a String operand concatenates, Nothing as "");
   - methods on values (`.ToString()`, `.Trim()`, `.Substring(…)`), `Round` (to even), `Abs`;
   - VB's `Replace` returns the text unchanged for an empty search string (Book Value 01's column
     headings are `Replace(x, "", Chr(10))`; a per-character replace broke 18 tests' caption lookups);
   - calls into the layout's `<Code>` block.

   BC's `<Code>` blocks share a family of standard functions (`SetData`, `GetData`, `IsNewPage`,
   `BlankZero`, `BlankPos`, `GetGroupPageNumber`, …), which rdlc_bind implements natively with the
   same state rules (shared per report). About a third of the W1 layouts (130 of 384) also declare
   their own functions: `CalcPct` (15 layouts), `SetRowColor` (11), then a long tail used by one or
   two layouts each. `rdlc_vb.rs` interprets those: module variables, `Function`/`Sub` with
   parameters, assignment, `Return`, `If`/`ElseIf`/`Else` (with or without `Then`), `Select Case`,
   `For … Next`, `On Error Resume Next`. 382 of the 384 W1 blocks parse. A function the
   interpreter can't run is reported (`Session::missing`) and the report falls back.
3. **Binding.** The input is the dataset report.rs already builds (the same rows BC builds). It works
   through the tablix member hierarchy:
   - static members repeat once; dynamic members once per group value (group expressions over the
     rows in scope) or per detail row;
   - tablix and group filters and sort expressions apply;
   - aggregates are scoped to the group (`Sum(…, "Group")`);
   - Hidden expressions are evaluated per instance, and hidden members and items are dropped (they
     add no edges, as in BC).
4. **Page layout.** Items are placed top to bottom:
   - a tablix grows by its expanded rows' defined heights;
   - items below a growing item move down by the growth (RDLC's "items stay below" rule);
   - explicit page breaks start a new page;
   - CanGrow text measurement is a later refinement, because it changes heights but rarely the
     number of rows.
5. **Excel writer.** It applies LayoutEngine's grid rule to the boxes:
   - a textbox becomes a cell, or a merged range, at its edges;
   - numbers stay numbers; the textbox's `Format` (evaluated per instance, often
     `=Fields!AmountFormat.Value`) becomes the cell's Excel number format as ReportViewer's
     `FormatHandler.GetNumberFormat` converts it (en-US: `N2` → `#,##0.00;-#,##0.00`, `C` →
     `"$"#,##0.00;\-"$"#,##0.00`, custom formats with literals quoted). The tests read values, not
     formats, so this is fidelity; whether BC adds a language prefix (`[$-409]`) is to check against
     a real export;
   - column widths and row heights come from the edge distances (heights capped at 409 pt);
   - the page header goes into the grid above the body (the page footer isn't placed yet);
   - one worksheet per page break as in BC (`Comment: 1` in the tests is the sheet number).

**Integration.** `report.rs::rdlc_excel` calls the engine for Excel output when the report's default
layout is RDLC and its file is available. Otherwise, or when the engine returns an error (counted in
`Session::missing`), the dataset output stays, so nothing regresses where the engine has no answer. Word (docx) layouts are a separate path with their own model (content
controls bound to the dataset), and only the PDF/HTML positioning reuses stages 1–4.

## Plan

- **Landed 2026-11-08 12:01 UTC:** stages 2–5, wired into Excel output (`report.rs::rdlc_excel`:
  the report's RDLC layout when its default layout is RDLC; any engine error, such as a `<Code>`
  function outside BC's standard set, is counted in `Session::missing` and the dataset output stays).
  Full A/B against the same tree: w1demo +27/−2, contoso +26/−1. All three losses are ERM Financial
  Report Schedules tests that flip between runs of the same binary. The gains are Job Reports II
  (11), ERM Fixed Assets Reports (7, Book Value 02 group totals and captions), SCM Inventory
  Reports - I (3), ERM FA Derogatory Depreciation (2, including
  FABookValue01WithoutLinkedBookHidesDerogatoryColumns), Marketing Reports, ERM MIR Reports and
  ERM Excel Reports. `BC_RDLC_DUMP=<file>` appends each rendered sheet's cells to the file.
- **Landed for I12 (2026-11-08)**, each after a w1demo + contoso A/B on four slices of the suite; a
  fifth slice, the 358 codeunits those four missed (Tests-Report among them), was then run for all
  three (+0/−0 for the first two together, part of the third's A/B):
  - 12:45 UTC: the `<Code>` interpreter (`rdlc_vb.rs`), VB `+`, methods on values, `Round` & co;
    the static grid retired. +4 real gains on each dataset (ERM MIR Reports, Job Reports II, Employee
    Reports, FA Derogatory's FABookValue01WithoutLinkedBookHidesDerogatoryColumns, which I11 still
    failed because Book Value 01 calls its own `Code.SetRowColor`).
  - 13:11 UTC: number formats, row heights, `Format` per instance, VB `Replace` with an empty
    search string. +9 on each dataset (ERM Fixed Assets Reports' Book Value 01 column captions).
  - 15:39 UTC: a data region's visibility in its own scope, zero-size items dropped, group page
    breaks with one worksheet and one page header per page. +5 real gains on each dataset (ERM Excel
    Reports' VAT specification, ERM Sales Report III's per-document shipment sheets, which I11
    regressed, ERM Purchase Reports III, ERM Financial Reports).
  The losses in these A/Bs were ERM Financial Report Schedules and CRM synch tests, which flip
  between runs of the same binary.
- **Next:** P2's 13 "There is no Excel Buffer within the filter" tests (the Archived Sales Quote
  totals land in AH45, BC's tests read AK45); item-level page breaks (`PageBreak` on a tablix or
  rectangle, 129 W1 layouts break at End); `Split` and arrays in the interpreter; the real BC export
  (see above); then the "Row not found for Column" and "Incorrect value in cell" groups.
- **Then:** PDF/HTML from the same positioned boxes.
- **Risks:**
  - the variety of VB expressions in the `<Code>` blocks outside the standard functions;
  - CanGrow measurement where a test depends on row heights;
  - item-level page breaks and page breaks inside nested data regions;
  - subreports, which BC's layouts rarely use.
