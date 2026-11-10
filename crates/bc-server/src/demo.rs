//! Full demo company from Microsoft's own demo-data tool.
//!
//! Microsoft builds the CRONUS company by creating a company (codeunit 2 "Company-Initialize")
//! and then running codeunit 5193 "Contoso Demo Tool".CreateAllDemoData() from the W1
//! "Contoso Coffee Demo Dataset" app. This module runs exactly that AL, unmodified, through the
//! interpreter on any backend, and reports what it created.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use al_syntax::ObjectKind;
use bc_store::filter::FieldFilter;
use bc_store::{Agg, AggOut, Backend, Conn, MemBackend, Row, ScanReq, SeqOp, StoreError, TableSchema};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// The legacy W1 demo tool's company (what BCApps' CI runs the Base App tests on).
pub mod w1;

/// Codeunit 2 "Company-Initialize": what BC runs for every new company.
pub const COMPANY_INITIALIZE: i64 = 2;
/// Codeunit 5193 "Contoso Demo Tool" (Contoso Coffee Demo Dataset app).
pub const CONTOSO_DEMO_TOOL: i64 = 5193;

/// App directories for a server or test run that can create demo data: the default apps plus
/// the Contoso Coffee Demo Dataset app.
pub fn demo_app_dirs(root: &Path) -> Vec<PathBuf> {
    al_model::app_dirs_with(root, &al_model::demo_tool_dirs(root))
}

/// How much demo data to create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// CreateSetupDemoData(): setup data only (what Microsoft uses for "My Company").
    Setup,
    /// CreateAllDemoData(): setup, master, transactional and historical data (CRONUS).
    All,
}

/// Rows written while one Contoso module ran one level (from the tool's own telemetry markers).
#[derive(Clone, Debug, serde::Serialize)]
pub struct Phase {
    pub module: String,
    pub level: String,
    pub rows: i64,
    pub ms: f64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TableCount {
    pub id: i64,
    pub name: String,
    pub rows: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct DemoReport {
    pub company: String,
    pub backend: String,
    pub init_ms: f64,
    pub contoso_ms: f64,
    pub steps: u64,
    pub phases: Vec<Phase>,
    pub tables: Vec<TableCount>,
    pub total_rows: usize,
    /// Platform calls that were shimmed or missing while the AL ran (Session::missing).
    pub missing: BTreeMap<String, usize>,
    /// Steps that failed but did not stop the run (Company-Initialize).
    pub warnings: Vec<String>,
}

/// Net rows written per table by one session (inserts minus deletes), with the label of the
/// Contoso phase that is running.
#[derive(Default)]
struct Counter {
    phase: Option<(String, String, Instant)>,
    rows: i64,
    phases: Vec<Phase>,
}

impl Counter {
    fn close(&mut self) {
        if let Some((module, level, t0)) = self.phase.take() {
            self.phases.push(Phase { module, level, rows: std::mem::take(&mut self.rows), ms: t0.elapsed().as_secs_f64() * 1000.0 });
        }
    }
}

/// A `Conn` that counts rows written, so the report can attribute them to Contoso modules.
struct CountingConn {
    inner: Box<dyn Conn>,
    c: Rc<RefCell<Counter>>,
}

impl Conn for CountingConn {
    fn get(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, lock: bool) -> Result<Option<Row>, StoreError> {
        self.inner.get(company, schema, key, lock)
    }
    fn scan(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Row>, StoreError> {
        self.inner.scan(company, schema, req)
    }
    fn count(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter]) -> Result<usize, StoreError> {
        self.inner.count(company, schema, filters)
    }
    fn aggregate(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter], col: usize, agg: Agg) -> Result<AggOut, StoreError> {
        self.inner.aggregate(company, schema, filters, col, agg)
    }
    fn insert(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        self.inner.insert(company, schema, row)?;
        self.c.borrow_mut().rows += 1;
        Ok(())
    }
    fn modify(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        self.inner.modify(company, schema, row)
    }
    fn delete(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row) -> Result<(), StoreError> {
        self.inner.delete(company, schema, key)?;
        self.c.borrow_mut().rows -= 1;
        Ok(())
    }
    fn lock_table(&mut self, company: &str, schema: &Arc<TableSchema>) -> Result<(), StoreError> {
        self.inner.lock_table(company, schema)
    }
    fn is_locked(&self, company: &str, table: i64) -> bool {
        self.inner.is_locked(company, table)
    }
    fn savepoint(&mut self) -> Result<usize, StoreError> {
        self.inner.savepoint()
    }
    fn rollback_to(&mut self, sp: usize) -> Result<(), StoreError> {
        self.inner.rollback_to(sp)
    }
    fn commit(&mut self) -> Result<(), StoreError> {
        self.inner.commit()
    }
    fn rollback(&mut self) {
        self.inner.rollback()
    }
    fn set_lock_timeout(&mut self, on: bool) {
        self.inner.set_lock_timeout(on)
    }
    fn lock_timeout(&self) -> bool {
        self.inner.lock_timeout()
    }
    fn set_lock_timeout_duration(&mut self, secs: i64) {
        self.inner.set_lock_timeout_duration(secs)
    }
    fn lock_timeout_duration(&self) -> i64 {
        self.inner.lock_timeout_duration()
    }
    fn in_write(&self) -> bool {
        self.inner.in_write()
    }
    fn seq(&mut self, op: SeqOp) -> Result<Option<i64>, StoreError> {
        self.inner.seq(op)
    }
    fn seq_names(&mut self) -> Result<Vec<String>, StoreError> {
        self.inner.seq_names()
    }
    fn batch_hint(&self) -> usize {
        self.inner.batch_hint()
    }
}

fn al_err(what: &str, e: Flow) -> anyhow::Error {
    match e {
        Flow::Err(e) => anyhow::anyhow!("{what}: {}\n  at {}", e.msg, e.stack.join("\n  at ")),
        _ => anyhow::anyhow!("{what}: unexpected control flow"),
    }
}

/// Run a parameterless procedure of a codeunit (a fresh instance, like `Codeunit.Method()` on a
/// local variable).
pub fn call_codeunit(s: &mut Session, id: i64, proc: &str) -> anyhow::Result<()> {
    let ci = *s.cat.code_by_key.get(&(ObjectKind::Codeunit, id)).ok_or_else(|| anyhow::anyhow!("codeunit {id} is not loaded"))?;
    let p = s.find_proc(ci, proc, 0).ok_or_else(|| anyhow::anyhow!("codeunit {id} has no procedure {proc}"))?;
    s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None).map_err(|e| al_err(&format!("{} {proc}", s.cat.code[ci as usize].name), e))?;
    Ok(())
}

/// The work date BC gives a session that opens this company: LogInManagement (codeunit 40)
/// GetDefaultWorkDate, which for a demo company is the posting date of the last G/L entry.
pub fn default_work_date(s: &mut Session) -> Option<i32> {
    let ci = *s.cat.code_by_key.get(&(ObjectKind::Codeunit, 40))?;
    let p = s.find_proc(ci, "GetDefaultWorkDate", 0)?;
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(al_runtime::Val::Date(d)) if d != 0 => Some(d),
        _ => None,
    }
}

/// Create the demo company in the session's company: platform rows (company, user, profiles),
/// codeunit 2 "Company-Initialize", then "Contoso Demo Tool".CreateAllDemoData (or
/// CreateSetupDemoData). Commits at the end. The catalog must include `demo_tool_dirs`.
pub fn create_demo_company(s: &mut Session, level: Level) -> anyhow::Result<DemoReport> {
    create_company(s, level, false)
}

/// al-test's Contoso company: `create_demo_company` with all demo data, then `test_company_seeds`
/// before the final commit. Never used for the live demo company (`demo-data`, `bc serve`).
pub fn create_test_company(s: &mut Session) -> anyhow::Result<DemoReport> {
    create_company(s, Level::All, true)
}

fn create_company(s: &mut Session, level: Level, test_seeds: bool) -> anyhow::Result<DemoReport> {
    if s.cat.code_by_key.get(&(ObjectKind::Codeunit, CONTOSO_DEMO_TOOL)).is_none() {
        anyhow::bail!("the Contoso Coffee Demo Dataset app is not loaded (load the catalog with demo::demo_app_dirs)");
    }
    let counter = Rc::new(RefCell::new(Counter::default()));
    let inner = std::mem::replace(&mut s.db, Box::new(NullConn));
    s.db = Box::new(CountingConn { inner, c: counter.clone() });
    // the counting wrapper stays installed for the rest of the session (a pass-through)
    let res = run_demo_tool(s, level, test_seeds, &counter);
    s.on_log = None;
    let (init_ms, contoso_ms, warnings) = res?;
    counter.borrow_mut().close();
    let phases = std::mem::take(&mut counter.borrow_mut().phases);
    let tables = table_counts(s.rt, &s.company);
    let total_rows = tables.iter().map(|t| t.rows).sum();
    Ok(DemoReport {
        company: s.company.clone(),
        backend: s.rt.db.describe(),
        init_ms,
        contoso_ms,
        steps: s.steps,
        phases,
        tables,
        total_rows,
        missing: std::mem::take(&mut s.missing),
        warnings,
    })
}

fn run_demo_tool(s: &mut Session, level: Level, test_seeds: bool, counter: &Rc<RefCell<Counter>>) -> anyhow::Result<(f64, f64, Vec<String>)> {
    let t0 = Instant::now();
    let mut warnings = vec![];
    s.init_platform_data().map_err(|e| al_err("platform data", e))?;
    s.commit()?;
    counter.borrow_mut().phase = Some(("Company-Initialize".into(), String::new(), Instant::now()));
    match s.run_codeunit(COMPANY_INITIALIZE, None) {
        Ok(()) => s.commit()?,
        // the demo tool creates its own setup data, so it can still run without this
        Err(e) => {
            s.rollback();
            let msg = al_err("Company-Initialize failed (rolled back, continuing)", e).to_string();
            eprintln!("warning: {msg}");
            warnings.push(msg);
            counter.borrow_mut().rows = 0;
        }
    }
    counter.borrow_mut().close();
    let init_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = Instant::now();
    // The tool logs 'Starting demo data module %1, for level %2' (0000OL6) and 'Finishing ...'
    // (0000OL7) around every module and level; use those markers to attribute rows.
    let c = counter.clone();
    s.on_log = Some(Box::new(move |event_id: &str, msg: &str| {
        let mut c = c.borrow_mut();
        match event_id {
            "0000OL6" => {
                c.close();
                // rows written between two modules (bookkeeping on "Contoso Demo Data Module") are not
                // attributed to the next module
                c.rows = 0;
                let rest = msg.trim_start_matches("Starting demo data module ");
                let (m, l) = rest.split_once(", for level ").unwrap_or((rest, ""));
                c.phase = Some((m.to_string(), l.to_string(), Instant::now()));
            }
            "0000OL7" => c.close(),
            _ => {}
        }
    }));
    {
        let mut cc = counter.borrow_mut();
        cc.phase = Some(("(tool)".into(), String::new(), Instant::now()));
    }
    let proc = match level {
        Level::All => "CreateAllDemoData",
        Level::Setup => "CreateSetupDemoData",
    };
    call_codeunit(s, CONTOSO_DEMO_TOOL, proc)?;
    if level == Level::All {
        legacy_demo_tool_setup(s)?;
    }
    if test_seeds {
        warnings.extend(test_company_seeds(s));
    }
    s.commit()?;
    Ok((init_ms, t1.elapsed().as_secs_f64() * 1000.0, warnings))
}

