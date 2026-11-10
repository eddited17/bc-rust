// mimalloc: the interpreter allocates many small values (rows, strings, frames). Served-path A/B
// (P12 round 6): posting median about 5% lower, RSS 1.9 -> 1.25 GB, boot faster.
// Opt-in (cargo feature `mimalloc`, Linux and Windows only, see Cargo.toml): off by default while
// the heap corruption that crashed both mimalloc versions is open.
#[cfg(all(feature = "mimalloc", any(target_os = "linux", target_os = "windows")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use al_runtime::{Session, Val};
use bc_server::app::*;
use std::time::Instant;

fn table(s: &Session, name: &str) -> i64 {
    s.cat.table_by_name(name).unwrap_or_else(|| panic!("table {name}")).id
}

fn demo() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let app = boot(&root, Db::Memory)?;
    println!("catalog loaded in {} ms", app.load_ms);
    let mut s = Session::new(&app.rt, "CRONUS");
    s.trace = std::env::var("TRACE").is_ok();
    // minimal setup data, inserted without triggers
    for t in ["Marketing Setup", "General Ledger Setup", "Inventory Setup", "Purchases & Payables Setup", "Company Information"] {
        let r = s.new_record(table(&s, t), false);
        s.rec_insert(&r, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    }
    let setup = s.new_record(table(&s, "Sales & Receivables Setup"), false);
    set(&mut s, &setup, "Customer Nos.", Val::text("CUST"))?;
    s.rec_insert(&setup, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    let ns = s.new_record(table(&s, "No. Series"), false);
    set(&mut s, &ns, "Code", Val::text("CUST"))?;
    set(&mut s, &ns, "Default Nos.", Val::Bool(true))?;
    s.rec_insert(&ns, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    let nsl = s.new_record(table(&s, "No. Series Line"), false);
    set(&mut s, &nsl, "Series Code", Val::text("CUST"))?;
    set(&mut s, &nsl, "Line No.", Val::Int(10000))?;
    set(&mut s, &nsl, "Starting No.", Val::text("C00010"))?;
    set(&mut s, &nsl, "Increment-by No.", Val::Int(10))?;
    s.rec_insert(&nsl, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    s.commit()?;

    for i in 0..3 {
        let t0 = Instant::now();
        let cust = s.new_record(18, false);
        let r = s.rec_insert(&cust, true).map_err(flow_err)?;
        if let Err(e) = r {
            anyhow::bail!(e);
        }
        let fi = cust.borrow().tdef.field("Name").unwrap();
        s.rec_validate(&cust, fi, Some(Val::text(format!("Adatum Corporation {i}"))), 0).map_err(flow_err)?;
        s.rec_modify(&cust, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
        s.commit()?;
        println!(
            "inserted customer No.={} Name={:?} Search Name={:?} in {:?} ({} AL statements)",
            get(&cust, "No.").to_text(),
            get(&cust, "Name").to_text(),
            get(&cust, "Search Name").to_text(),
            t0.elapsed(),
            s.steps
        );
    }
    let all = s.new_record(18, false);
    let mut ok = s.find_first(&all, false).map_err(flow_err)?;
    while ok {
        println!("  stored: {} | {} | xrow-set={}", get(&all, "No.").to_text(), get(&all, "Name").to_text(), all.borrow().xrow.is_some());
        ok = s.next(&all, 1).map_err(flow_err)? != 0;
    }
    let nsl2 = s.new_record(table(&s, "No. Series Line"), false);
    s.find_first(&nsl2, false).map_err(flow_err)?;
    println!("  no. series line: last no used={} ", get(&nsl2, "Last No. Used").to_text());
    println!("messages: {:?}", s.messages);
    println!("missing builtins: {:?}", s.missing);
    Ok(())
}

fn bench() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let n_cust: usize = arg("--customers").and_then(|x| x.parse().ok()).unwrap_or(10_000);
    let n_entries: usize = arg("--entries").and_then(|x| x.parse().ok()).unwrap_or(500_000);
    let db = std::env::temp_dir().join(format!("bc-bench-{}.wal", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let spec = match db_arg(&root, true)? {
        Db::Memory => Db::Wal(db.clone()),
        other => other,
    };
    let is_pg = matches!(spec, Db::Postgres(_));
    if is_pg {
        reset_pg_company(&root, "CRONUS")?;
    }
    let app = boot(&root, spec)?;
    if std::env::args().any(|a| a == "--nosync") {
        eprintln!("note: --nosync only affects the embedded engine");
    }
    println!("boot (parse+catalog of {} AL files): {} ms", app.rt.cat.stats.files, app.load_ms);
    let mut s = Session::new(&app.rt, "CRONUS");
    bc_server::seed::seed(&mut s)?;

    // 1. master data through the real Base App triggers, one transaction (fsync) per customer
    let t0 = Instant::now();
    let steps0 = s.steps;
    let name_fi = s.cat.table_by_name("Customer").unwrap().field("Name").unwrap();
    for i in 0..n_cust {
        let c = s.new_record(18, false);
        s.rec_insert(&c, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
        s.rec_validate(&c, name_fi, Some(Val::text(format!("Bench Customer {i}"))), 2).map_err(flow_err)?;
        s.rec_modify(&c, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
        s.commit()?;
    }
    let el = t0.elapsed();
    println!(
        "Customer Insert(true)+Validate(Name)+Modify(true)+Commit({}): {n_cust} in {:.2?} = {:.0}/s, {:.1} us each, {} AL statements each",
        if std::env::args().any(|a| a == "--nosync") { "no fsync" } else { "fsync" },
        el,
        n_cust as f64 / el.as_secs_f64(),
        el.as_secs_f64() * 1e6 / n_cust as f64,
        (s.steps - steps0) / n_cust as u64
    );

    // 2. ledger volume (bulk, single transaction) to exercise FlowFields
    let dcle = s.cat.table_by_name("Detailed Cust. Ledg. Entry").unwrap().id;
    let all = s.new_record(18, false);
    let mut nos = vec![];
    let mut ok = s.find_first(&all, false).map_err(flow_err)?;
    while ok {
        nos.push(get(&all, "No.").to_text());
        ok = s.next(&all, 1).map_err(flow_err)? != 0;
    }
    let t0 = Instant::now();
    let e = s.new_record(dcle, false);
    for i in 0..n_entries {
        set(&mut s, &e, "Entry No.", Val::Int(i as i32 + 100_000))?;
        set(&mut s, &e, "Customer No.", Val::text(nos[i % nos.len()].clone()))?;
        set(&mut s, &e, "Amount (LCY)", Val::Dec(rust_decimal::Decimal::from((i % 997) as i64 + 1)))?;
        set(&mut s, &e, "Posting Date", Val::Date(al_runtime::value::today()))?;
        s.rec_insert(&e, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    }
    s.commit()?;
    let el = t0.elapsed();
    println!("Detailed Cust. Ledg. Entry bulk insert: {n_entries} in {:.2?} = {:.0}/s (one commit)", el, n_entries as f64 / el.as_secs_f64());

    // 3. FlowField: Balance (LCY) for every customer (CalcFormula sum over the ledger, index seek)
    let bal = s.cat.table_by_name("Customer").unwrap().field("Balance (LCY)").unwrap();
    let t0 = Instant::now();
    let all = s.new_record(18, false);
    let mut ok = s.find_first(&all, false).map_err(flow_err)?;
    let mut k = 0;
    let mut total = rust_decimal::Decimal::ZERO;
    while ok {
        s.calc_field(&all, bal).map_err(flow_err)?;
        total += get(&all, "Balance (LCY)").as_dec();
        k += 1;
        ok = s.next(&all, 1).map_err(flow_err)? != 0;
    }
    let el = t0.elapsed();
    println!("CalcFields(Balance (LCY)) over {k} customers / {n_entries} entries: {:.2?} = {:.1} us per customer (sum {total})", el, el.as_secs_f64() * 1e6 / k as f64);

    // 4. point reads
    let t0 = Instant::now();
    let c = s.new_record(18, false);
    let reps = 200_000;
    for i in 0..reps {
        set(&mut s, &c, "No.", Val::text(nos[i % nos.len()].clone()))?;
        assert!(s.rec_get_pk(&c).is_some());
    }
    let el = t0.elapsed();
    println!("Customer.Get by primary key: {reps} in {:.2?} = {:.2} us each", el, el.as_secs_f64() * 1e6 / reps as f64);

    // 5. restart: WAL replay (embedded engine only)
    drop(s);
    let st = app.rt.db.stats();
    println!("store: {} ({} rows in {} tables)", app.rt.db.describe(), st.rows, st.tables);
    drop(app);
    if !is_pg {
        let t0 = Instant::now();
        let cat: &'static al_model::Catalog = Box::leak(Box::new(al_model::Catalog::load(&al_model::default_app_dirs(&root))?));
        let t1 = Instant::now();
        let (_store, n) = bc_store::Store::open(&db, &al_runtime::Runtime::resolver(cat))?;
        println!("restart: catalog {:.2?}, WAL replay of {n} records {:.2?}; WAL size {} MB", t1 - t0, t1.elapsed(), std::fs::metadata(&db)?.len() / 1_000_000);
    }
    let _ = std::fs::remove_file(&db);
    Ok(())
}

/// CPU time of the calling thread in ns (Unix; 0 elsewhere). Unlike wall time it doesn't count
/// time the thread spends waiting for a CPU, so it survives a loaded host better.
fn thread_cpu_ns() -> u64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime only writes the timespec we pass
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn sales() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let app_set = bc_server::appsets::app_set_arg();
    let app = bc_server::appsets::boot_app_set(&root, db_arg(&root, true)?, &app_set)?;
    let mut s = Session::new(&app.rt, "CRONUS");
    bc_server::seed::seed(&mut s)?;
    bc_server::appsets::sync_app_set(&mut s, &root, &app_set);
    s.trace = std::env::var("TRACE").is_ok();
    if s.trace {
        eprintln!("TRACE: seed and install done");
    }
    let t0 = Instant::now();
    let sh = s.new_record(table(&s, "Sales Header"), false);
    let dt = s.enum_value("Sales Document Type", "Order").map_err(flow_err)?;
    set(&mut s, &sh, "Document Type", dt)?;
    s.rec_insert(&sh, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    println!("sales header inserted: {} {}", get(&sh, "Document Type").to_text(), get(&sh, "No.").to_text());
    let fi = sh.borrow().tdef.field("Sell-to Customer No.").unwrap();
    s.rec_validate(&sh, fi, Some(Val::text("C10000")), 2).map_err(flow_err)?;
    s.rec_modify(&sh, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    println!("customer validated: {} / bill-to {} / payment terms {} / posting date {} in {:?}", get(&sh, "Sell-to Customer Name").to_text(), get(&sh, "Bill-to Customer No.").to_text(), get(&sh, "Payment Terms Code").to_text(), get(&sh, "Posting Date").to_text(), t0.elapsed());
    let sl = s.new_record(table(&s, "Sales Line"), false);
    set(&mut s, &sl, "Document Type", get(&sh, "Document Type"))?;
    set(&mut s, &sl, "Document No.", get(&sh, "No."))?;
    set(&mut s, &sl, "Line No.", Val::Int(10000))?;
    s.rec_insert(&sl, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    for (f, v) in [("Type", s.enum_value("Sales Line Type", "Item").map_err(flow_err)?), ("No.", Val::text("1000")), ("Quantity", Val::Int(3))] {
        let fi = sl.borrow().tdef.field(f).unwrap();
        let fid = sl.borrow().field(fi).id as i32;
        s.rec_validate(&sl, fi, Some(v), fid).map_err(flow_err)?;
        println!("  line validated {f}");
    }
    s.rec_modify(&sl, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
    println!("line: {} x {} @ {} = {}", get(&sl, "Quantity").to_text(), get(&sl, "Description").to_text(), get(&sl, "Unit Price").to_text(), get(&sl, "Line Amount").to_text());
    s.commit()?;
    let posts: usize = std::env::var("POST").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    if posts > 1 {
        // warm-up / repeated postings: create and post (posts-1) more orders, report per-order time
        let mut times = vec![];
        // BENCH=1: thread CPU time and AL steps per order (entry + posting)
        let mut cpu: Vec<(u64, u64)> = vec![];
        // FRESH=1: a new session (database connection from the pool) for every
        // order entry and every posting, like the web API's one session per request
        let fresh = std::env::var("FRESH").is_ok();
        // BC_PG_TRACE=1: mark each order in the PostgreSQL statement trace (stderr)
        let mark = |what: &str, k: usize| {
            if std::env::var("BC_PG_TRACE").is_ok() {
                eprintln!("PGTRACE\tmark\t0\t{what} {}", k + 1);
            }
        };
        for k in 1..posts {
            if fresh {
                s = Session::new(&app.rt, "CRONUS");
            }
            let t0 = Instant::now();
            let cpu0 = thread_cpu_ns();
            let steps0 = s.steps;
            let qe0 = s.rt.db.queries();
            mark("ENTRY", k);
            let h2 = s.new_record(table(&s, "Sales Header"), false);
            let ord = s.enum_value("Sales Document Type", "Order").map_err(flow_err)?;
            set(&mut s, &h2, "Document Type", ord)?;
            s.rec_insert(&h2, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
            let fi = h2.borrow().tdef.field("Sell-to Customer No.").unwrap();
            s.rec_validate(&h2, fi, Some(Val::text(["C10000", "C10010", "C10020"][k % 3])), 2).map_err(flow_err)?;
            s.rec_modify(&h2, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
            let l2 = s.new_record(table(&s, "Sales Line"), false);
            set(&mut s, &l2, "Document Type", get(&h2, "Document Type"))?;
            set(&mut s, &l2, "Document No.", get(&h2, "No."))?;
            set(&mut s, &l2, "Line No.", Val::Int(10000))?;
            s.rec_insert(&l2, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
            for (f, v) in [("Type", s.enum_value("Sales Line Type", "Item").map_err(flow_err)?), ("No.", Val::text("1010")), ("Quantity", Val::Int(2))] {
                let fi = l2.borrow().tdef.field(f).unwrap();
                let fid = l2.borrow().field(fi).id as i32;
                s.rec_validate(&l2, fi, Some(v), fid).map_err(flow_err)?;
            }
            s.rec_modify(&l2, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
            s.commit()?;
            let entry = t0.elapsed();
            let entry_steps = s.steps - steps0;
            if fresh {
                s = Session::new(&app.rt, "CRONUS");
            }
            let steps1 = s.steps;
            let q0 = s.rt.db.queries();
            let entry_q = q0 - qe0;
            mark("POST", k);
            let t1 = Instant::now();
            set(&mut s, &h2, "Ship", Val::Bool(true))?;
            set(&mut s, &h2, "Invoice", Val::Bool(true))?;
            if let Some(row) = s.rec_get_pk(&h2) { h2.borrow_mut().row = row.into(); }
            set(&mut s, &h2, "Ship", Val::Bool(true))?;
            set(&mut s, &h2, "Invoice", Val::Bool(true))?;
            let id = s.cat.id_of(al_syntax::ObjectKind::Codeunit, "Sales-Post").unwrap();
            s.run_codeunit(id, Some(h2.clone())).map_err(flow_err)?;
            s.commit()?;
            times.push((entry, t1.elapsed(), entry_steps + s.steps - steps1, s.rt.db.queries() - q0, entry_q));
            cpu.push((thread_cpu_ns() - cpu0, entry_steps + s.steps - steps1));
            mark("END", k);
        }
        for (i, (e, p, st, q, eq)) in times.iter().enumerate() {
            if i < 3 || i + 1 == times.len() {
                println!("order {}: entry {:.1?} ({} SQL statements), post {:.1?}, {} AL statements, {} SQL statements in posting", i + 2, e, eq, p, st, q);
            }
        }
        if std::env::var_os("BENCH").is_some() && cpu.len() > 2 {
            let mut c: Vec<u64> = cpu.iter().skip(2).map(|x| x.0).collect();
            c.sort_unstable();
            let q = |p: f64| c[((c.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0;
            let steps: Vec<u64> = cpu.iter().skip(2).map(|x| x.1).collect();
            // CPU per AL step per order: interference on a busy host only inflates it, so the low
            // quantiles are the robust estimate of the code's own cost
            let mut nsps: Vec<f64> = cpu.iter().skip(2).map(|x| x.0 as f64 / x.1.max(1) as f64).collect();
            nsps.sort_by(|a, b| a.total_cmp(b));
            let pq = |p: f64| nsps[((nsps.len() - 1) as f64 * p).round() as usize];
            println!("BENCH orders={} cpu_us_median={:.1} cpu_us_p25={:.1} cpu_us_p75={:.1} steps_min={} steps_max={} ns_per_step_min={:.1} ns_per_step_p10={:.1} ns_per_step_median={:.1}", c.len(), q(0.5), q(0.25), q(0.75), steps.iter().min().unwrap_or(&0), steps.iter().max().unwrap_or(&0), pq(0.0), pq(0.1), pq(0.5));
        }
        let n = times.len() as f64;
        let avg_post: f64 = times.iter().skip(1).map(|t| t.1.as_secs_f64()).sum::<f64>() / (n - 1.0).max(1.0);
        let avg_entry: f64 = times.iter().skip(1).map(|t| t.0.as_secs_f64()).sum::<f64>() / (n - 1.0).max(1.0);
        println!("steady state (orders 3..{}): order entry {:.2} ms, posting {:.2} ms", posts, avg_entry * 1000.0, avg_post * 1000.0);
        let median = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.total_cmp(b));
            (v.get(v.len() / 2).copied().unwrap_or(0.0), v.first().copied().unwrap_or(0.0))
        };
        let (me, mine) = median(times.iter().skip(1).map(|t| t.0.as_secs_f64() * 1000.0).collect());
        let (mp, minp) = median(times.iter().skip(1).map(|t| t.1.as_secs_f64() * 1000.0).collect());
        if let Some(last) = times.last() {
            println!("steady state median/min: order entry {me:.2}/{mine:.2} ms ({} SQL statements), posting {mp:.2}/{minp:.2} ms ({} SQL statements)", last.4, last.3);
        }
    }
    if std::env::var("POST").is_ok() {
        let t0 = Instant::now();
        let steps0 = s.steps;
        s.rec_get_pk(&sh).map(|row| sh.borrow_mut().row = row.into());
        set(&mut s, &sh, "Ship", Val::Bool(true))?;
        set(&mut s, &sh, "Invoice", Val::Bool(true))?;
        let id = s.cat.id_of(al_syntax::ObjectKind::Codeunit, "Sales-Post").unwrap();
        match s.run_codeunit(id, Some(sh.clone())) {
            Ok(()) => {
                s.commit()?;
                println!("POSTED in {:?} ({} AL statements)", t0.elapsed(), s.steps - steps0);
                for tn in ["Sales Invoice Header", "Sales Invoice Line", "Cust. Ledger Entry", "Detailed Cust. Ledg. Entry", "G/L Entry", "VAT Entry", "Item Ledger Entry", "Value Entry", "G/L Register", "Sales Shipment Header"] {
                    let r = s.new_record(table(&s, tn), false);
                    let mut n = 0;
                    let mut ok = s.find_first(&r, false).map_err(flow_err)?;
                    while ok { n += 1; ok = s.next(&r, 1).map_err(flow_err)? != 0; }
                    println!("  {tn}: {n}");
                }
                let gl = s.new_record(table(&s, "G/L Entry"), false);
                let mut ok = s.find_first(&gl, false).map_err(flow_err)?;
                let mut sum = rust_decimal::Decimal::ZERO;
                while ok {
                    println!("    G/L {} acct {} {:>10} {} ({})", get(&gl, "Entry No.").to_text(), get(&gl, "G/L Account No.").to_text(), get(&gl, "Amount").to_text(), get(&gl, "Document No.").to_text(), get(&gl, "Description").to_text());
                    sum += get(&gl, "Amount").as_dec();
                    ok = s.next(&gl, 1).map_err(flow_err)? != 0;
                }
                println!("    G/L balance check: {sum}");
                let c = s.new_record(18, false);
                set(&mut s, &c, "No.", Val::text("C10000"))?;
                s.rec_get_pk(&c).map(|row| c.borrow_mut().row = row.into());
                let fi = c.borrow().tdef.field("Balance (LCY)").unwrap();
                s.calc_field(&c, fi).map_err(flow_err)?;
                println!("    customer C10000 balance after posting: {}", get(&c, "Balance (LCY)").to_text());
                let ile = s.new_record(table(&s, "Item Ledger Entry"), false);
                s.find_first(&ile, false).map_err(flow_err)?;
                println!("    item ledger: item {} qty {} type {}", get(&ile, "Item No.").to_text(), get(&ile, "Quantity").to_text(), get(&ile, "Entry Type").to_text());
            }
            Err(e) => {
                s.rollback();
                println!("posting failed after {} AL statements: {}", s.steps - steps0, flow_err(e));
            }
        }
    }
    println!("missing: {:?}", s.missing);
    Ok(())
}

/// `--pg` (default connection) / `--pg <conn>` / `--db <file.wal>` / `--memory`.
fn db_arg(root: &std::path::Path, default_memory: bool) -> anyhow::Result<Db> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--pg") {
        let next = args.get(i + 1).filter(|x| !x.starts_with("--")).cloned();
        return Ok(Db::Postgres(match next {
            Some(c) => c,
            None => default_pg(root).ok_or_else(|| anyhow::anyhow!("no PostgreSQL connection (set BC_PG or data/pg/password)"))?,
        }));
    }
    if args.iter().any(|a| a == "--memory") {
        return Ok(Db::Memory);
    }
    if let Some(p) = arg("--db") {
        let p = std::path::PathBuf::from(p);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        return Ok(Db::Wal(p));
    }
    Ok(if default_memory { Db::Memory } else { Db::Wal(root.join("data/cronus.wal")) })
}

/// Drop a company's schema (bench/test isolation on PostgreSQL).
fn reset_pg_company(root: &std::path::Path, company: &str) -> anyhow::Result<()> {
    let url = default_pg(root).ok_or_else(|| anyhow::anyhow!("no PostgreSQL connection"))?;
    let mut c = postgres::Client::connect(&url, postgres::NoTls)?;
    let s: String = company.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    c.batch_execute(&format!("DROP SCHEMA IF EXISTS \"c_{s}\" CASCADE; DROP SCHEMA IF EXISTS shared CASCADE; CREATE SCHEMA shared;"))?;
    Ok(())
}

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}

fn serve() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let db = db_arg(&root, false)?;
    let host = arg("--host").unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(8740);
    // --apps <set> / BC_APPS=<set>: load an app set (bc_server::appsets), e.g. "standard"
    let app_set = bc_server::appsets::app_set_arg();
    let app = bc_server::appsets::boot_app_set(&root, db, &app_set)?;
    let rt: &'static al_runtime::Runtime = Box::leak(Box::new(app.rt));
    let company = "CRONUS".to_string();
    {
        let mut s = Session::new(rt, &company);
        let ci = s.cat.table_by_name("Company Information").unwrap().id;
        if rt.db.table_count(&company, &rt.schemas[&ci]) == 0 {
            let t0 = Instant::now();
            match bc_server::seed::seed(&mut s) {
                Ok(n) => {
                    eprintln!("seeded demo company with {n} master records in {:?}", t0.elapsed());
                    rt.db.compact()?;
                }
                // another instance started on the same empty database and seeded it first
                // (its uncommitted rows made our inserts fail): use its company
                Err(e) => {
                    s.rollback();
                    if rt.db.table_count(&company, &rt.schemas[&ci]) == 0 {
                        return Err(e);
                    }
                    eprintln!("demo company was seeded by another instance meanwhile ({e})");
                }
            }
        }
    }
    // apps of the set that this database doesn't have installed yet (or at an older version)
    if app_set.is_some() {
        let mut s = Session::new(rt, &company);
        bc_server::appsets::sync_app_set(&mut s, &root, &app_set);
        rt.db.compact()?;
    }
    // --bg-workers N / BC_BG_WORKERS=N: run Scheduled Tasks (job queue) and StartSession sessions
    // concurrently in sessions of their own (al_runtime::workers, docs/job-queue.md). Off by default.
    let bg_workers = arg("--bg-workers").or_else(|| std::env::var("BC_BG_WORKERS").ok()).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
    if bg_workers > 0 {
        // the pool's AutoIncrement mode (workers::start) from the first request on, not only
        // once the pool runs
        al_runtime::recops::set_identity_autoincrement(true);
    }
    {
        // schema sync in the background: creates missing tables/columns/indexes ahead of first use;
        // the pool starts when it is done, so its first tasks don't set up tables beside the sync
        let schemas: Vec<std::sync::Arc<bc_store::TableSchema>> = rt.schemas.values().cloned().collect();
        let company = company.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            match rt.db.sync_schema(&company, &schemas) {
                Ok(n) if n > 0 => eprintln!("schema sync: {n} tables ready in {:.1?}", t0.elapsed()),
                Ok(_) => {}
                Err(e) => eprintln!("schema sync failed: {e}"),
            }
            if bg_workers > 0 {
                al_runtime::workers::start(rt, al_runtime::workers::Config::with_workers(bg_workers, &company));
            }
        });
    }
    // BC_AUTH: read the configuration now, so a bad one stops the start instead of the first request
    bc_server::auth::authenticator();
    let ctx: &'static bc_server::api::Ctx = Box::leak(Box::new(bc_server::api::Ctx { rt, company, boot_ms: app.load_ms }));
    let tokio = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    tokio.block_on(async move {
        let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
        eprintln!("bc-rust listening on http://{host}:{port}/ (catalog+db ready in {} ms)", ctx.boot_ms);
        // the peer address feeds the sign-in lockout (bc_server::auth)
        axum::serve(listener, bc_server::api::router(ctx).into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
        anyhow::Ok(())
    })
}

/// `bc user ...`: user accounts for BC_AUTH (bc_server::usercmd, docs/authentication.md), on the
/// database `bc serve` would use (same flags).
fn user() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let db = db_arg(&root, false)?;
    let app_set = bc_server::appsets::app_set_arg();
    let app = bc_server::appsets::boot_app_set(&root, db, &app_set)?;
    let mut s = Session::new(&app.rt, "CRONUS");
    let args: Vec<String> = std::env::args().skip(2).collect();
    print!("{}", bc_server::usercmd::run(&mut s, &args, &mut std::io::stdin().lock())?);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    match cmd.as_str() {
        "demo" => demo(),
        "serve" => serve(),
        "bench" => bench(),
        "sales" => sales(),
        "user" => user(),
        other => anyhow::bail!("unknown command {other} (use: serve | demo | user)"),
    }
}
