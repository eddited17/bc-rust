//! Helper for tools/platform-gen/gen.py: loads AL through bc-rust's own parser (al-model)
//! and prints what the generator needs. Output is line-based text on stdout.
//!
//!   default-dirs <repo root>   the app directories bc-rust loads (al_model::default_app_dirs)
//!   dump <dir>...              canonical dump of the tables, codeunits, pages and reports with id >= 2000000000
//!   refs <dir>...              per system table: resolved relation/CalcFormula targets and enum types
//!   unknown <dir>...           table names the loaded code uses but no loaded table defines
//!   stats <dir>...             load statistics and every catalog warning
use std::collections::BTreeMap;
use std::path::PathBuf;

use al_model::{Catalog, FieldClass, FieldType};

fn load(dirs: &[String]) -> anyhow::Result<Catalog> {
    let dirs: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
    Catalog::load(&dirs)
}

fn ty(t: &FieldType) -> String {
    match t {
        FieldType::Code(n) => format!("Code[{n}]"),
        FieldType::Text(n) => format!("Text[{n}]"),
        FieldType::Enum(e) => format!("Enum \"{e}\""),
        FieldType::Other(o) => format!("Other({o})"),
        o => format!("{o:?}"),
    }
}

fn system_tables(cat: &Catalog) -> Vec<&al_model::TableDef> {
    let mut v: Vec<_> = cat.tables.values().filter(|t| t.id >= 2000000000).collect();
    v.sort_by_key(|t| t.id);
    v
}

/// Fields, types, options, classes, relations, keys: what must match Microsoft's definition.
fn dump(cat: &Catalog) {
    for t in system_tables(cat) {
        let tt = if t.table_type.is_empty() { String::new() } else { format!(" TableType={}", t.table_type) };
        println!("table {} \"{}\" DataPerCompany={}{}{}", t.id, t.name, t.data_per_company, tt, if t.obsolete_removed { " REMOVED" } else { "" });
        for f in t.fields.iter().filter(|f| f.id < 2000000000) {
            let mut extra = String::new();
            if !f.options.is_empty() {
                extra += &format!(" Options=[{}]", f.options.join(","));
            }
            if f.class != FieldClass::Normal {
                extra += &format!(" Class={:?}", f.class);
            }
            if f.auto_increment {
                extra += " AutoIncrement";
            }
            if let Some(c) = &f.calc {
                extra += &format!(" Calc={:?}:{}.{}", c.method, c.table, c.field.clone().unwrap_or_default());
            }
            for r in &f.relation {
                extra += &format!(" Rel={}.{}", r.table, r.field.clone().unwrap_or_default());
            }
            if let Some(v) = &f.init_value {
                extra += &format!(" Init={v}");
            }
            if let Some(s) = f.prop_src.get("obsoletestate") {
                extra += &format!(" Obsolete={s}");
            }
            println!("  field {} \"{}\" {}{}", f.id, f.name, ty(&f.ty), extra);
        }
        for k in &t.keys {
            let names: Vec<_> = k.fields.iter().map(|&i| format!("\"{}\"", t.fields[i].name)).collect();
            println!("  key {} ({}){}{}", k.name, names.join(","), if k.clustered { " clustered" } else { "" }, if k.enabled { "" } else { " disabled" });
        }
    }
    dump_code(cat);
}

/// Codeunits, pages and reports with id >= 2000000000 as the parser reads them: properties the
/// catalog keeps, globals, and every procedure with attributes, signature, locals and body.
/// Statement line numbers are left out, since the copies drop the `///` lines.
fn dump_code(cat: &Catalog) {
    use al_syntax::ObjectKind;
    let mut v: Vec<_> = cat
        .code
        .iter()
        .filter(|c| matches!(c.kind, ObjectKind::Codeunit | ObjectKind::Page | ObjectKind::Report) && c.id >= 2000000000)
        .collect();
    v.sort_by_key(|c| (format!("{:?}", c.kind), c.id));
    for c in v {
        let kind = format!("{:?}", c.kind).to_lowercase();
        println!(
            "{kind} {} \"{}\" ns={:?} SingleInstance={} Subtype={:?} Table={:?} Implements={:?} ManualSubscriber={}",
            c.id, c.name, c.namespace, c.single_instance, c.subtype, c.table, c.implements, c.manual_subscriber
        );
        for g in &c.globals {
            println!("  var {g:?}");
        }
        for p in &c.procs {
            println!("  {:?} {:?} {} attrs={:?} params={:?} ret={:?} ret_name={:?}", p.kind, p.access, p.name, p.attributes, p.params, p.ret, p.ret_name);
            for l in &p.vars {
                println!("    var {l:?}");
            }
            if let Some(body) = &p.body {
                for s in body {
                    println!("    {}", without_lines(&format!("{s:?}")));
                }
            }
        }
    }
}