/// Setup that Microsoft's legacy W1 demo tool sets and Contoso leaves blank (vendor/bcapps
/// Layers/W1/DemoTool: "Create Sales & Receivables Setup".Finalize and "Create Purchases &
/// Payables Setup"): posted prepayment invoices and credit memos use the posted invoice and
/// credit memo number series. Microsoft's prepayment tests (ERM Prepayment*, about 350) need it:
/// documents get their "Prepayment No. Series" from these fields. Only blank fields are set.
/// Contoso only: it keeps Base App runs on the Contoso company (`--data contoso`) comparable; the
/// legacy W1 company (demo::w1) sets these itself.
fn legacy_demo_tool_setup(s: &mut Session) -> anyhow::Result<()> {
    const PAIRS: [(&str, &str); 2] = [("Posted Prepmt. Inv. Nos.", "Posted Invoice Nos."), ("Posted Prepmt. Cr. Memo Nos.", "Posted Credit Memo Nos.")];
    for table in ["Sales & Receivables Setup", "Purchases & Payables Setup"] {
        let Some(t) = s.cat.table_by_name(table) else { continue };
        let h = s.new_record(t.id, false);
        if !s.find_first(&h, false).map_err(|e| al_err(table, e))? {
            continue;
        }
        let mut changed = false;
        for (dst, src) in PAIRS {
            let (Some(di), Some(si)) = (t.field(dst), t.field(src)) else { continue };
            let blank = matches!(&h.borrow().row[di], bc_store::Value::Text(x) if x.is_empty());
            if blank {
                let v = h.borrow().row[si].clone();
                h.borrow_mut().row[di] = v;
                changed = true;
            }
        }
        if changed {
            s.rec_modify(&h, false).map_err(|e| al_err(table, e))?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
        }
    }
    // No warehouse employees, as in the legacy W1 company: Contoso makes the demo user a warehouse
    // employee of three locations, one of them as default, and Library - Warehouse
    // .CreateWarehouseEmployee(.., IsDefault = true) then finds that default and leaves the test's
    // own location without one ("Assigned User ID ... Warehouse Employee", 93 tests on Contoso, 0
    // on w1demo). Contoso's own "Warehouse Module".DeleteWarehouseEmployee removes them.
    call_codeunit(s, 5140, "DeleteWarehouseEmployee")?;
    // Inventory posting setup as complete as the legacy tool's ("Create Inventory Posting Setup"
    // sets Inventory Account (Interim) on every row, "Create WIP Accounts" a WIP Account on every
    // row): Contoso leaves FINISHED without an interim account and RESALE without a WIP account
    // (11 "Inventory Account (Interim)" and 9 "WIP Account" failures in expected-cost and output
    // posting tests, Contoso only). A blank one gets the account Contoso uses most for that field.
    const IPS: &str = "Inventory Posting Setup";
    if let Some(t) = s.cat.table_by_name(IPS) {
        let fields: Vec<usize> = ["Inventory Account (Interim)", "WIP Account"].iter().filter_map(|f| t.field(f)).collect();
        let mut rows = vec![];
        let h = s.new_record(t.id, false);
        if s.find_first(&h, false).map_err(|e| al_err(IPS, e))? {
            loop {
                rows.push(h.borrow().row.clone());
                if s.next(&h, 1).map_err(|e| al_err(IPS, e))? == 0 {
                    break;
                }
            }
        }
        for &fi in &fields {
            let mut uses: BTreeMap<String, usize> = BTreeMap::new();
            for r in &rows {
                if let bc_store::Value::Text(acc) = &r[fi] {
                    if !acc.is_empty() {
                        *uses.entry(acc.clone()).or_default() += 1;
                    }
                }
            }
            let Some((acc, _)) = uses.into_iter().max_by_key(|(_, n)| *n) else { continue };
            for r in &rows {
                if matches!(&r[fi], bc_store::Value::Text(x) if x.is_empty()) {
                    let h = s.new_record(t.id, false);
                    h.borrow_mut().row = r.clone();
                    if s.rec_get_pk(&h).is_some() {
                        let fresh = s.rec_get_pk(&h).unwrap();
                        h.borrow_mut().row = fresh.into();
                        h.borrow_mut().row[fi] = bc_store::Value::Text(acc.clone());
                        s.rec_modify(&h, false).map_err(|e| al_err(IPS, e))?.map_err(|e| anyhow::anyhow!("{IPS}: {e}"))?;
                    }
                }
            }
        }
    }
    // Journal templates, a budget and a number series that the legacy tool creates and Contoso
    // doesn't (the Contoso-only failure causes of I8: "There is no Res. Journal Template / Warehouse
    // Journal Template / Cost Journal Template / G/L Budget Name within the filter", "Internal
    // Movement Nos. must have a value"; about 130 tests). Values as in the legacy codeunits Create
    // Res. Journal Template, Create Whse. Journal Template, Create Cost Acct. Jnl. Template, Create
    // G/L Budget Name and Create Inventory Setup; existing rows are left alone.
    legacy_templates(s)?;
    legacy_prepayment_accounts(s)?;
    posting_setup_grid(s)?;
    inventory_setup_grid(s)?;
    if let Err(e) = legacy_setup_more(s) {
        eprintln!("warning: more legacy setup for Contoso: {e}");
    }
    if let Err(e) = legacy_item_templates(s) {
        eprintln!("warning: legacy item journal templates for Contoso: {e}");
    }
    item_journal_series(s)?;
    if let Err(e) = drop_job_table_defaults(s) {
        eprintln!("warning: table-level project default dimensions on Contoso: {e}");
    }
    if let Err(e) = legacy_service(s) {
        eprintln!("warning: legacy service data for Contoso: {e}");
    }
    // Cost types from the chart of accounts, as the legacy tool's "Create Cost Types" does first
    // (Cost Account Mgt.GetCostTypesFromChartDirect): Contoso has none, and the cost accounting
    // tests stop at "There are no records within the filters specified for table Cost Type".
    if let Err(e) = call_codeunit(s, 1100, "GetCostTypesFromChartDirect") {
        eprintln!("warning: cost types from the chart of accounts: {e}");
    }
    // Warehouse worksheet templates as the legacy tool's "Create Whse. Wksh.-Template" (codeunit
    // 118829) makes them: PICK, PUT-AWAY and MOVEMENT. Contoso creates none, and Library -
    // Warehouse.SelectWhseWorksheetTemplate takes the first template of a type (251 Contoso tests
    // failed with "There is no Whse. Worksheet Template within the filter"). Only types that have
    // no template get one.
    const WKSH: &str = "Whse. Worksheet Template";
    if let Some(t) = s.cat.table_by_name(WKSH) {
        let (Some(name_fi), Some(desc_fi), Some(type_fi)) = (t.field("Name"), t.field("Description"), t.field("Type")) else { return Ok(()) };
        let mut have = vec![];
        let h = s.new_record(t.id, false);
        if s.find_first(&h, false).map_err(|e| al_err(WKSH, e))? {
            loop {
                have.push(h.borrow().row[type_fi].clone());
                if s.next(&h, 1).map_err(|e| al_err(WKSH, e))? == 0 {
                    break;
                }
            }
        }
        for (name, desc, ty) in [("PICK", "Pick Worksheet", "Pick"), ("PUT-AWAY", "Put-away Worksheet", "Put-away"), ("MOVEMENT", "Movement Worksheet", "Movement")] {
            let tyv = s.enum_value("Warehouse Worksheet Template Type", ty).map_err(|e| al_err(WKSH, e))?;
            let al_runtime::Val::Opt { ord, .. } = tyv else { continue };
            if have.contains(&bc_store::Value::Option(ord)) {
                continue;
            }
            let h = s.new_record(t.id, false);
            s.rec_validate(&h, name_fi, Some(al_runtime::Val::text(name)), 0).map_err(|e| al_err(WKSH, e))?;
            s.rec_validate(&h, desc_fi, Some(al_runtime::Val::text(desc)), 0).map_err(|e| al_err(WKSH, e))?;
            s.rec_insert(&h, true).map_err(|e| al_err(WKSH, e))?.map_err(|e| anyhow::anyhow!("{WKSH}: {e}"))?;
            s.rec_validate(&h, type_fi, Some(tyv), 0).map_err(|e| al_err(WKSH, e))?;
            s.rec_modify(&h, false).map_err(|e| al_err(WKSH, e))?.map_err(|e| anyhow::anyhow!("{WKSH}: {e}"))?;
        }
    }
    Ok(())
}

/// Setup for al-test's Contoso company only (`create_test_company`; the live demo company never
/// gets it). The legacy W1 company that Microsoft's CI runs the Base App tests on has these values
/// and Contoso doesn't. I14 S5 proved them on fresh Contoso companies, with a no-seed control on the
/// same binary (data/parity/full-baseapp-15/notes.md, "Contoso-only failures"; I15 L1 slice A/B,
/// not in the repository).
/// - DEFAULT warehouse worksheet names as the legacy "Create Whse. Wksh.-Name" (118830) inserts
///   them, for its locations that exist ("There is no Whse. Worksheet Name within the filter":
///   SCM Pick Worksheet, 11 tests);
/// - no additional reporting currency (Contoso sets EUR; 8);
/// - blank Inventory and General Posting Setup accounts get the account used most in that column
///   (Material Variance, Overhead Applied, ... are blank in Contoso; 7);
/// - Inventory Setup."Automatic Cost Posting" = No, the legacy company's value (Contoso: Yes; 10).
/// Not seeded: the legacy cost accounting setup (cost center dimension = global dimension 1). With
/// Contoso's dimension data, new DEPARTMENT values then ask to extend a default dimension's Allowed
/// Values Filter, an unhandled Confirm in 4 ERM Cost Accounting Setup tests that pass on W1.
/// A step that fails is reported (returned and printed) and the others still run.
fn test_company_seeds(s: &mut Session) -> Vec<String> {
    let steps: [(&str, fn(&mut Session) -> anyhow::Result<String>); 4] = [
        ("warehouse worksheet names", seed_whse_worksheet_names),
        ("additional reporting currency", seed_no_reporting_currency),
        ("posting setup accounts", seed_posting_setup_accounts),
        ("automatic cost posting", seed_no_automatic_cost_posting),
    ];
    let mut warnings = vec![];
    for (what, step) in steps {
        match step(s) {
            Ok(done) => eprintln!("test company seed, {what}: {done}"),
            Err(e) => {
                let msg = format!("test company seed, {what}: {e:#}");
                eprintln!("warning: {msg}");
                warnings.push(msg);
            }
        }
    }
    warnings
}

