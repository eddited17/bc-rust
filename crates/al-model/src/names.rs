//! Object names the way the AL compiler resolves them when several objects of a kind share a
//! name (the standard-prod app set has eight such names, e.g. table "Dimension Set Entry": Base
//! App's 480 and PowerBIReports' Removed 36950).
//!
//! The compiler looks a name up in the objects of the app itself and of the apps it references
//! (app.json dependencies), in the namespace of the referencing file first, then in its `using`
//! namespaces, then in the global namespace. An object with `ObsoleteState = Removed` is still
//! found, but referencing it is an error outside upgrade code and other Removed objects, so
//! valid code never resolves to it.
//!
//! The catalog keeps one context-free name map ([`crate::Catalog::names`]); for a shared name it
//! holds the object that most apps see (not Removed, from the app most apps depend on).
//! While the catalog is built, every file resolves shared names in its own scope
//! ([`NameScope`]), and references whose scoped object differs from the context-free one are
//! pinned to the object id ([`pin_object_refs`]), so the runtime's context-free lookups find
//! the right object.

use al_syntax::*;

/// One of several objects of a kind that share a name.
#[derive(Debug, Clone)]
pub struct NameCand {
    pub id: i64,
    /// Index into the catalog's apps (`apps_of_dirs(app_dirs)`), None outside an app.
    pub app: Option<u16>,
    /// Namespace of the declaring file, lower-cased.
    pub namespace: Option<String>,
    pub removed: bool,
    /// Load order (the old resolution was last-wins).
    pub seq: u32,
}

/// Where a reference is written: the app, namespace and `using` namespaces of its file.
#[derive(Debug, Clone, Default)]
pub struct NameScope {
    pub app: Option<u16>,
    /// Lower-cased.
    pub namespace: Option<String>,
    /// Lower-cased.
    pub usings: Vec<String>,
}

impl NameScope {
    pub fn new(app: Option<usize>, namespace: Option<&str>, usings: &[String]) -> NameScope {
        NameScope {
            app: app.map(|a| a as u16),
            namespace: namespace.map(|n| n.to_ascii_lowercase()),
            usings: usings.iter().map(|u| u.to_ascii_lowercase()).collect(),
        }
    }
}

/// Which apps each loaded app can reference.
#[derive(Debug, Default)]
pub struct AppGraph {
    /// `sees[a][b]`: app b is a (transitive) dependency of app a, or a itself.
    pub sees: Vec<Vec<bool>>,
    /// `seen_by[b]`: how many apps see app b.
    pub seen_by: Vec<usize>,
}

impl AppGraph {
    pub fn new(apps: &[crate::apps::AppManifest]) -> AppGraph {
        let sees: Vec<Vec<bool>> = (0..apps.len()).map(|i| crate::apps::dependency_closure(apps, &[i])).collect();
        let seen_by = (0..apps.len()).map(|b| sees.iter().filter(|s| s[b]).count()).collect();
        AppGraph { sees, seen_by }
    }

    fn seen_by(&self, app: Option<u16>) -> usize {
        app.and_then(|a| self.seen_by.get(a as usize).copied()).unwrap_or(0)
    }

    /// Objects outside an app see, and are seen by, everything.
    fn visible(&self, from: Option<u16>, to: Option<u16>) -> bool {
        match (from, to) {
            (Some(a), Some(b)) => self.sees.get(a as usize).and_then(|s| s.get(b as usize)).copied().unwrap_or(true),
            _ => true,
        }
    }