/// `{:?}` of a statement without its `line: N` fields.
fn without_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(", line: ") {
        out.push_str(&rest[..i]);
        rest = rest[i + ", line: ".len()..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

/// `<table id>\ttable\t<target id>` for relations and CalcFormulas the catalog resolves,
/// `<table id>\tenum\t<enum name>` for enum-typed fields.
fn refs(cat: &Catalog) {
    for t in system_tables(cat) {
        let mut out = std::collections::BTreeSet::new();
        for f in &t.fields {
            let targets = f.relation.iter().map(|r| r.table.as_str()).chain(f.calc.iter().map(|c| c.table.as_str()));
            for name in targets {
                if let Some(rt) = cat.table_by_name(name) {
                    out.insert(format!("table\t{}", rt.id));
                }
            }
            if let FieldType::Enum(e) = &f.ty {
                let name = cat.enum_by_name(e).map(|d| d.name.clone()).unwrap_or_else(|| e.clone());
                out.insert(format!("enum\t{name}"));
            }
        }
        for o in out {
            println!("{}\t{o}", t.id);
        }
    }
}

/// `<count>\t<name>\t<kinds>`: record types, relations, CalcFormulas and page source tables
/// that name a table the catalog does not have.
fn unknown(cat: &Catalog) {
    use al_syntax::TypeRef;
    let known = |n: &str| cat.table_by_name(n).is_some();
    let mut m: BTreeMap<String, BTreeMap<&'static str, usize>> = Default::default();
    let mut add = |n: &str, kind: &'static str| {
        *m.entry(n.trim_matches('"').to_string()).or_default().entry(kind).or_default() += 1;
    };
    let visit = |t: &TypeRef, add: &mut dyn FnMut(&str, &'static str)| {
        if let TypeRef::Object { kind, target, .. } = t {
            if kind == "record" && !known(target) {
                add(target, "record");
            }
        }
    };
    for co in &cat.code {
        for g in &co.globals {
            visit(&g.ty, &mut add);
        }
        for p in &co.procs {
            for v in &p.vars {
                visit(&v.ty, &mut add);
            }
            for v in &p.params {
                visit(&v.ty, &mut add);
            }
        }
    }
    for t in cat.tables.values() {
        for f in &t.fields {
            for r in f.relation.iter().filter(|r| !known(&r.table)) {
                add(&r.table, "relation");
            }
            if let Some(c) = f.calc.as_ref().filter(|c| !known(&c.table)) {
                add(&c.table, "calc");
            }
        }
    }
    for p in cat.pages.values().filter(|p| p.source_table.is_none()) {
        if let Some(s) = p.prop_src.get("sourcetable").filter(|s| !known(s)) {
            add(s, "page");
        }
    }
    let mut v: Vec<_> = m.into_iter().map(|(n, k)| (k.values().sum::<usize>(), n, k)).collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (total, n, k) in v {
        let kinds: Vec<_> = k.iter().map(|(a, b)| format!("{a}={b}")).collect();
        println!("{total}\t{n}\t{}", kinds.join(" "));
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = args.split_first().map(|(c, r)| (c.as_str(), r)).unwrap_or(("", &[]));
    match cmd {
        "default-dirs" => {
            let root = PathBuf::from(rest.first().ok_or_else(|| anyhow::anyhow!("default-dirs needs the repo root"))?);
            for d in al_model::default_app_dirs(&root) {
                println!("{}", d.display());
            }
        }
        "dump" => dump(&load(rest)?),
        "refs" => refs(&load(rest)?),
        "unknown" => unknown(&load(rest)?),
        "stats" => {
            let cat = load(rest)?;
            println!("{:?} tables={} enums={} interfaces={} warnings={}", cat.stats, cat.tables.len(), cat.enums.len(), cat.interfaces.len(), cat.warnings.len());
            for w in &cat.warnings {
                println!("warn: {w}");
            }
        }
        _ => anyhow::bail!("usage: platform-gen-probe default-dirs <root> | dump|refs|unknown|stats <dir>..."),
    }
    Ok(())
}