/// Set one field of the first row of a setup table without validation; returns "old -> new".
fn set_setup_field(s: &mut Session, table: &str, field: &str, v: bc_store::Value) -> anyhow::Result<String> {
    let t = s.cat.table_by_name(table).ok_or_else(|| anyhow::anyhow!("no table {table}"))?;
    let fi = t.field(field).ok_or_else(|| anyhow::anyhow!("no field {table}.{field}"))?;
    let h = s.new_record(t.id, false);
    if !s.find_first(&h, false).map_err(|e| al_err(table, e))? {
        anyhow::bail!("{table} has no row");
    }
    let old = h.borrow().row[fi].clone();
    h.borrow_mut().row[fi] = v.clone();
    s.rec_modify(&h, false).map_err(|e| al_err(table, e))?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
    Ok(format!("{field} {old:?} -> {v:?}"))
}

/// All rows of a table, in primary key order.
fn table_rows(s: &mut Session, table: &str) -> anyhow::Result<Vec<Row>> {
    let mut out = vec![];
    let Some(t) = s.cat.table_by_name(table) else { return Ok(out) };
    let h = s.new_record(t.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(table, e))? {
        loop {
            out.push(h.borrow().row.to_vec().into());
            if s.next(&h, 1).map_err(|e| al_err(table, e))? == 0 {
                break;
            }
        }
    }
    Ok(out)
}

fn seed_whse_worksheet_names(s: &mut Session) -> anyhow::Result<String> {
    use al_runtime::Val;
    const NAMES: [(&str, &str); 9] = [
        ("PUT-AWAY", "YELLOW"),
        ("PUT-AWAY", "GREEN"),
        ("PUT-AWAY", "WHITE"),
        ("PUT-AWAY", "SILVER"),
        ("PICK", "YELLOW"),
        ("PICK", "GREEN"),
        ("PICK", "WHITE"),
        ("PICK", "SILVER"),
        ("MOVEMENT", "WHITE"),
    ];
    let locations: Vec<bc_store::Value> = table_rows(s, "Location")?.into_iter().map(|r| r[0].clone()).collect();
    let (mut inserted, mut failed) = (0, vec![]);
    for (template, location) in NAMES {
        if !locations.contains(&bc_store::Value::Text(location.into())) {
            continue;
        }
        let key = [("Worksheet Template Name", Val::text(template)), ("Name", Val::text("DEFAULT")), ("Location Code", Val::text(location))];
        match insert_setup_row(s, "Whse. Worksheet Name", &key, &[("Description", Val::text("Default Journal"))]) {
            Ok(true) => inserted += 1,
            Ok(false) => {}
            Err(e) => failed.push(format!("{template} at {location}: {e}")),
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("{inserted} inserted, failed: {}", failed.join("; "));
    }
    Ok(format!("{inserted} inserted"))
}

fn seed_no_reporting_currency(s: &mut Session) -> anyhow::Result<String> {
    set_setup_field(s, "General Ledger Setup", "Additional Reporting Currency", bc_store::Value::Text(String::new()))
}

fn seed_no_automatic_cost_posting(s: &mut Session) -> anyhow::Result<String> {
    set_setup_field(s, "Inventory Setup", "Automatic Cost Posting", bc_store::Value::Bool(false))
}

fn seed_posting_setup_accounts(s: &mut Session) -> anyhow::Result<String> {
    let inv = fill_blank_accounts(s, "Inventory Posting Setup", &["Material Variance Account", "Capacity Variance Account", "Mfg. Overhead Variance Account", "Cap. Overhead Variance Account", "Subcontracted Variance Account"])?;
    let gen = fill_blank_accounts(
        s,
        "General Posting Setup",
        &["Overhead Applied Account", "Purchase Variance Account", "Direct Cost Applied Account", "Invt. Accrual Acc. (Interim)", "COGS Account (Interim)", "Inventory Adjmt. Account", "COGS Account"],
    )?;
    Ok(format!("{inv} inventory and {gen} general posting setup accounts filled"))
}

/// Blank account fields of every row of a posting setup table get the account used most in that
/// field (no validation, as the legacy tool's direct assignments). Returns the number filled.
fn fill_blank_accounts(s: &mut Session, table: &str, fields: &[&str]) -> anyhow::Result<usize> {
    let t = s.cat.table_by_name(table).ok_or_else(|| anyhow::anyhow!("no table {table}"))?;
    let mut filled = 0;
    for f in fields {
        let fi = t.field(f).ok_or_else(|| anyhow::anyhow!("no field {table}.{f}"))?;
        let rows = table_rows(s, table)?;
        let mut uses: BTreeMap<String, usize> = BTreeMap::new();
        for r in &rows {
            if let bc_store::Value::Text(a) = &r[fi] {
                if !a.is_empty() {
                    *uses.entry(a.clone()).or_default() += 1;
                }
            }
        }
        // ties: the last of them in key order (deterministic; what S5 proved)
        let Some((most, _)) = uses.into_iter().max_by_key(|(_, n)| *n) else { continue };
        for r in rows {
            if !matches!(&r[fi], bc_store::Value::Text(x) if x.is_empty()) {
                continue;
            }
            let h = s.new_record(t.id, false);
            h.borrow_mut().row = r.into();
            let Some(fresh) = s.rec_get_pk(&h) else { continue };
            h.borrow_mut().row = fresh.into();
            h.borrow_mut().row[fi] = bc_store::Value::Text(most.clone());
            s.rec_modify(&h, false).map_err(|e| al_err(table, e))?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
            filled += 1;
        }
    }
    Ok(filled)
}

/// A field value of the first row of a setup table (Text; empty when missing).
fn setup_text(s: &mut Session, table: &str, field: &str) -> String {
    let Some(t) = s.cat.table_by_name(table) else { return String::new() };
    let Some(fi) = t.field(field) else { return String::new() };
    let h = s.new_record(t.id, false);
    if !s.find_first(&h, false).unwrap_or(false) {
        return String::new();
    }
    let v = h.borrow().row[fi].clone();
    match v {
        bc_store::Value::Text(x) => x,
        _ => String::new(),
    }
}

/// Insert a row of `table` unless one with the same primary key exists: the key fields are set
/// first, then the other fields are validated (as the legacy tool's InsertData does), then
/// Insert(true). Returns whether it inserted.
fn insert_setup_row(s: &mut Session, table: &str, key: &[(&str, al_runtime::Val)], fields: &[(&str, al_runtime::Val)]) -> anyhow::Result<bool> {
    let Some(t) = s.cat.table_by_name(table) else { return Ok(false) };
    let h = s.new_record(t.id, false);
    for (f, v) in key {
        let Some(fi) = t.field(f) else { return Ok(false) };
        s.rec_validate(&h, fi, Some(v.clone()), 0).map_err(|e| al_err(table, e))?;
    }
    if s.rec_get_pk(&h).is_some() {
        return Ok(false);
    }
    s.rec_insert(&h, true).map_err(|e| al_err(table, e))?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
    let mut changed = false;
    for (f, v) in fields {
        if let Some(fi) = t.field(f) {
            s.rec_validate(&h, fi, Some(v.clone()), 0).map_err(|e| al_err(&format!("{table}.{f}"), e))?;
            changed = true;
        }
    }
    if changed {
        s.rec_modify(&h, true).map_err(|e| al_err(table, e))?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
    }
    Ok(true)
}

fn legacy_templates(s: &mut Session) -> anyhow::Result<()> {
    use al_runtime::Val;
    let text = |x: &str| Val::text(x);
    // Res. Journal Template (Create Res. Journal Template)
    let res_src = setup_text(s, "Source Code Setup", "Resource Journal");
    insert_setup_row(s, "Res. Journal Template", &[("Name", text("RESOURCES"))], &[("Description", text("Resource Journal")), ("Recurring", Val::Bool(false)), ("Source Code", text(&res_src))])?;
    insert_setup_row(s, "Res. Journal Template", &[("Name", text("RECURRING"))], &[("Description", text("Recurring Resource Journal")), ("Recurring", Val::Bool(true)), ("Source Code", text(&res_src))])?;
    // Warehouse Journal Template (Create Whse. Journal Template)
    for (name, desc, ty, src_field) in [
        ("ADJMT", "Adjustment Journal", "Item", "Whse. Item Journal"),
        ("RECLASS", "Reclassification Journal", "Reclassification", "Whse. Reclassification Journal"),
        ("PHYSINVT", "Physical Inventory Journal", "Physical Inventory", "Whse. Phys. Invt. Journal"),
    ] {
        let src = setup_text(s, "Source Code Setup", src_field);
        let tyv = s.enum_value("Warehouse Journal Template Type", ty).map_err(|e| al_err("Warehouse Journal Template Type", e))?;
        insert_setup_row(s, "Warehouse Journal Template", &[("Name", text(name))], &[("Description", text(desc)), ("Type", tyv), ("Source Code", text(&src))])?;
    }
    // Cost Journal Template (Create Cost Acct. Jnl. Template)
    let cost_register = s.cat.code.iter().find(|c| c.kind == al_syntax::ObjectKind::Report && c.name == "Cost Register").map(|c| c.id);
    let mut cost_fields = vec![("Description", text("Standard Journal"))];
    if let Some(id) = cost_register {
        cost_fields.push(("Posting Report ID", Val::Int(id as i32)));
    }
    insert_setup_row(s, "Cost Journal Template", &[("Name", text("COSTACCT"))], &cost_fields)?;
    // G/L Budget Name for the work date's year (Create G/L Budget Name)
    {
        let year = al_runtime::value::date_to_naive(s.work_date).map(|d| chrono::Datelike::year(&d)).unwrap_or_else(|| chrono::Datelike::year(&chrono::Utc::now()));
        insert_setup_row(s, "G/L Budget Name", &[("Name", text(&year.to_string()))], &[("Description", text(&format!("{year} budget")))])?;
    }
    // Internal Movement Nos. (Create Inventory Setup): series INT-MOVE, INTM000001..INTM999999
    if setup_text(s, "Inventory Setup", "Internal Movement Nos.").is_empty() {
        insert_setup_row(s, "No. Series", &[("Code", text("INT-MOVE"))], &[("Description", text("Internal Movement")), ("Default Nos.", Val::Bool(true))])?;
        insert_setup_row(s, "No. Series Line", &[("Series Code", text("INT-MOVE")), ("Line No.", Val::Int(10000))], &[("Starting No.", text("INTM000001")), ("Ending No.", text("INTM999999")), ("Increment-by No.", Val::Int(1))])?;
        if let Some(t) = s.cat.table_by_name("Inventory Setup") {
            if let Some(fi) = t.field("Internal Movement Nos.") {
                let h = s.new_record(t.id, false);
                if s.find_first(&h, false).map_err(|e| al_err("Inventory Setup", e))? {
                    s.rec_validate(&h, fi, Some(text("INT-MOVE")), 0).map_err(|e| al_err("Inventory Setup", e))?;
                    s.rec_modify(&h, false).map_err(|e| al_err("Inventory Setup", e))?.map_err(|e| anyhow::anyhow!("Inventory Setup: {e}"))?;
                }
            }
        }
    }
    Ok(())
}

/// Service costs and a service item, which the legacy tool creates (Create Service Cost, Create
/// Service Item) and Contoso doesn't: Library - Service.FindServiceCost needs a cost with an account
/// ("There is no Service Cost within the filter", 23 tests), and service tests need a service item
/// of a customer and an item that are not blocked (22 tests). The costs take the sales account of
/// the DOMESTIC/SERVICES posting setup (the legacy tool's 6810 is a service income account); the
/// legacy travel costs are per service zone, which Contoso doesn't have.
fn legacy_service(s: &mut Session) -> anyhow::Result<()> {
    use al_runtime::Val;
    let Some(gps) = s.cat.table_by_name("General Posting Setup") else { return Ok(()) };
    let account = {
        let h = s.new_record(gps.id, false);
        let (Some(b), Some(p), Some(acc)) = (gps.field("Gen. Bus. Posting Group"), gps.field("Gen. Prod. Posting Group"), gps.field("Sales Account")) else { return Ok(()) };
        h.borrow_mut().row[b] = bc_store::Value::Text("DOMESTIC".into());
        h.borrow_mut().row[p] = bc_store::Value::Text("SERVICES".into());
        match s.rec_get_pk(&h) {
            Some(row) => match &row[acc] {
                bc_store::Value::Text(x) => x.clone(),
                _ => String::new(),
            },
            None => String::new(),
        }
    };
    if !account.is_empty() {
        let other = s.enum_value("Service Cost Type", "Other").map_err(|e| al_err("Service Cost Type", e))?;
        let travel = s.enum_value("Service Cost Type", "Travel").map_err(|e| al_err("Service Cost Type", e))?;
        for (code, desc, price, ty) in [("START", "Start Fee", 20, other), ("TRAVEL", "Travel Fee", 2, travel)] {
            insert_setup_row(s, "Service Cost", &[("Code", Val::text(code))], &[("Description", Val::text(desc)), ("Account No.", Val::text(&account)), ("Default Unit Price", Val::Int(price)), ("Default Quantity", Val::Int(1)), ("Cost Type", ty)])?;
        }
    }
    // one service item, of the first customer and inventory item that are not blocked
    let (Some(si), Some(cust), Some(item)) = (s.cat.table_by_name("Service Item"), s.cat.table_by_name("Customer"), s.cat.table_by_name("Item")) else { return Ok(()) };
    let any_service_item = {
        let h = s.new_record(si.id, false);
        s.find_first(&h, false).map_err(|e| al_err("Service Item", e))?
    };
    if any_service_item {
        return Ok(());
    }
    let first_unblocked = |s: &mut Session, t: &'static al_model::TableDef, blocked_ok: &dyn Fn(&bc_store::Row) -> bool| -> anyhow::Result<Option<String>> {
        let h = s.new_record(t.id, false);
        let no = t.field("No.").unwrap();
        if s.find_first(&h, false).map_err(|e| al_err(&t.name, e))? {
            loop {
                let row = h.borrow().row.clone();
                if blocked_ok(&row) {
                    if let bc_store::Value::Text(x) = &row[no] {
                        return Ok(Some(x.clone()));
                    }
                }
                if s.next(&h, 1).map_err(|e| al_err(&t.name, e))? == 0 {
                    return Ok(None);
                }
            }
        }
        Ok(None)
    };
    let cb = cust.field("Blocked");
    let customer = first_unblocked(s, cust, &|r| cb.is_none_or(|f| matches!(r[f], bc_store::Value::Option(0))))?;
    let (ib, it) = (item.field("Blocked"), item.field("Type"));
    let item_no = first_unblocked(s, item, &|r| ib.is_none_or(|f| matches!(r[f], bc_store::Value::Bool(false))) && it.is_none_or(|f| matches!(r[f], bc_store::Value::Option(0))))?;
    if let (Some(c), Some(i)) = (customer, item_no) {
        let h = s.new_record(si.id, false);
        s.rec_insert(&h, true).map_err(|e| al_err("Service Item", e))?.map_err(|e| anyhow::anyhow!("Service Item: {e}"))?;
        for (f, v) in [("Item No.", Val::text(&i)), ("Customer No.", Val::text(&c))] {
            if let Some(fi) = si.field(f) {
                s.rec_validate(&h, fi, Some(v), 0).map_err(|e| al_err(&format!("Service Item.{f}"), e))?;
            }
        }
        s.rec_modify(&h, true).map_err(|e| al_err("Service Item", e))?.map_err(|e| anyhow::anyhow!("Service Item: {e}"))?;
    }
    Ok(())
}