    /// The context-free choice: not Removed; among apps, the one most apps see (then the lowest
    /// id, so the load order does not matter); if an object outside an app takes part, the last
    /// loaded (the old rule).
    pub fn global_pick<'a>(&self, cands: &'a [NameCand]) -> &'a NameCand {
        self.pick(live(cands.iter().collect()))
    }

    fn pick<'a>(&self, cands: Vec<&'a NameCand>) -> &'a NameCand {
        if cands.iter().all(|c| c.app.is_some()) {
            cands.into_iter().min_by_key(|c| (std::cmp::Reverse(self.seen_by(c.app)), c.id, c.seq)).unwrap()
        } else {
            cands.into_iter().max_by_key(|c| c.seq).unwrap()
        }
    }

    /// The object a reference in `scope` resolves to (None: none of them is visible there).
    pub fn scoped_pick<'a>(&self, scope: &NameScope, cands: &'a [NameCand]) -> Option<&'a NameCand> {
        let vis: Vec<&NameCand> = cands.iter().filter(|c| self.visible(scope.app, c.app)).collect();
        if vis.is_empty() {
            return None;
        }
        let live = live(vis);
        let tiers: [&dyn Fn(&NameCand) -> bool; 3] = [
            // the referencing file's own namespace (both in the global namespace counts)
            &|c| c.namespace == scope.namespace,
            // its `using` namespaces
            &|c| c.namespace.as_ref().is_some_and(|n| scope.usings.iter().any(|u| u == n)),
            // its own app
            &|c| scope.app.is_some() && c.app == scope.app,
        ];
        let pool: Vec<&NameCand> = tiers.iter().map(|t| live.iter().copied().filter(|c| t(c)).collect::<Vec<_>>()).find(|p| !p.is_empty()).unwrap_or(live);
        // own app first, then the context-free order
        let own: Vec<&NameCand> = pool.iter().copied().filter(|c| scope.app.is_some() && c.app == scope.app).collect();
        Some(self.pick(if own.is_empty() { pool } else { own }))
    }
}

/// Removed objects drop out unless every candidate is Removed.
fn live(cands: Vec<&NameCand>) -> Vec<&NameCand> {
    if cands.iter().any(|c| !c.removed) {
        cands.into_iter().filter(|c| !c.removed).collect()
    } else {
        cands
    }
}

/// The object kind a type or `Kind::` keyword names (None: not an object reference this module
/// pins; interfaces keep their names, they have no ids).
pub fn kind_of_keyword(kw: &str) -> Option<ObjectKind> {
    Some(match kw.to_ascii_lowercase().as_str() {
        "record" | "database" => ObjectKind::Table,
        "codeunit" => ObjectKind::Codeunit,
        "page" | "testpage" => ObjectKind::Page,
        "report" | "testrequestpage" => ObjectKind::Report,
        "query" => ObjectKind::Query,
        "xmlport" => ObjectKind::XmlPort,
        "enum" => ObjectKind::Enum,
        _ => return None,
    })
}

/// Replace the object names in `o` for which `pin(kind, name)` returns a replacement (an object
/// id): variable, parameter and return types (`Codeunit "Search"`), `Kind::"Name"` and enum
/// literals (`"Search Style"::Precise`) in code and attributes, and `Kind "Name"` token pairs in
/// properties and element arguments (field types `Enum "Search Confidence"`, `RunObject`).
pub fn pin_object_refs(o: &mut Object, pin: &mut dyn FnMut(ObjectKind, &str) -> Option<String>) {
    pin_body(&mut o.body, pin);
}

fn pin_body(b: &mut Body, pin: &mut dyn FnMut(ObjectKind, &str) -> Option<String>) {
    for m in &mut b.members {
        match m {
            Member::Property(p) => pin_tokens(&mut p.value, pin),
            Member::Element(e) => {
                for a in &mut e.args {
                    pin_tokens(a, pin);
                }
                pin_body(&mut e.body, pin);
            }
            Member::Procedure(p) => pin_proc(p, pin),
            Member::Vars { vars, .. } => {
                for v in vars {
                    pin_type(&mut v.ty, pin);
                }
            }
        }
    }
}

fn pin_tokens(ts: &mut [Token], pin: &mut dyn FnMut(ObjectKind, &str) -> Option<String>) {
    for i in 1..ts.len() {
        if ts[i].is_name() && ts[i - 1].kind == Tk::Ident {
            if let Some(k) = kind_of_keyword(&ts[i - 1].text) {
                if let Some(r) = pin(k, &ts[i].text) {
                    ts[i].text = r.into();
                }
            }
        }
    }
}

fn pin_type(t: &mut TypeRef, pin: &mut dyn FnMut(ObjectKind, &str) -> Option<String>) {
    match t {
        TypeRef::Object { kind, target, .. } => {
            if let Some(k) = kind_of_keyword(kind) {
                if let Some(r) = pin(k, target) {
                    *target = r;
                }
            }
        }
        TypeRef::Array { elem, .. } | TypeRef::List(elem) => pin_type(elem, pin),
        TypeRef::Dictionary(k, v) => {
            pin_type(k, pin);
            pin_type(v, pin);
        }
        _ => {}
    }
}

