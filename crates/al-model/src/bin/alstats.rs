//! Load the default app set into a catalog and print statistics.
fn main() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let dirs = al_model::default_app_dirs(&root);
    let cat = al_model::Catalog::load(&dirs)?;
    println!("{:?}", cat.stats);
    println!(
        "code objects={} tables={} pages={} enums={} interfaces={} event-subscriptions={} warnings={}",
        cat.code.len(),
        cat.tables.len(),
        cat.pages.len(),
        cat.enums.len(),
        cat.interfaces.len(),
        cat.subscribers.values().map(|v| v.len()).sum::<usize>(),
        cat.warnings.len()
    );
    for w in cat.warnings.iter().take(15) {
        println!("warn: {w}");
    }
    let t = cat.table_by_name("Customer").unwrap();
    println!("Customer: {} fields, {} keys, pk={:?}, ext fields={}", t.fields.len(), t.keys.len(), t.pk().fields.iter().map(|&i| &t.fields[i].name).collect::<Vec<_>>(), t.fields.iter().filter(|f| f.from_extension.is_some()).count());
    let bal = &t.fields[t.field("Balance (LCY)").unwrap()];
    println!("Balance (LCY): {:?}", bal.calc);
    let p = cat.page_by_name("Customer Card").unwrap();
    if std::env::var("DUPS").is_ok() {
        dup_fields(&cat);
    }
    if std::env::var("UNKNOWN").is_ok() {
        for (n, c) in unknown_tables(&cat).iter().take(45) {
            println!("unknown table {c:5} {n}");
        }
    }
    println!("Customer Card: type={} groups={} actions={} exts={}", p.page_type, p.layout.first().map(|a| a.children.len()).unwrap_or(0), p.actions.len(), p.ext_code.len());
    Ok(())
}

#[allow(dead_code)]
fn unknown_tables(cat: &al_model::Catalog) -> Vec<(String, usize)> {
    use al_syntax::TypeRef;
    let mut m: std::collections::HashMap<String, usize> = Default::default();
    let mut visit = |t: &TypeRef| {
        if let TypeRef::Object { kind, target, .. } = t {
            if kind == "record" && cat.id_of(al_syntax::ObjectKind::Table, target).and_then(|id| cat.tables.get(&id)).is_none() {
                *m.entry(target.clone()).or_default() += 1;
            }
        }
    };
    for co in &cat.code {
        for g in &co.globals {
            visit(&g.ty);
        }
        for p in &co.procs {
            for v in &p.vars {
                visit(&v.ty);
            }
            for v in &p.params {
                visit(&v.ty);
            }
        }
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

#[allow(dead_code)]
pub fn dup_fields(cat: &al_model::Catalog) {
    for t in cat.tables.values() {
        let mut seen = std::collections::HashMap::new();
        for f in &t.fields {
            if let Some(prev) = seen.insert(f.id, f.name.clone()) {
                println!("dup field id {} in table {} {}: {:?} vs {:?} (ext {:?})", f.id, t.id, t.name, prev, f.name, f.from_extension);
            }
        }
    }
}