/// Every item journal template with a document number series, as in the legacy company (Create
/// Item Journal Template gives each one): Contoso's service, project and manufacturing modules add
/// Item-type templates without one, the test libraries take the first template of a type, and the
/// batches they create then have no series ("Enter the document no." in Calculate Whse.
/// Adjustment, 32 tests). A template without one takes the series of a template of its type.
fn item_journal_series(s: &mut Session) -> anyhow::Result<()> {
    const IJT: &str = "Item Journal Template";
    let Some(t) = s.cat.table_by_name(IJT) else { return Ok(()) };
    let (Some(ty), Some(ns)) = (t.field("Type"), t.field("No. Series")) else { return Ok(()) };
    let mut rows = vec![];
    let h = s.new_record(t.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(IJT, e))? {
        loop {
            rows.push(h.borrow().row.clone());
            if s.next(&h, 1).map_err(|e| al_err(IJT, e))? == 0 {
                break;
            }
        }
    }
    let series = |r: &bc_store::Row| match &r[ns] {
        bc_store::Value::Text(x) => x.clone(),
        _ => String::new(),
    };
    for r in &rows {
        if !series(r).is_empty() {
            continue;
        }
        let Some(model) = rows.iter().find(|m| m[ty] == r[ty] && !series(m).is_empty()).map(|m| series(m)) else { continue };
        let h = s.new_record(t.id, false);
        h.borrow_mut().row = r.clone().into();
        if let Some(fresh) = s.rec_get_pk(&h) {
            h.borrow_mut().row = fresh.into();
            h.borrow_mut().row[ns] = bc_store::Value::Text(model);
            s.rec_modify(&h, false).map_err(|e| al_err(IJT, e))?.map_err(|e| anyhow::anyhow!("{IJT}: {e}"))?;
        }
    }
    // and every batch without a series takes its template's (Contoso makes its DEFAULT batches
    // with an explicit blank series; the libraries reuse them)
    const IJB: &str = "Item Journal Batch";
    let Some(bt) = s.cat.table_by_name(IJB) else { return Ok(()) };
    let (Some(tn), Some(bns)) = (bt.field("Journal Template Name"), bt.field("No. Series")) else { return Ok(()) };
    let Some(tname) = t.field("Name") else { return Ok(()) };
    let mut template_series: BTreeMap<String, String> = BTreeMap::new();
    let h = s.new_record(t.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(IJT, e))? {
        loop {
            let (n, ser) = {
                let r = h.borrow();
                (match &r.row[tname] { bc_store::Value::Text(x) => x.clone(), _ => String::new() }, series(&r.row))
            };
            template_series.insert(n, ser);
            if s.next(&h, 1).map_err(|e| al_err(IJT, e))? == 0 {
                break;
            }
        }
    }
    let mut batches = vec![];
    let h = s.new_record(bt.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(IJB, e))? {
        loop {
            batches.push((*h.borrow().row).clone());
            if s.next(&h, 1).map_err(|e| al_err(IJB, e))? == 0 {
                break;
            }
        }
    }
    for b in batches {
        let blank = matches!(&b[bns], bc_store::Value::Text(x) if x.is_empty());
        let tpl = match &b[tn] { bc_store::Value::Text(x) => x.clone(), _ => continue };
        let Some(ser) = template_series.get(&tpl).filter(|x| !x.is_empty()).cloned() else { continue };
        if !blank {
            continue;
        }
        let h = s.new_record(bt.id, false);
        h.borrow_mut().row = b.into();
        h.borrow_mut().row[bns] = bc_store::Value::Text(ser);
        s.rec_modify(&h, false).map_err(|e| al_err(IJB, e))?.map_err(|e| anyhow::anyhow!("{IJB}: {e}"))?;
    }
    Ok(())
}