fn pin_proc(p: &mut Procedure, pin: &mut dyn FnMut(ObjectKind, &str) -> Option<String>) {
    for a in &mut p.attributes {
        for e in &mut a.args {
            pin_expr(e, pin);
        }
    }
    for prm in &mut p.params {
        pin_type(&mut prm.ty, pin);
    }
    if let Some(r) = &mut p.ret {
        pin_type(r, pin);
    }
    for v in &mut p.vars {
        pin_type(&mut v.ty, pin);
    }
    if let Some(body) = &mut p.body {
        for s in body {
            pin_stmt(s, pin);
        }
    }
}

fn pin_stmt(s: &mut Stmt, pin: &mut dyn FnMut(ObjectKind, &str) -> Option<String>) {
    match &mut s.kind {
        StmtKind::Empty | StmtKind::Break | StmtKind::Continue | StmtKind::Exit(None) => {}
        StmtKind::Block(b) => b.iter_mut().for_each(|s| pin_stmt(s, pin)),
        StmtKind::Assign { target, value, .. } => {
            pin_expr(target, pin);
            pin_expr(value, pin);
        }
        StmtKind::Expr(e) | StmtKind::Exit(Some(e)) => pin_expr(e, pin),
        StmtKind::If { cond, then, els } => {
            pin_expr(cond, pin);
            pin_stmt(then, pin);
            if let Some(e) = els {
                pin_stmt(e, pin);
            }
        }
        StmtKind::Case { subject, branches, els } => {
            pin_expr(subject, pin);
            for b in branches {
                b.values.iter_mut().for_each(|v| pin_expr(v, pin));
                pin_stmt(&mut b.body, pin);
            }
            if let Some(e) = els {
                e.iter_mut().for_each(|s| pin_stmt(s, pin));
            }
        }
        StmtKind::While { cond, body } => {
            pin_expr(cond, pin);
            pin_stmt(body, pin);
        }
        StmtKind::Repeat { body, until } => {
            body.iter_mut().for_each(|s| pin_stmt(s, pin));
            pin_expr(until, pin);
        }
        StmtKind::For { var, from, to, body, .. } => {
            pin_expr(var, pin);
            pin_expr(from, pin);
            pin_expr(to, pin);
            pin_stmt(body, pin);
        }
        StmtKind::ForEach { var, iter, body } => {
            pin_expr(var, pin);
            pin_expr(iter, pin);
            pin_stmt(body, pin);
        }
        StmtKind::With { rec, body } => {
            pin_expr(rec, pin);
            pin_stmt(body, pin);
        }
        StmtKind::AssertError(b) => pin_stmt(b, pin),
    }
}