/// Prepayment accounts on the domestic general posting setups, as the legacy tool gives them
/// (Create General Posting Setup.UpdatePrepmtAccounts: MISC, RAW MAT and RETAIL take the 25 %
/// accounts, NO VAT the 0 % ones, SERVICES the 10 % ones). Contoso has these accounts (Customer /
/// Vendor Prepayments VAT 0 %, 10 %, 25 %, with the matching product posting groups) but no setup
/// uses them, so Library - ERM.FindGeneralPostingSetupInvtFull finds no full setup and gives one
/// new G/L accounts without a VAT product group: the prepayment tests then stop at "The VAT
/// Posting Setup does not exist ... 'DOMESTIC',''" (ERM Prepayment III/V and others, Contoso
/// only). A setup takes the account whose product posting group is its own, else the 25 % one.
fn legacy_prepayment_accounts(s: &mut Session) -> anyhow::Result<()> {
    const GLA: &str = "G/L Account";
    const GPS: &str = "General Posting Setup";
    let text = |v: &bc_store::Value| match v {
        bc_store::Value::Text(x) => x.clone(),
        _ => String::new(),
    };
    let Some(ga) = s.cat.table_by_name(GLA) else { return Ok(()) };
    let (Some(no), Some(name), Some(gp)) = (ga.field("No."), ga.field("Name"), ga.field("Gen. Prod. Posting Group")) else { return Ok(()) };
    // (customer?, gen. prod. group, account no., is the 25 % one)
    let mut accounts: Vec<(bool, String, String, bool)> = vec![];
    let h = s.new_record(ga.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(GLA, e))? {
        loop {
            let (n, nm, g) = {
                let r = h.borrow();
                (text(&r.row[no]), text(&r.row[name]), text(&r.row[gp]))
            };
            for (prefix, customer) in [("Customer Prepayments VAT ", true), ("Vendor Prepayments VAT ", false)] {
                if let Some(rest) = nm.strip_prefix(prefix) {
                    accounts.push((customer, g.clone(), n.clone(), rest.starts_with("25")));
                }
            }
            if s.next(&h, 1).map_err(|e| al_err(GLA, e))? == 0 {
                break;
            }
        }
    }
    if accounts.is_empty() {
        return Ok(());
    }
    let pick = |customer: bool, prod: &str| -> Option<String> {
        let mine = accounts.iter().filter(|a| a.0 == customer);
        mine.clone().find(|a| a.1 == prod).or_else(|| mine.clone().find(|a| a.3)).map(|a| a.2.clone())
    };
    let Some(gt) = s.cat.table_by_name(GPS) else { return Ok(()) };
    let (Some(bus), Some(prod), Some(sp), Some(pp)) = (gt.field("Gen. Bus. Posting Group"), gt.field("Gen. Prod. Posting Group"), gt.field("Sales Prepayments Account"), gt.field("Purch. Prepayments Account")) else { return Ok(()) };
    let mut rows = vec![];
    let h = s.new_record(gt.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(GPS, e))? {
        loop {
            rows.push((*h.borrow().row).clone());
            if s.next(&h, 1).map_err(|e| al_err(GPS, e))? == 0 {
                break;
            }
        }
    }
    for mut r in rows {
        if text(&r[bus]) != "DOMESTIC" {
            continue;
        }
        let p = text(&r[prod]);
        let mut changed = false;
        for (fi, customer) in [(sp, true), (pp, false)] {
            if text(&r[fi]).is_empty() {
                if let Some(acc) = pick(customer, &p) {
                    r[fi] = bc_store::Value::Text(acc);
                    changed = true;
                }
            }
        }
        if changed {
            let h = s.new_record(gt.id, false);
            h.borrow_mut().row = r.into();
            s.rec_modify(&h, false).map_err(|e| al_err(GPS, e))?.map_err(|e| anyhow::anyhow!("{GPS}: {e}"))?;
        }
    }
    Ok(())
}

/// A General Posting Setup row for every business x product posting group, as the legacy W1
/// company has ("Create General Posting Setup": '', DOMESTIC, EU and EXPORT x every product
/// group). Contoso has a FREIGHT product group with no rows at all (it sorts first, so tests that
/// take the first product group get it) and no EXPORT x MANUFACT row: "The General Posting Setup
/// does not exist" (81 CI-run tests fail on Contoso and pass on W1 in I10). A missing row is a
/// copy of the business group's MISC row, else its RETAIL row, else its first row.
fn posting_setup_grid(s: &mut Session) -> anyhow::Result<()> {
    const GPS: &str = "General Posting Setup";
    let text = |v: &bc_store::Value| match v {
        bc_store::Value::Text(x) => x.clone(),
        _ => String::new(),
    };
    let Some(t) = s.cat.table_by_name(GPS) else { return Ok(()) };
    let (Some(bus), Some(prod)) = (t.field("Gen. Bus. Posting Group"), t.field("Gen. Prod. Posting Group")) else { return Ok(()) };
    let mut rows = vec![];
    let h = s.new_record(t.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(GPS, e))? {
        loop {
            rows.push((*h.borrow().row).clone());
            if s.next(&h, 1).map_err(|e| al_err(GPS, e))? == 0 {
                break;
            }
        }
    }
    // the product groups: the Gen. Product Posting Group table
    let mut prods = vec![];
    if let Some(gp) = s.cat.table_by_name("Gen. Product Posting Group") {
        if let Some(code) = gp.field("Code") {
            let h = s.new_record(gp.id, false);
            if s.find_first(&h, false).map_err(|e| al_err("Gen. Product Posting Group", e))? {
                loop {
                    prods.push(text(&h.borrow().row[code]));
                    if s.next(&h, 1).map_err(|e| al_err("Gen. Product Posting Group", e))? == 0 {
                        break;
                    }
                }
            }
        }
    }
    let mut buses: Vec<String> = rows.iter().map(|r| text(&r[bus])).collect();
    buses.sort();
    buses.dedup();
    for b in &buses {
        let mine: Vec<&bc_store::Row> = rows.iter().filter(|r| text(&r[bus]) == *b).collect();
        let Some(model) = ["MISC", "RETAIL"].iter().find_map(|m| mine.iter().find(|r| text(&r[prod]) == *m)).or(mine.first()).map(|r| (*r).clone()) else { continue };
        for pg in &prods {
            if mine.iter().any(|r| text(&r[prod]) == *pg) {
                continue;
            }
            let mut row = model.clone();
            row[prod] = bc_store::Value::Text(pg.clone());
            let h = s.new_record(t.id, false);
            h.borrow_mut().row = row.into();
            s.rec_insert(&h, false).map_err(|e| al_err(GPS, e))?.map_err(|e| anyhow::anyhow!("{GPS}: {e}"))?;
        }
    }
    Ok(())
}

/// An Inventory Posting Setup row for every storage location x inventory posting group that has
/// rows, as the legacy tool gives its storage locations ("Create Inventory Posting Setup": BLUE,
/// GREEN, RED, SILVER, WHITE, YELLOW x FINISHED, RAW MAT, RESALE). Contoso's WHITE and EAST have no
/// FINISHED row: "The Inventory Posting Setup does not exist" (48 CI-run tests fail on Contoso and
/// pass on W1 in I10, mostly service orders). In-transit locations are left as they are. A missing
/// row is a copy of the group's blank-location row, else of its first row.
fn inventory_setup_grid(s: &mut Session) -> anyhow::Result<()> {
    const IPS: &str = "Inventory Posting Setup";
    let text = |v: &bc_store::Value| match v {
        bc_store::Value::Text(x) => x.clone(),
        _ => String::new(),
    };
    let Some(t) = s.cat.table_by_name(IPS) else { return Ok(()) };
    let (Some(loc), Some(grp)) = (t.field("Location Code"), t.field("Invt. Posting Group Code")) else { return Ok(()) };
    let mut rows = vec![];
    let h = s.new_record(t.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(IPS, e))? {
        loop {
            rows.push((*h.borrow().row).clone());
            if s.next(&h, 1).map_err(|e| al_err(IPS, e))? == 0 {
                break;
            }
        }
    }
    let mut locations = vec![];
    if let Some(lt) = s.cat.table_by_name("Location") {
        if let (Some(code), Some(transit)) = (lt.field("Code"), lt.field("Use As In-Transit")) {
            let h = s.new_record(lt.id, false);
            if s.find_first(&h, false).map_err(|e| al_err("Location", e))? {
                loop {
                    let (c, it) = {
                        let r = h.borrow();
                        (text(&r.row[code]), matches!(r.row[transit], bc_store::Value::Bool(true)))
                    };
                    if !it {
                        locations.push(c);
                    }
                    if s.next(&h, 1).map_err(|e| al_err("Location", e))? == 0 {
                        break;
                    }
                }
            }
        }
    }
    let mut groups: Vec<String> = rows.iter().map(|r| text(&r[grp])).collect();
    groups.sort();
    groups.dedup();
    for g in &groups {
        let Some(model) = rows.iter().find(|r| text(&r[grp]) == *g && text(&r[loc]).is_empty()).or_else(|| rows.iter().find(|r| text(&r[grp]) == *g)).cloned() else { continue };
        for l in &locations {
            if rows.iter().any(|r| text(&r[grp]) == *g && text(&r[loc]) == *l) {
                continue;
            }
            let mut row = model.clone();
            row[loc] = bc_store::Value::Text(l.clone());
            let h = s.new_record(t.id, false);
            h.borrow_mut().row = row.into();
            s.rec_insert(&h, false).map_err(|e| al_err(IPS, e))?.map_err(|e| anyhow::anyhow!("{IPS}: {e}"))?;
        }
    }
    Ok(())
}

/// More setup the legacy W1 tool creates and Contoso lacks (the top Contoso-only failure groups of
/// I11's full run, CI-run tests that pass on W1):
/// - Phys. Invt. Counting Periods FAST, NORMAL and SLOW ("Create Physical Inventory"; "The Phys.
///   Invt. Counting Period table is empty", 14 tests);
/// - the SM-QUOTE number series for Service Mgt. Setup."Service Quote Nos." ("Create Service Mgt.
///   Setup", SQ000001; "Service Quote Nos. must have a value", 12);
/// - item budget names DEFAULT for sales (by customer group) and purchases ("Create Item Budget
///   Name"; "There is no Item Budget Name within the filter", 11);
/// - a Purchase Variance Account on every general posting setup row ("Create WIP Accounts" sets
///   one per product group; Contoso's MISC has none: 11), the account Contoso uses most for it;
/// - the DEFAULT batch of the RESOURCES resource journal template ("Create Res. Journal Batch";
///   "There is no Res. Journal Batch within the filter", 10).
fn legacy_setup_more(s: &mut Session) -> anyhow::Result<()> {
    for (what, step) in [("counting periods", more_counting_periods as fn(&mut Session) -> anyhow::Result<()>), ("service quote series", more_service_quotes), ("item budget names", more_item_budgets), ("purchase variance accounts", more_purchase_variance), ("resource journal batch", more_res_batch)] {
        if let Err(e) = step(s) {
            eprintln!("warning: legacy setup for Contoso, {what}: {e}");
        }
    }
    Ok(())
}

fn more_counting_periods(s: &mut Session) -> anyhow::Result<()> {
    use al_runtime::Val;
    let text = |x: &str| Val::text(x);
    for (code, desc, freq) in [("FAST", "Fast movers or High value", 6), ("NORMAL", "Normal sale or medium value", 2), ("SLOW", "Slow movers or low value", 1)] {
        insert_setup_row(s, "Phys. Invt. Counting Period", &[("Code", text(code))], &[("Description", text(desc)), ("Count Frequency per Year", Val::Int(freq))])?;
    }
    Ok(())
}

fn more_service_quotes(s: &mut Session) -> anyhow::Result<()> {
    use al_runtime::Val;
    let text = |x: &str| Val::text(x);
    if s.cat.table_by_name("Service Mgt. Setup").is_some() && setup_text(s, "Service Mgt. Setup", "Service Quote Nos.").is_empty() {
        insert_setup_row(s, "No. Series", &[("Code", text("SM-QUOTE"))], &[("Description", text("Service Quotes")), ("Default Nos.", Val::Bool(true))])?;
        insert_setup_row(s, "No. Series Line", &[("Series Code", text("SM-QUOTE")), ("Line No.", Val::Int(10000))], &[("Starting No.", text("SQ000001")), ("Increment-by No.", Val::Int(1))])?;
        if let Some(t) = s.cat.table_by_name("Service Mgt. Setup") {
            if let Some(fi) = t.field("Service Quote Nos.") {
                let h = s.new_record(t.id, false);
                if s.find_first(&h, false).map_err(|e| al_err("Service Mgt. Setup", e))? {
                    s.rec_validate(&h, fi, Some(text("SM-QUOTE")), 0).map_err(|e| al_err("Service Mgt. Setup", e))?;
                    s.rec_modify(&h, false).map_err(|e| al_err("Service Mgt. Setup", e))?.map_err(|e| anyhow::anyhow!("Service Mgt. Setup: {e}"))?;
                }
            }
        }
    }
    Ok(())
}

fn more_item_budgets(s: &mut Session) -> anyhow::Result<()> {
    let text = |x: &str| al_runtime::Val::text(x);
    if s.cat.table_by_name("Item Budget Name").is_some() {
        let sales = s.enum_value("Analysis Area Type", "Sales").map_err(|e| al_err("Analysis Area Type", e))?;
        let purchase = s.enum_value("Analysis Area Type", "Purchase").map_err(|e| al_err("Analysis Area Type", e))?;
        let customer_group = {
            let dim = s.cat.table_by_name("Dimension");
            match dim {
                Some(d) => {
                    let h = s.new_record(d.id, false);
                    if let Some(fi) = d.field("Code") {
                        h.borrow_mut().row[fi] = bc_store::Value::Text("CUSTOMERGROUP".into());
                    }
                    s.rec_get_pk(&h).is_some()
                }
                None => false,
            }
        };
        // no budget dimension: Contoso uses CUSTOMERGROUP in General Ledger Setup, and a budget
        // can't use a dimension the setup uses ("You cannot use the same dimension twice")
        let _ = customer_group;
        let sales_fields = vec![("Description", text("Default budget"))];
        insert_setup_row(s, "Item Budget Name", &[("Analysis Area", sales), ("Name", text("DEFAULT"))], &sales_fields)?;
        insert_setup_row(s, "Item Budget Name", &[("Analysis Area", purchase), ("Name", text("DEFAULT"))], &[("Description", text("Default budget"))])?;
    }
    Ok(())
}