fn pin_expr(e: &mut Expr, pin: &mut dyn FnMut(ObjectKind, &str) -> Option<String>) {
    match e {
        Expr::Scope(base, name) => {
            match &mut **base {
                // Codeunit::"Search", Enum::"Search Style", Database::"X"
                Expr::Ident(k) if kind_of_keyword(k).is_some() => {
                    if let Some(r) = pin(kind_of_keyword(k).unwrap(), name) {
                        *name = r.into();
                    }
                }
                // "Search Style"::Precise: an enum literal (pinned only if the name is an enum
                // whose scoped object differs; a variable or field of that name is rare)
                Expr::Ident(n) => {
                    if let Some(r) = pin(ObjectKind::Enum, n) {
                        *n = r.into();
                    }
                }
                other => pin_expr(other, pin),
            }
        }
        Expr::Member(b, _) | Expr::Unary(_, b) | Expr::Is(b, _) | Expr::As(b, _) => pin_expr(b, pin),
        Expr::Call(b, args) | Expr::Index(b, args) => {
            pin_expr(b, pin);
            args.iter_mut().for_each(|a| pin_expr(a, pin));
        }
        Expr::Binary(_, a, b) => {
            pin_expr(a, pin);
            pin_expr(b, pin);
        }
        Expr::Range(a, b) => {
            if let Some(a) = a {
                pin_expr(a, pin);
            }
            if let Some(b) = b {
                pin_expr(b, pin);
            }
        }
        Expr::Set(v) => v.iter_mut().for_each(|x| pin_expr(x, pin)),
        Expr::Ternary(a, b, c) => {
            pin_expr(a, pin);
            pin_expr(b, pin);
            pin_expr(c, pin);
        }
        Expr::Int(_)
        | Expr::BigInt(_)
        | Expr::Dec(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Date(_)
        | Expr::Time(_)
        | Expr::DateTime(_)
        | Expr::Ident(_)
        | Expr::This => {}
    }
}

#[cfg(test)]
mod tests {
    //! The eight names the standard-prod app set shares (2026-11-08), as small apps with the
    //! real ids, namespaces and dependencies, loaded in an order where last-wins picks wrong.
    use crate::{Catalog, FieldType};
    use al_syntax::*;
    use std::path::PathBuf;

    const SYS: &str = "00000000-0000-0000-0000-00000000a001";
    const BASE: &str = "00000000-0000-0000-0000-00000000a002";

    /// (folder, app name, app id, dependency ids, files)
    type App = (&'static str, &'static str, &'static str, &'static [&'static str], &'static [(&'static str, &'static str)]);

    const APPS: &[App] = &[
        (
            "sys",
            "System Application",
            SYS,
            &[],
            &[
                ("GraphClient.Codeunit.al", "namespace System.Integration.Graph;\ncodeunit 9350 \"Graph Client\" { }"),
                ("MagicFunction.Codeunit.al", "namespace System.Text;\ncodeunit 2018 \"Magic Function\" { }"),
                (
                    "SharePointGraphReqHelper.Codeunit.al",
                    "namespace System.Integration.Sharepoint;\nusing System.Integration.Graph;\ncodeunit 9120 \"SharePoint Graph Req. Helper\" { var GraphClient: Codeunit \"Graph Client\"; }",
                ),
                ("EntityTextImpl.Codeunit.al", "namespace System.Text;\ncodeunit 2012 \"Entity Text Impl.\" { var MagicFunction: Codeunit \"Magic Function\"; }"),
            ],
        ),
        (
            "base",
            "Base Application",
            BASE,
            &[SYS],
            &[
                (
                    "DimensionSetEntry.Table.al",
                    "namespace Microsoft.Finance.Dimension;\ntable 480 \"Dimension Set Entry\" { fields { field(1; \"Dimension Set ID\"; Integer) { } field(2; \"Dimension Code\"; Code[20]) { } } }",
                ),
                (
                    "SalesDims.Codeunit.al",
                    "namespace Microsoft.Sales.Document;\nusing Microsoft.Finance.Dimension;\ncodeunit 50480 \"Sales Dims\" {\n var DimSetEntry: Record \"Dimension Set Entry\";\n procedure TableNo(): Integer begin exit(Database::\"Dimension Set Entry\"); end;\n}",
                ),
            ],
        ),
        (
            "pbi",
            "Power BI Report embeddings for Dynamics 365 Business Central",
            "00000000-0000-0000-0000-00000000a003",
            &[BASE],
            &[
                (
                    "DimensionSetEntry.Table.al",
                    "namespace Microsoft.PowerBIReports;\ntable 36950 \"Dimension Set Entry\" { ObsoleteState = Removed; fields { field(1; \"Dimension Set ID\"; Integer) { } } }",
                ),
                (
                    "PBIDims.Codeunit.al",
                    "namespace Microsoft.PowerBIReports;\ncodeunit 36951 \"PBI Dims\" {\n var Qualified: Record Microsoft.Finance.Dimension.\"Dimension Set Entry\";\n var Plain: Record \"Dimension Set Entry\";\n}",
                ),
            ],
        ),
        (
            "m365",
            "E-Document Connector - Microsoft 365",
            "00000000-0000-0000-0000-00000000a004",
            &[BASE],
            &[
                ("GraphClient.Codeunit.al", "namespace Microsoft.EServices.EDocumentConnector.Microsoft365;\ncodeunit 6384 \"Graph Client\" { }"),
                (
                    "DriveProcessing.Codeunit.al",
                    "namespace Microsoft.EServices.EDocumentConnector.Microsoft365;\nusing System.Integration;\ncodeunit 6383 \"Drive Processing\" { var GraphClient: Codeunit \"Graph Client\"; }",
                ),
            ],
        ),
        (
            "cpi",
            "Create Product Information With Copilot",
            "00000000-0000-0000-0000-00000000a005",
            &[BASE],
            &[
                (
                    "Objects.al",
                    "namespace Microsoft.Inventory.Item.Substitution;\n\
                     codeunit 7341 \"Magic Function\" { }\n\
                     codeunit 7343 \"Notification Manager\" { }\n\
                     codeunit 7333 \"Search\" { }\n\
                     enum 7331 \"Search Confidence\" { value(0; None) { } value(3; High) { } }\n\
                     enum 7332 \"Search Style\" { value(0; Permissive) { } value(1; Balanced) { } value(2; Precise) { } }\n\
                     table 7339 \"Search API Response\" { fields { field(1; Id; Integer) { } field(2; Confidence; Enum \"Search Confidence\") { } } }",
                ),
                (
                    "ItemSubstSuggestionImpl.Codeunit.al",
                    "namespace Microsoft.Inventory.Item.Substitution;\n\
                     codeunit 7330 \"Item Subst. Suggestion Impl.\" {\n\
                      var MagicFunction: Codeunit \"Magic Function\"; NotificationManager: Codeunit \"Notification Manager\"; SearchUtility: Codeunit \"Search\";\n\
                      var Response: Record \"Search API Response\" temporary; Style: Enum \"Search Style\";\n\
                      procedure Confidence(): Enum \"Search Confidence\" begin exit(\"Search Confidence\"::High); end;\n\
                      procedure StyleOf(): Enum \"Search Style\" begin exit(Enum::\"Search Style\"::Balanced); end;\n\
                      procedure SearchId(): Integer begin exit(Codeunit::\"Search\"); end;\n\
                     }",
                ),
            ],
        ),
        (
            "sls",
            "Sales Lines Suggestions",
            "00000000-0000-0000-0000-00000000a006",
            &[BASE],
            &[
                (
                    "Objects.al",
                    "namespace Microsoft.Sales.Document;\n\
                     codeunit 7284 \"Magic Function\" { }\n\
                     codeunit 7279 \"Notification Manager\" { }\n\
                     codeunit 7282 \"Search\" { }\n\
                     enum 7276 \"Search Confidence\" { value(0; None) { } value(3; High) { } }\n\
                     enum 7277 \"Search Style\" { value(0; Permissive) { } value(1; Balanced) { } value(2; Precise) { } }\n\
                     table 7276 \"Search API Response\" { fields { field(1; Id; Integer) { } field(16; Confidence; Enum \"Search Confidence\") { } } }",
                ),
                (
                    "SalesLinesSuggestionsImpl.Codeunit.al",
                    "namespace Microsoft.Sales.Document;\n\
                     codeunit 7275 \"Sales Lines Suggestions Impl.\" {\n\
                      var MagicFunction: Codeunit \"Magic Function\"; NotificationManager: Codeunit \"Notification Manager\"; SearchUtility: Codeunit \"Search\";\n\
                      var Response: Record \"Search API Response\" temporary; Style: Enum \"Search Style\";\n\
                      procedure Confidence(): Enum \"Search Confidence\" begin exit(\"Search Confidence\"::High); end;\n\
                      procedure StyleOf(): Enum \"Search Style\" begin exit(Enum::\"Search Style\"::Balanced); end;\n\
                      procedure SearchId(): Integer begin exit(Codeunit::\"Search\"); end;\n\
                     }",
                ),
                // the current namespace wins over a `using` (System.Text has a "Magic Function" too)
                (
                    "SearchItemsWithFiltersFunc.Codeunit.al",
                    "namespace Microsoft.Sales.Document;\nusing System.Text;\ncodeunit 7290 \"Search Items With Filters Func\" { var MagicFunction: Codeunit \"Magic Function\"; }",
                ),
                // another namespace of the app, reaching its objects through `using`
                (
                    "SalesLineFromAttachment.Codeunit.al",
                    "namespace Microsoft.Sales.Document.Attachment;\nusing Microsoft.Sales.Document;\ncodeunit 7291 \"Sales Line From Attachment\" { var NotificationManager: Codeunit \"Notification Manager\"; Style: Enum \"Search Style\"; }",
                ),
            ],
        ),
    ];

    /// Write the apps under a fresh temp directory; load them in `order` (folder names).
    fn load(tag: &str, order: &[&str]) -> Catalog {
        // one directory per call: the tests run in parallel and each removes its directory at the end
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("al-model-names-{tag}-{}-{n}", std::process::id()));
        let mut dirs: Vec<PathBuf> = vec![];
        for (folder, name, id, deps, files) in APPS {
            let d = root.join(folder);
            std::fs::create_dir_all(&d).unwrap();
            let deps: Vec<String> = deps.iter().map(|x| format!(r#"{{ "id": "{x}", "name": "dep", "publisher": "Microsoft", "version": "1.0.0.0" }}"#)).collect();
            std::fs::write(d.join("app.json"), format!(r#"{{ "id": "{id}", "name": "{name}", "publisher": "Microsoft", "version": "1.0.0.0", "dependencies": [{}] }}"#, deps.join(","))).unwrap();
            for (f, src) in *files {
                std::fs::write(d.join(f), src).unwrap();
            }
        }
        for f in order {
            dirs.push(root.join(f));
        }
        let cat = Catalog::load(&dirs).unwrap();
        assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
        let _ = std::fs::remove_dir_all(&root);
        cat
    }

    /// Load order where last-wins resolves every shared name to the wrong object for most apps.
    fn catalog() -> Catalog {
        load("fwd", &["sys", "base", "pbi", "m365", "cpi", "sls"])
    }

    fn kind_of(kind: &str) -> ObjectKind {
        super::kind_of_keyword(kind).unwrap()
    }

    /// The object a global variable of code object `code` (codeunit id) has as its type.
    fn var_obj(cat: &Catalog, code: i64, var: &str) -> i64 {
        let co = cat.code_of(ObjectKind::Codeunit, code).unwrap_or_else(|| panic!("codeunit {code}"));
        let v = co.globals.iter().find(|v| v.name.eq_ignore_ascii_case(var)).unwrap_or_else(|| panic!("{var} in {code}"));
        match &v.ty {
            TypeRef::Object { kind, target, .. } => cat.id_of(kind_of(kind), target).unwrap(),
            t => panic!("{var}: {t:?}"),
        }
    }

    fn proc_of<'a>(cat: &'a Catalog, code: i64, name: &str) -> &'a Procedure {
        let co = cat.code_of(ObjectKind::Codeunit, code).unwrap();
        co.procs.iter().find(|p| p.name.eq_ignore_ascii_case(name)).unwrap()
    }

    /// The object of the first `Kind::"Name"` / `"Enum"::Value` / `Enum::"Name"::Value` in the
    /// `exit(...)` of procedure `name`, and of its return type.
    fn exit_obj(cat: &Catalog, code: i64, name: &str) -> (i64, Option<i64>) {
        let p = proc_of(cat, code, name);
        let Some(StmtKind::Exit(Some(e))) = p.body.as_ref().unwrap().first().map(|s| &s.kind) else { panic!("exit in {name}") };
        let id = match e {
            Expr::Scope(b, n) => match &**b {
                Expr::Ident(k) if super::kind_of_keyword(k).is_some() => cat.id_of(kind_of(k), n).unwrap(),
                Expr::Ident(n) => cat.id_of(ObjectKind::Enum, n).unwrap(),
                Expr::Scope(k, n) if matches!(&**k, Expr::Ident(k) if k.eq_ignore_ascii_case("enum")) => cat.id_of(ObjectKind::Enum, n).unwrap(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        let ret = match &p.ret {
            Some(TypeRef::Object { kind, target, .. }) => cat.id_of(kind_of(kind), target),
            _ => None,
        };
        (id, ret)
    }

    fn field_enum(cat: &Catalog, table: i64, field: &str) -> i64 {
        let t = &cat.tables[&table];
        let FieldType::Enum(e) = &t.fields[t.field(field).unwrap()].ty else { panic!("{field} is no enum field") };
        cat.enum_by_name(e).unwrap().id
    }

    #[test]
    fn removed_object_is_not_referable() {
        // context-free and from every app: the live object, whatever the load order
        for order in [["base", "pbi"], ["pbi", "base"]] {
            let cat = load(&format!("removed-{}", order[0]), &["sys", order[0], order[1]]);
            assert_eq!(cat.table_by_name("Dimension Set Entry").map(|t| t.id), Some(480), "{order:?}");
            assert_eq!(var_obj(&cat, 36951, "Plain"), 480, "{order:?}: PowerBIReports' own namespace has only the Removed table");
            assert_eq!(var_obj(&cat, 36951, "Qualified"), 480, "{order:?}");
        }
        // a Removed object nobody else's name hides stays referable (its table still exists)
        let cat = load("removed-only", &["sys", "pbi"]);
        assert_eq!(cat.id_of(ObjectKind::Table, "Dimension Set Entry"), Some(36950));
    }

    #[test]
    fn collision_dimension_set_entry() {
        let cat = catalog();
        assert_eq!(cat.names[&(ObjectKind::Table, "dimension set entry".into())], 480);
        assert_eq!(cat.table_by_name("Dimension Set Entry").unwrap().field("Dimension Code").is_some(), true);
        assert_eq!(var_obj(&cat, 50480, "DimSetEntry"), 480);
        assert_eq!(exit_obj(&cat, 50480, "TableNo").0, 480);
        assert_eq!(var_obj(&cat, 36951, "Plain"), 480);
        assert_eq!(cat.id_of(ObjectKind::Table, "Microsoft.Finance.Dimension.Dimension Set Entry"), Some(480));
        assert_eq!(cat.id_of(ObjectKind::Table, "Microsoft.PowerBIReports.Dimension Set Entry"), Some(36950));
    }

    #[test]
    fn collision_graph_client() {
        let cat = catalog();
        // context-free: System Application's (every app sees it); the connector its own
        assert_eq!(cat.id_of(ObjectKind::Codeunit, "Graph Client"), Some(9350));
        assert_eq!(var_obj(&cat, 9120, "GraphClient"), 9350);
        assert_eq!(var_obj(&cat, 6383, "GraphClient"), 6384);
    }

    #[test]
    fn collision_magic_function() {
        let cat = catalog();
        assert_eq!(cat.id_of(ObjectKind::Codeunit, "Magic Function"), Some(2018));
        assert_eq!(var_obj(&cat, 2012, "MagicFunction"), 2018);
        assert_eq!(var_obj(&cat, 7330, "MagicFunction"), 7341);
        assert_eq!(var_obj(&cat, 7275, "MagicFunction"), 7284);
        // `using System.Text` does not beat the file's own namespace
        assert_eq!(var_obj(&cat, 7290, "MagicFunction"), 7284);
    }

    #[test]
    fn collision_notification_manager() {
        let cat = catalog();
        assert_eq!(var_obj(&cat, 7330, "NotificationManager"), 7343);
        assert_eq!(var_obj(&cat, 7275, "NotificationManager"), 7279);
        assert_eq!(var_obj(&cat, 7291, "NotificationManager"), 7279, "through `using Microsoft.Sales.Document`");
    }

    #[test]
    fn collision_search() {
        let cat = catalog();
        assert_eq!(var_obj(&cat, 7330, "SearchUtility"), 7333);
        assert_eq!(var_obj(&cat, 7275, "SearchUtility"), 7282);
        assert_eq!(exit_obj(&cat, 7330, "SearchId").0, 7333);
        assert_eq!(exit_obj(&cat, 7275, "SearchId").0, 7282);
    }

    #[test]
    fn collision_search_confidence() {
        let cat = catalog();
        assert_eq!(exit_obj(&cat, 7330, "Confidence"), (7331, Some(7331)));
        assert_eq!(exit_obj(&cat, 7275, "Confidence"), (7276, Some(7276)));
        // table fields of the enum type
        assert_eq!(field_enum(&cat, 7339, "Confidence"), 7331);
        assert_eq!(field_enum(&cat, 7276, "Confidence"), 7276);
    }

    #[test]
    fn collision_search_style() {
        let cat = catalog();
        assert_eq!(var_obj(&cat, 7330, "Style"), 7332);
        assert_eq!(var_obj(&cat, 7275, "Style"), 7277);
        assert_eq!(var_obj(&cat, 7291, "Style"), 7277);
        assert_eq!(exit_obj(&cat, 7330, "StyleOf"), (7332, Some(7332)));
        assert_eq!(exit_obj(&cat, 7275, "StyleOf"), (7277, Some(7277)));
    }

    #[test]
    fn collision_search_api_response() {
        let cat = catalog();
        assert_eq!(var_obj(&cat, 7330, "Response"), 7339);
        assert_eq!(var_obj(&cat, 7275, "Response"), 7276);
    }

    #[test]
    fn resolution_does_not_depend_on_load_order() {
        let fwd = catalog();
        let rev = load("rev", &["sls", "cpi", "m365", "pbi", "base", "sys"]);
        for (code, var) in [(9120, "GraphClient"), (6383, "GraphClient"), (2012, "MagicFunction"), (7330, "MagicFunction"), (7275, "MagicFunction"), (7290, "MagicFunction"), (7330, "Style"), (7275, "Style"), (7330, "Response"), (7275, "Response"), (36951, "Plain")] {
            assert_eq!(var_obj(&fwd, code, var), var_obj(&rev, code, var), "{code} {var}");
        }
        for k in fwd.names.keys() {
            assert_eq!(fwd.names[k], rev.names[k], "{k:?}");
        }
    }
}