fn more_purchase_variance(s: &mut Session) -> anyhow::Result<()> {
    // Purchase Variance Account on every General Posting Setup row
    const GPS: &str = "General Posting Setup";
    if let Some(t) = s.cat.table_by_name(GPS) {
        if let Some(fi) = t.field("Purchase Variance Account") {
            let mut rows = vec![];
            let h = s.new_record(t.id, false);
            if s.find_first(&h, false).map_err(|e| al_err(GPS, e))? {
                loop {
                    rows.push((*h.borrow().row).clone());
                    if s.next(&h, 1).map_err(|e| al_err(GPS, e))? == 0 {
                        break;
                    }
                }
            }
            let mut uses: BTreeMap<String, usize> = BTreeMap::new();
            for r in &rows {
                if let bc_store::Value::Text(acc) = &r[fi] {
                    if !acc.is_empty() {
                        *uses.entry(acc.clone()).or_default() += 1;
                    }
                }
            }
            if let Some((acc, _)) = uses.into_iter().max_by_key(|(_, n)| *n) {
                for mut r in rows {
                    if matches!(&r[fi], bc_store::Value::Text(x) if x.is_empty()) {
                        r[fi] = bc_store::Value::Text(acc.clone());
                        let h = s.new_record(t.id, false);
                        h.borrow_mut().row = r.into();
                        s.rec_modify(&h, false).map_err(|e| al_err(GPS, e))?.map_err(|e| anyhow::anyhow!("{GPS}: {e}"))?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn more_res_batch(s: &mut Session) -> anyhow::Result<()> {
    let text = |x: &str| al_runtime::Val::text(x);
    // the DEFAULT batch of the RESOURCES resource journal template
    let has_template = s.cat.table_by_name("Res. Journal Template").is_some_and(|t| {
        let t = t;
        let h = s.new_record(t.id, false);
        if let Some(fi) = t.field("Name") {
            h.borrow_mut().row[fi] = bc_store::Value::Text("RESOURCES".into());
        }
        s.rec_get_pk(&h).is_some()
    });
    if has_template {
        insert_setup_row(s, "Res. Journal Batch", &[("Journal Template Name", text("RESOURCES")), ("Name", text("DEFAULT"))], &[("Description", text("Default Journal"))])?;
    }
    Ok(())
}

/// The legacy tool's item journal templates for every type Contoso has none of ("Create Item
/// Journal Template" + "Create Item Journal Batch": PHYS. INV. and REVAL with their number series,
/// each with a DEFAULT batch). Contoso has no Phys. Inventory or Revaluation template: tests that
/// pick a batch by its template type then get none ("The Item Journal Template does not exist ...
/// Name=''", ERM Dimension Report since FlowField filters apply, I11) or a template of another type
/// ("Type must be equal to 'Revaluation' in Item Journal Template", 13).
fn legacy_item_templates(s: &mut Session) -> anyhow::Result<()> {
    use al_runtime::Val;
    let text = |x: &str| Val::text(x);
    const IJT: &str = "Item Journal Template";
    let Some(t) = s.cat.table_by_name(IJT) else { return Ok(()) };
    let (Some(ty_f), Some(rec_f)) = (t.field("Type"), t.field("Recurring")) else { return Ok(()) };
    // (name, description, type, recurring, source code setup field, series, series description, starting no.)
    let specs: [(&str, &str, &str, bool, &str, &str, &str, &str); 7] = [
        ("RECLASS", "Item Reclass. Journal", "Transfer", false, "Item Reclass. Journal", "IJNL-RCL", "Item Reclass. Journal", "T02001"),
        ("PHYS. INV.", "Item Journal", "Phys. Inventory", false, "Phys. Inventory Journal", "IJNL-PHYS", "Physical Inventory Journal", "T03001"),
        ("RECURRING", "Recurring Item Journal", "Item", true, "Item Journal", "IJNL-REC", "Recurring Item Journal", "T04001"),
        ("REVAL", "Revaluation Journal", "Revaluation", false, "Revaluation Journal", "IJNL-REVAL", "Revaluation Journal", "T05001"),
        ("CONSUMP", "Consumption Journal", "Consumption", false, "Consumption Journal", "", "", ""),
        ("OUTPUT", "Output Journal", "Output", false, "Output Journal", "", "", ""),
        ("CAPACITY", "Capacity Journal", "Capacity", false, "Capacity Journal", "", "", ""),
    ];
    // the (type, recurring) pairs that have a template
    let mut have = vec![];
    let h = s.new_record(t.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(IJT, e))? {
        loop {
            let r = h.borrow();
            have.push((r.row[ty_f].clone(), r.row[rec_f].clone()));
            drop(r);
            if s.next(&h, 1).map_err(|e| al_err(IJT, e))? == 0 {
                break;
            }
        }
    }
    for (name, desc, ty, recurring, src_field, series, series_desc, start) in specs {
        let tyv = s.enum_value("Item Journal Template Type", ty).map_err(|e| al_err("Item Journal Template Type", e))?;
        let ty_store = match &tyv {
            Val::Opt { ord, .. } => bc_store::Value::Option(*ord),
            _ => continue,
        };
        if have.iter().any(|(a, b)| *a == ty_store && *b == bc_store::Value::Bool(recurring)) {
            continue;
        }
        if !series.is_empty() {
            insert_setup_row(s, "No. Series", &[("Code", text(series))], &[("Description", text(series_desc)), ("Default Nos.", Val::Bool(true))])?;
            insert_setup_row(s, "No. Series Line", &[("Series Code", text(series)), ("Line No.", Val::Int(10000))], &[("Starting No.", text(start)), ("Increment-by No.", Val::Int(1))])?;
        }
        let src = setup_text(s, "Source Code Setup", src_field);
        let series_field = if recurring { "Posting No. Series" } else { "No. Series" };
        let mut fields = vec![("Description", text(desc)), ("Type", tyv), ("Recurring", Val::Bool(recurring)), ("Source Code", text(&src))];
        if !series.is_empty() {
            fields.push((series_field, text(series)));
        }
        if insert_setup_row(s, IJT, &[("Name", text(name))], &fields)? {
            insert_setup_row(s, "Item Journal Batch", &[("Journal Template Name", text(name)), ("Name", text("DEFAULT"))], &[("Description", text("Default Journal"))])?;
        }
    }
    Ok(())
}

/// No table-level default dimensions on projects, as in the legacy W1 company. Contoso's
/// ContosoJob.InsertJob validates "Bill-to Customer No." before Insert(true) gives the project its
/// number, so Job.CopyDefaultDimensionsFromCustomer writes the customer's default dimensions with
/// "No." = '' (BC does the same). After the last project, every project then has a table-level
/// AREA default with Value Posting Code Mandatory (and CUSTOMERGROUP Same Code), and posting WIP
/// for a test's own project stops at "The AREA dimension is the default dimension, and it must
/// have a value" (Job Performance WIP 24 of 53 on Contoso, all pass on W1; 28 in I11).
fn drop_job_table_defaults(s: &mut Session) -> anyhow::Result<()> {
    const DD: &str = "Default Dimension";
    let Some(t) = s.cat.table_by_name(DD) else { return Ok(()) };
    let (Some(tid), Some(no)) = (t.field("Table ID"), t.field("No.")) else { return Ok(()) };
    let job_table = s.cat.table_by_name("Job").map(|j| j.id).unwrap_or(167);
    let mut rows = vec![];
    let h = s.new_record(t.id, false);
    if s.find_first(&h, false).map_err(|e| al_err(DD, e))? {
        loop {
            {
                let r = h.borrow();
                if r.row[tid] == bc_store::Value::Int(job_table as i32) && matches!(&r.row[no], bc_store::Value::Text(x) if x.is_empty()) {
                    rows.push((*r.row).clone());
                }
            }
            if s.next(&h, 1).map_err(|e| al_err(DD, e))? == 0 {
                break;
            }
        }
    }
    for r in rows {
        let h = s.new_record(t.id, false);
        h.borrow_mut().row = r.into();
        s.rec_delete(&h, false).map_err(|e| al_err(DD, e))?.map_err(|e| anyhow::anyhow!("{DD}: {e}"))?;
    }
    Ok(())
}

/// Placeholder for the instant the session's connection is moved into the counting wrapper.
struct NullConn;

impl Conn for NullConn {
    fn get(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row, _: bool) -> Result<Option<Row>, StoreError> {
        Err(StoreError::Other("no connection".into()))
    }
    fn scan(&mut self, _: &str, _: &Arc<TableSchema>, _: &ScanReq) -> Result<Vec<Row>, StoreError> {
        Err(StoreError::Other("no connection".into()))
    }
    fn count(&mut self, _: &str, _: &Arc<TableSchema>, _: &[FieldFilter]) -> Result<usize, StoreError> {
        Err(StoreError::Other("no connection".into()))
    }
    fn aggregate(&mut self, _: &str, _: &Arc<TableSchema>, _: &[FieldFilter], _: usize, _: Agg) -> Result<AggOut, StoreError> {
        Err(StoreError::Other("no connection".into()))
    }
    fn insert(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row) -> Result<(), StoreError> {
        Err(StoreError::Other("no connection".into()))
    }
    fn modify(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row) -> Result<(), StoreError> {
        Err(StoreError::Other("no connection".into()))
    }
    fn delete(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row) -> Result<(), StoreError> {
        Err(StoreError::Other("no connection".into()))
    }
    fn lock_table(&mut self, _: &str, _: &Arc<TableSchema>) -> Result<(), StoreError> {
        Ok(())
    }
    fn is_locked(&self, _: &str, _: i64) -> bool {
        false
    }
    fn savepoint(&mut self) -> Result<usize, StoreError> {
        Ok(0)
    }
    fn rollback_to(&mut self, _: usize) -> Result<(), StoreError> {
        Ok(())
    }
    fn commit(&mut self) -> Result<(), StoreError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn in_write(&self) -> bool {
        false
    }
    fn seq(&mut self, _: SeqOp) -> Result<Option<i64>, StoreError> {
        Ok(None)
    }
}

/// Rows per table of a company (per-company tables plus shared ones), non-empty tables only.
pub fn table_counts(rt: &Runtime, company: &str) -> Vec<TableCount> {
    let mut out: Vec<TableCount> = rt
        .schemas
        .iter()
        .filter(|(id, _)| !al_runtime::virt::is_catalog_table(**id) && !rt.cat.tables[*id].temporary_only)
        .map(|(id, schema)| TableCount { id: *id, name: rt.cat.tables[id].name.clone(), rows: rt.db.table_count(company, schema) })
        .filter(|t| t.rows > 0)
        .collect();
    out.sort_by(|a, b| b.rows.cmp(&a.rows).then(a.id.cmp(&b.id)));
    out
}

/// State of one NumberSequence: the last value handed out and the increment.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SeqState {
    pub name: String,
    pub current: i64,
    pub increment: i64,
}

/// The number sequences BC code keeps next to the data: "TableSeq<table no.>" (entry numbers,
/// codeunit "Sequence No. Mgt.") and the "Sequence Name" of No. Series lines that allow gaps.
/// Copying rows without them would make the AL re-create them from their starting numbers.
pub fn number_sequences(rt: &Runtime, company: &str, conn: &mut dyn Conn) -> anyhow::Result<Vec<SeqState>> {
    let mut names: Vec<(String, i64)> = rt.cat.tables.keys().map(|id| (format!("TableSeq{id}"), 1)).collect();
    if let Some(t) = rt.cat.table_by_name("No. Series Line") {
        if let (Some(nf), Some(inf)) = (t.field("Sequence Name"), t.field("Increment-by No.")) {
            let req = ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
            for row in conn.scan(company, &rt.schemas[&t.id], &req)? {
                let name = match &row[nf] {
                    bc_store::Value::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let inc = match &row[inf] {
                    bc_store::Value::Int(i) => *i as i64,
                    bc_store::Value::BigInt(i) => *i,
                    _ => 1,
                };
                if !name.is_empty() {
                    names.push((name, inc.max(1)));
                }
            }
        }
    }
    let mut out = vec![];
    for (name, increment) in names {
        if conn.seq(SeqOp::Exists(&name))? != Some(1) {
            continue;
        }
        if let Some(current) = conn.seq(SeqOp::Current(&name))? {
            out.push(SeqState { name, current, increment });
        }
    }
    Ok(out)
}

/// Re-create sequences so that Current() returns the saved value, and drop the sequences that
/// are not in `seqs` (created after the snapshot, e.g. "TableSeq7313" by the first Warehouse
/// Register of a test, or AutoIncrement counters), so that a test doesn't see the numbers the
/// worker's earlier tests drew.
pub fn restore_sequences(conn: &mut dyn Conn, seqs: &[SeqState]) -> anyhow::Result<()> {
    let keep: std::collections::HashSet<String> = seqs.iter().map(|s| s.name.to_lowercase()).collect();
    for n in conn.seq_names()? {
        if !keep.contains(&n) {
            conn.seq(SeqOp::Delete(&n))?;
        }
    }
    for s in seqs {
        if conn.seq(SeqOp::Exists(&s.name))? == Some(1) {
            conn.seq(SeqOp::Delete(&s.name))?;
        }
        // created at the saved value and drawn once: Current() is the saved value and Next() the
        // one after it on every backend (a fresh PostgreSQL sequence reports its start value, the
        // memory store the value before it)
        conn.seq(SeqOp::Create(&s.name, s.current, s.increment))?;
        conn.seq(SeqOp::Next(&s.name))?;
    }
    Ok(())
}

/// A new in-memory backend holding a copy of every row of `src` (the given companies and the
/// shared tables) and its number sequences. Used to give each test worker its own copy of a
/// demo company that was created once.
pub fn copy_mem_backend(rt: &Runtime, src: &MemBackend, companies: &[&str], seqs: &[SeqState]) -> anyhow::Result<MemBackend> {
    let dst = MemBackend::new(bc_store::Store::memory());
    let mut from = src.connect()?;
    let mut to = dst.connect()?;
    let req = ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
    for (id, schema) in &rt.schemas {
        if al_runtime::virt::is_catalog_table(*id) || rt.cat.tables[id].temporary_only {
            continue;
        }
        let comps: &[&str] = if schema.per_company { companies } else { &companies[..companies.len().min(1)] };
        for company in comps {
            for row in from.scan(company, schema, &req)? {
                to.insert(company, schema, &row)?;
            }
        }
    }
    to.commit()?;
    restore_sequences(to.as_mut(), seqs)?;
    Ok(dst)
}

/// A test worker's copy-on-write view of a demo database that was created once and is only read
/// from then on (the master): row-level copy-on-write. The worker's own store holds only the rows
/// it changed, as an overlay over the shared master store:
///
/// * an inserted or modified row is stored in the overlay under its own table;
/// * a deleted master row is recorded as a tombstone: a copy of the row in a shadow table (same
///   key, table id negated) of the overlay;
/// * a read sees the overlay row if there is one, else the master row unless it has a tombstone.
///   Tables the worker has not changed (the common case) are read straight from the master.
///
/// Transactions, savepoints and rollbacks are the overlay store's own, so rolling back a test
/// leaves the overlay empty again. Each worker therefore holds the rows its running test changed
/// instead of a full copy of the company (`copy_mem_backend`). The number sequences are the
/// worker's own.
///
/// Like `MemBackend`, there is no isolation between connections of the same backend. The master
/// must not be written while views of it exist.
pub struct CowBackend {
    base: Arc<bc_store::Store>,
    own: MemBackend,
    tombs: Arc<std::sync::Mutex<std::collections::HashMap<i64, Arc<TableSchema>>>>,
}

/// A copy-on-write view of `master` (see `CowBackend`) with the number sequences `seqs`.
pub fn cow_backend(master: &MemBackend, seqs: &[SeqState]) -> anyhow::Result<CowBackend> {
    let b = CowBackend { base: master.store.clone(), own: MemBackend::new(bc_store::Store::memory()), tombs: Default::default() };
    let mut c = b.own.connect()?;
    restore_sequences(c.as_mut(), seqs)?;
    Ok(b)
}

impl CowBackend {
    /// Tables and rows in the worker's overlay (changed rows and tombstones; empty tables count).
    pub fn overlay_stats(&self) -> (usize, usize) {
        let s = self.own.stats();
        (s.tables, s.rows)
    }
}

impl Backend for CowBackend {
    fn connect(&self) -> Result<Box<dyn Conn>, StoreError> {
        Ok(Box::new(CowConn { base: self.base.clone(), over: self.own.store.clone(), own: self.own.connect()?, tombs: self.tombs.clone() }))
    }
    fn describe(&self) -> String {
        "copy-on-write view of an embedded in-memory store".into()
    }
    fn stats(&self) -> bc_store::StoreStats {
        self.base.stats()
    }
    fn table_count(&self, company: &str, schema: &Arc<TableSchema>) -> usize {
        match self.connect() {
            Ok(mut c) => c.count(company, schema, &[]).unwrap_or(0),
            Err(_) => 0,
        }
    }
}

struct CowConn {
    base: Arc<bc_store::Store>,
    /// the overlay store, for lookups outside the transaction API
    over: Arc<bc_store::Store>,
    /// the session's connection to the overlay store (its transaction)
    own: Box<dyn Conn>,
    tombs: Arc<std::sync::Mutex<std::collections::HashMap<i64, Arc<TableSchema>>>>,
}

impl CowConn {
    /// The shadow table holding tombstones of `schema`'s deleted master rows: same key, table id
    /// negated, no secondary keys.
    fn tomb(&self, schema: &TableSchema) -> Arc<TableSchema> {
        let mut m = self.tombs.lock().unwrap();
        m.entry(schema.id)
            .or_insert_with(|| {
                let mut t = schema.clone();
                t.id = -schema.id;
                t.keys.truncate(1);
                Arc::new(t)
            })
            .clone()
    }

    /// Whether the overlay has rows (changed or deleted master rows) for the table.
    fn has_overlay(&self, company: &str, schema: &TableSchema) -> bool {
        self.over.count(company, schema) > 0 || self.over.count(company, &self.tomb(schema)) > 0
    }

    /// Visit the master's rows of a scan in its order (bounds, filters and `after` as in
    /// `scan_table`), skipping rows the overlay hides (changed or deleted). The visitor gets the
    /// row's key in the scanned index and returns false to stop.
    fn visit_base(&self, company: &str, schema: &TableSchema, req: &ScanReq, mut f: impl FnMut(&[u8], &Arc<Row>) -> bool) {
        use std::ops::Bound;
        let tomb = self.tomb(schema);
        self.base.with_table(company, schema, |t| {
            let key = req.key.min(t.schema.keys.len() - 1);
            let (lo, hi) = bc_store::backend::key_bounds(&t.schema, key, req.filters);
            let after = req.after.map(|r| t.schema.index_key(key, r));
            let mut lo_b: Bound<&[u8]> = if lo.is_empty() { Bound::Unbounded } else { Bound::Included(&lo) };
            let mut hi_b: Bound<&[u8]> = match &hi {
                Some(h) => Bound::Excluded(h),
                None => Bound::Unbounded,
            };
            if let Some(a) = &after {
                if req.reverse {
                    if hi.as_ref().is_none_or(|h| a.as_ref() < h.as_slice()) {
                        hi_b = Bound::Excluded(a);
                    }
                } else if a.as_ref() >= lo.as_slice() {
                    lo_b = Bound::Excluded(a);
                }
            }
            t.scan(key, lo_b, hi_b, req.reverse, |k, row| {
                if !bc_store::backend::row_matches(req.filters, row) {
                    return true;
                }
                let secondary;
                let pk: &[u8] = if key == 0 {
                    k
                } else {
                    secondary = t.schema.pk(row);
                    &secondary
                };
                if self.over.get(company, schema, pk).is_some() || self.over.get(company, &tomb, pk).is_some() {
                    return true;
                }
                f(k, row)
            });
        });
    }

    /// A master row that is visible (not hidden by a tombstone).
    fn base_visible(&self, company: &str, schema: &TableSchema, pk: &[u8]) -> Option<Arc<Row>> {
        let row = self.base.get(company, schema, pk)?;
        self.over.get(company, &self.tomb(schema), pk).is_none().then_some(row)
    }
}

impl Conn for CowConn {
    fn get(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, _lock: bool) -> Result<Option<Row>, StoreError> {
        let pk = schema.pk(key);
        if let Some(r) = self.over.get(company, schema, &pk) {
            return Ok(Some((*r).clone()));
        }
        Ok(self.base_visible(company, schema, &pk).map(|r| (*r).clone()))
    }
    fn scan(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Row>, StoreError> {
        if !self.has_overlay(company, schema) {
            return Ok(self.base.with_table(company, schema, |t| bc_store::backend::scan_table(t, req)).unwrap_or_default());
        }
        let mut base: Vec<(Box<[u8]>, Row)> = vec![];
        self.visit_base(company, schema, req, |k, row| {
            base.push((k.into(), (**row).clone()));
            base.len() < req.limit
        });
        let own = self.own.scan(company, schema, req)?;
        if own.is_empty() {
            return Ok(base.into_iter().map(|(_, r)| r).collect());
        }
        // merge in the order of the scanned key (index keys end with the primary key: unique)
        let key = req.key.min(schema.keys.len() - 1);
        let own: Vec<(Box<[u8]>, Row)> = own.into_iter().map(|r| (schema.index_key(key, &r), r)).collect();
        let mut out = Vec::with_capacity((base.len() + own.len()).min(req.limit));
        let (mut a, mut b) = (base.into_iter().peekable(), own.into_iter().peekable());
        while out.len() < req.limit {
            let take_a = match (a.peek(), b.peek()) {
                (Some(x), Some(y)) => (x.0 < y.0) != req.reverse,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            out.push(if take_a { a.next() } else { b.next() }.unwrap().1);
        }
        Ok(out)
    }
    fn count(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter]) -> Result<usize, StoreError> {
        let req = ScanReq { key: bc_store::backend::best_key(schema, filters), reverse: false, filters, after: None, limit: usize::MAX, lock: false };
        if !self.has_overlay(company, schema) {
            if filters.is_empty() {
                return Ok(self.base.count(company, schema));
            }
            return Ok(self.base.with_table(company, schema, |t| bc_store::backend::scan_table(t, &req).len()).unwrap_or(0));
        }
        let mut n = 0;
        self.visit_base(company, schema, &req, |_, _| {
            n += 1;
            true
        });
        Ok(n + self.own.count(company, schema, filters)?)
    }
    fn aggregate(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter], col: usize, agg: Agg) -> Result<AggOut, StoreError> {
        let req = ScanReq {
            key: bc_store::backend::best_key(schema, filters),
            reverse: false,
            filters,
            after: None,
            limit: if agg == Agg::Exists { 1 } else { usize::MAX },
            lock: false,
        };
        let rows = self.scan(company, schema, &req)?;
        Ok(bc_store::backend::aggregate_rows(rows.iter(), col, agg))
    }
    fn insert(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        if self.base_visible(company, schema, &schema.pk(row)).is_some() {
            return Err(StoreError::Duplicate);
        }
        self.own.insert(company, schema, row)
    }
    fn modify(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        let pk = schema.pk(row);
        if self.over.get(company, schema, &pk).is_some() {
            return self.own.modify(company, schema, row);
        }
        if self.base_visible(company, schema, &pk).is_none() {
            return Err(StoreError::NotFound);
        }
        self.own.insert(company, schema, row)
    }
    fn delete(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row) -> Result<(), StoreError> {
        let pk = schema.pk(key);
        let in_own = self.over.get(company, schema, &pk).is_some();
        let in_base = self.base_visible(company, schema, &pk);
        if !in_own && in_base.is_none() {
            return Err(StoreError::NotFound);
        }
        if in_own {
            self.own.delete(company, schema, key)?;
        }
        if let Some(r) = in_base {
            let tomb = self.tomb(schema);
            self.own.insert(company, &tomb, &r)?;
        }
        Ok(())
    }
    fn lock_table(&mut self, company: &str, schema: &Arc<TableSchema>) -> Result<(), StoreError> {
        self.own.lock_table(company, schema)
    }
    fn is_locked(&self, company: &str, table: i64) -> bool {
        self.own.is_locked(company, table)
    }
    fn savepoint(&mut self) -> Result<usize, StoreError> {
        self.own.savepoint()
    }
    fn rollback_to(&mut self, sp: usize) -> Result<(), StoreError> {
        self.own.rollback_to(sp)
    }
    fn commit(&mut self) -> Result<(), StoreError> {
        self.own.commit()
    }
    fn rollback(&mut self) {
        self.own.rollback()
    }
    fn set_lock_timeout(&mut self, on: bool) {
        self.own.set_lock_timeout(on)
    }
    fn lock_timeout(&self) -> bool {
        self.own.lock_timeout()
    }
    fn set_lock_timeout_duration(&mut self, secs: i64) {
        self.own.set_lock_timeout_duration(secs)
    }
    fn lock_timeout_duration(&self) -> i64 {
        self.own.lock_timeout_duration()
    }
    fn in_write(&self) -> bool {
        self.own.in_write()
    }
    fn seq(&mut self, op: SeqOp) -> Result<Option<i64>, StoreError> {
        self.own.seq(op)
    }
    fn seq_names(&mut self) -> Result<Vec<String>, StoreError> {
        self.own.seq_names()
    }
    fn batch_hint(&self) -> usize {
        self.own.batch_hint()
    }
}

/// Shorthand for loading the catalog with the demo tool (leaked: the runtime needs 'static).
pub fn load_catalog(root: &Path) -> anyhow::Result<&'static Catalog> {
    Ok(Box::leak(Box::new(Catalog::load(&demo_app_dirs(root))?)))
}

/// A runtime whose catalog includes the demo tool (plus `extra` app directories), on `db`.
pub fn open_runtime(root: &Path, db: crate::app::Db, extra: &[PathBuf]) -> anyhow::Result<Runtime> {
    let mut dirs = demo_app_dirs(root);
    dirs.extend(extra.iter().cloned());
    open_runtime_dirs(&dirs, db)
}

/// A runtime on `db` whose catalog loads the app directories `dirs`.
pub fn open_runtime_dirs(dirs: &[PathBuf], db: crate::app::Db) -> anyhow::Result<Runtime> {
    let cat: &'static Catalog = Box::leak(Box::new(Catalog::load(dirs)?));
    let backend: Arc<dyn Backend> = match db {
        crate::app::Db::Memory => Arc::new(MemBackend::new(bc_store::Store::memory())),
        crate::app::Db::Wal(p) => {
            let (s, _) = bc_store::Store::open(&p, &Runtime::resolver(cat))?;
            s.set_sync(false);
            Arc::new(MemBackend::new(s))
        }
        crate::app::Db::Postgres(url) => Arc::new(bc_store::PgBackend::connect_url(&url).map_err(|e| anyhow::anyhow!("{e}"))?),
    };
    Ok(Runtime::new(cat, backend))
}

/// Create the demo company `company` in an empty company of `rt`'s database (refuses a company
/// whose Company Information already has a row), on a thread with a big stack, then compact an
/// embedded store's WAL.
pub fn seed(rt: &Runtime, company: &str, level: Level, trace: bool) -> anyhow::Result<DemoReport> {
    seed_with(rt, company, trace, move |s| create_demo_company(s, level))
}

/// `seed` with another way to create the company (e.g. `w1::create_w1_demo_company`).
pub fn seed_with(rt: &Runtime, company: &str, trace: bool, create: impl FnOnce(&mut Session) -> anyhow::Result<DemoReport> + Send) -> anyhow::Result<DemoReport> {
    if let Some(t) = rt.cat.table_by_name("Company Information") {
        let n = rt.db.table_count(company, &rt.schemas[&t.id]);
        if n > 0 {
            anyhow::bail!("company {company} already has data (Company Information has {n} rows); drop it first");
        }
    }
    let report = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(scope, || {
                al_runtime::stackguard::register(1 << 30);
                let mut s = Session::new(rt, company);
                s.trace = trace;
                create(&mut s)
            })
            .map_err(|e| anyhow::anyhow!("spawn: {e}"))?
            .join()
            .map_err(|_| anyhow::anyhow!("demo data thread panicked"))?
    })?;
    rt.db.compact()?;
    Ok(report)
}

/// Seed a database with the full Contoso demo company: what `demo-data --pg CONN` runs. For the
/// live demo, drop the company's schema (c_cronus) and `shared` first; `bc serve --pg` then finds
/// Company Information filled and skips its own small seed.
pub fn seed_database(root: &Path, db: crate::app::Db, company: &str, level: Level) -> anyhow::Result<DemoReport> {
    let rt = open_runtime(root, db, &[])?;
    seed(&rt, company, level, false)
}
