//! The object catalog: every object of the loaded apps, with table/page/enum
//! metadata resolved and extensions merged into their base objects.

use crate::filters::*;
use crate::props::*;
use al_syntax::*;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type ObjKey = (ObjectKind, i64);

/// Run `f` on the ASCII-lowercased `s` without allocating for names up to 128 bytes.
#[inline]
pub fn with_lower<R>(s: &str, f: impl FnOnce(&str) -> R) -> R {
    let b = s.as_bytes();
    if b.len() <= 128 {
        let mut buf = [0u8; 128];
        for (i, c) in b.iter().enumerate() {
            buf[i] = c.to_ascii_lowercase();
        }
        // ASCII lowercasing keeps UTF-8 valid
        f(std::str::from_utf8(&buf[..b.len()]).unwrap())
    } else {
        f(&s.to_ascii_lowercase())
    }
}

/// Reference to a procedure: index into `Catalog::code` + index into its `procs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct ProcRef {
    pub code: u32,
    pub proc: u32,
}

/// A unit of AL code with its own global variables: an object or an extension object.
#[derive(Debug)]
pub struct CodeObject {
    pub kind: ObjectKind,
    pub id: i64,
    pub name: String,
    pub namespace: Option<String>,
    /// For tables/table extensions: the table id. For pages/reports: source table id (if any).
    pub table: Option<i64>,
    /// For extension objects: the base object.
    pub extends: Option<ObjKey>,
    pub globals: Vec<VarDecl>,
    pub procs: Vec<Arc<Procedure>>,
    /// Callable procedures and object-level triggers by lower-cased name.
    pub by_name: HashMap<String, Vec<u32>>,
    pub single_instance: bool,
    pub subtype: Option<String>,
    pub implements: Vec<String>,
    pub file: Arc<PathBuf>,
    /// `EventSubscriberInstance = Manual`: subscribers only fire for instances bound with BindSubscription.
    pub manual_subscriber: bool,
}

impl CodeObject {
    pub fn find(&self, name: &str) -> &[u32] {
        with_lower(name, |l| self.by_name.get(l).map(|v| v.as_slice())).unwrap_or(&[])
    }
    pub fn trigger(&self, name: &str) -> Option<u32> {
        self.find(name).iter().copied().find(|&i| self.procs[i as usize].kind == ProcKind::Trigger)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum FieldType {
    Integer,
    BigInteger,
    Decimal,
    Boolean,
    Code(u32),
    Text(u32),
    Date,
    Time,
    DateTime,
    Duration,
    DateFormula,
    Guid,
    Option,
    Enum(String),
    Blob,
    Media,
    MediaSet,
    RecordId,
    TableFilter,
    Other(String),
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum FieldClass {
    Normal,
    FlowField,
    FlowFilter,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldDef {
    pub id: i64,
    pub name: String,
    pub caption: String,
    pub ty: FieldType,
    pub class: FieldClass,
    pub calc: Option<CalcFormula>,
    pub init_value: Option<String>,
    pub options: Vec<String>,
    pub option_captions: Vec<String>,
    pub relation: Vec<RelationBranch>,
    /// ValidateTableRelation (default true): Validate checks that the value exists in the related table.
    pub validate_relation: bool,
    pub editable: bool,
    pub not_blank: bool,
    pub auto_increment: bool,
    pub blank_zero: bool,
    pub tooltip: Option<String>,
    pub decimal_places: Option<String>,
    pub on_validate: Option<ProcRef>,
    pub before_validate: Vec<ProcRef>,
    pub after_validate: Vec<ProcRef>,
    pub on_lookup: Option<ProcRef>,
    /// Defined by a table extension (id of the tableextension object).
    pub from_extension: Option<i64>,
    /// Every property of the field by lower-cased name, as [`prop_value_src`] renders it
    /// (string literal content, otherwise lossless AL source). Covers the properties that
    /// have no typed member: `ClosingDates`, `ExtendedDatatype`, `CaptionClass`, `MinValue`,
    /// `AccessByPermission`, `AutoFormatType`, `DataClassification`, ...
    pub prop_src: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyDef {
    pub name: String,
    pub fields: Vec<usize>,
    pub sum_fields: Vec<usize>,
    pub clustered: bool,
    pub unique: bool,
    pub enabled: bool,
    pub maintain_sql_index: bool,
    pub maintain_sift_index: bool,
}

/// `fieldgroup(DropDown; "No.", Name)`, including fields added by table extensions.
#[derive(Debug, Clone, Serialize)]
pub struct FieldGroupDef {
    pub name: String,
    pub caption: Option<String>,
    /// Field names as written in AL; resolve with [`TableDef::field`].
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TableTriggers {
    pub on_insert: Option<ProcRef>,
    pub on_modify: Option<ProcRef>,
    pub on_delete: Option<ProcRef>,
    pub on_rename: Option<ProcRef>,
    /// From table extensions: OnBeforeInsert, OnAfterInsert, ... (lower-cased name).
    pub ext: Vec<(String, ProcRef)>,
}

#[derive(Debug, Serialize)]
pub struct TableDef {
    pub id: i64,
    pub name: String,
    pub caption: String,
    pub namespace: Option<String>,
    pub code: u32,
    pub ext_code: Vec<u32>,
    pub fields: Vec<FieldDef>,
    #[serde(skip)]
    pub field_by_name: HashMap<String, usize>,
    #[serde(skip)]
    pub field_by_id: HashMap<i64, usize>,
    pub keys: Vec<KeyDef>,
    pub table_type: String,
    pub data_per_company: bool,
    pub temporary_only: bool,
    pub lookup_page: Option<String>,
    pub drilldown_page: Option<String>,
    pub data_caption_fields: Vec<String>,
    pub triggers: TableTriggers,
    pub obsolete_removed: bool,
    pub field_groups: Vec<FieldGroupDef>,
    /// Table-level properties by lower-cased name, rendered as [`prop_value_src`] does.
    pub prop_src: HashMap<String, String>,
}

impl TableDef {
    pub fn field(&self, name: &str) -> Option<usize> {
        with_lower(name, |l| self.field_by_name.get(l).copied())
    }
    pub fn field_no(&self, id: i64) -> Option<usize> {
        self.field_by_id.get(&id).copied()
    }
    pub fn pk(&self) -> &KeyDef {
        &self.keys[0]
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EnumValue {
    pub ordinal: i64,
    pub name: String,
    pub caption: String,
    pub implementations: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnumDef {
    pub id: i64,
    pub name: String,
    pub caption: String,
    pub extensible: bool,
    pub implements: Vec<String>,
    pub values: Vec<EnumValue>,
    pub default_implementations: Vec<(String, String)>,
}

impl EnumDef {
    pub fn by_name(&self, n: &str) -> Option<&EnumValue> {
        self.values.iter().find(|v| v.name.eq_ignore_ascii_case(n))
    }
    pub fn by_ordinal(&self, o: i64) -> Option<&EnumValue> {
        self.values.iter().find(|v| v.ordinal == o)
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Control {
    pub kind: String,
    pub name: String,
    pub caption: Option<String>,
    /// Source expression text (field controls), e.g. `Rec."No."` or `BalanceAsVendor`.
    pub source: Option<String>,
    #[serde(skip)]
    pub source_expr: Option<Expr>,
    /// Resolved table field (index into the source table) when the source is `Rec.<field>`.
    pub field: Option<usize>,
    pub part_page: Option<String>,
    pub sub_page_link: Vec<Cond>,
    pub props: HashMap<String, String>,
    pub triggers: HashMap<String, ProcRef>,
    pub children: Vec<Control>,
    /// Second element argument for kinds without a dedicated member: the target action of
    /// `actionref(Name; Target)`, the add-in of `usercontrol(Name; AddIn)`.
    pub target: Option<String>,
    /// Same keys as `props`, but rendered losslessly ([`prop_value_src`]), so expression
    /// valued properties (`Visible = Rec."No." <> ''`) can be re-lexed and parsed.
    pub prop_src: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct PageDef {
    pub id: i64,
    pub name: String,
    pub caption: String,
    pub page_type: String,
    pub source_table: Option<i64>,
    pub code: u32,
    pub ext_code: Vec<u32>,
    pub card_page: Option<String>,
    pub props: HashMap<String, String>,
    pub view: TableView,
    pub layout: Vec<Control>,
    pub actions: Vec<Control>,
    /// Page-level properties by lower-cased name, rendered as [`prop_value_src`] does.
    pub prop_src: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Subscriber {
    pub proc: ProcRef,
    pub skip_missing_license: bool,
}

#[derive(Default)]
pub struct Catalog {
    pub code: Vec<CodeObject>,
    pub code_by_key: HashMap<ObjKey, u32>,
    /// Object id by (kind, lower-cased name), context-free: for a name several objects share,
    /// the one most apps see (crate::names).
    pub names: HashMap<(ObjectKind, String), i64>,
    /// The objects behind each name several objects of a kind share (crate::names).
    pub name_cands: HashMap<(ObjectKind, String), Vec<crate::names::NameCand>>,
    /// Object id by (kind, lower-cased "namespace.name"), for namespace-qualified references.
    pub qualified_names: HashMap<(ObjectKind, String), i64>,
    /// Install ranks and dependency visibility of the loaded apps (crate::names).
    pub app_graph: crate::names::AppGraph,
    /// While building: the scope of the file whose object is being added (`id_of` resolves
    /// shared names in it). None afterwards.
    build_scope: Option<Arc<crate::names::NameScope>>,
    pub tables: HashMap<i64, TableDef>,
    pub pages: HashMap<i64, PageDef>,
    pub enums: HashMap<i64, EnumDef>,
    pub interfaces: HashMap<String, u32>,
    /// (publisher kind, publisher id, event lower, element lower) → subscribers
    pub subscribers: HashMap<(ObjectKind, i64, String, String), Vec<Subscriber>>,
    pub warnings: Vec<String>,
    pub stats: LoadStats,
    /// Profile objects (the platform lists them in table "All Profile").
    pub profiles: Vec<ProfileDef>,
    /// PermissionSet objects (the platform lists them in the permission set system tables).
    pub permission_sets: Vec<PermissionSetDef>,
    /// PermissionSetExtension objects (al-runtime permissions.rs merges them into their set).
    pub permission_set_exts: Vec<PermissionSetExtDef>,
    /// The `Permissions` property of every other object (its indirect permissions), raw tokens.
    pub object_permissions: HashMap<ObjKey, Vec<Token>>,
    /// The `InherentPermissions` property of every object, raw tokens.
    pub inherent_permissions: HashMap<ObjKey, Vec<Token>>,
    /// Query objects: data items, columns, filters, order.
    pub queries: HashMap<i64, QueryDef>,
    /// Report objects: dataset, request page, layouts (report extensions merged).
    pub reports: HashMap<i64, crate::report::ReportDef>,
    /// XMLport objects: schema tree, format, triggers.
    pub xmlports: HashMap<i64, crate::xmlport::XmlPortDef>,
    /// DotNet type aliases from `dotnet { assembly(...) { type("Full.Name"; Alias) } }` blocks:
    /// lower-cased alias → .NET type name.
    pub dotnet_types: HashMap<String, String>,
    /// The app directories the catalog was loaded from (each one app, with its app.json).
    pub app_dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryColumn {
    pub name: String,
    pub field: String,
    /// Count, Sum, Average, Min, Max (lower-cased), None for a plain column.
    pub method: Option<String>,
    pub reverse_sign: bool,
    /// ColumnFilter property (filter text).
    pub filter: Option<String>,
    /// Caption property (Query.ColumnCaption; else the field's caption).
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryFilter {
    pub name: String,
    pub field: String,
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryDataItem {
    pub name: String,
    pub table: String,
    /// Index of the parent data item (None for the first).
    pub parent: Option<usize>,
    /// DataItemLink: (this item's field, linked data item name, its field).
    pub links: Vec<(String, String, String)>,
    /// SqlJoinType lower-cased (default leftouterjoin).
    pub join: String,
    pub table_filter: Vec<Cond>,
    pub columns: Vec<QueryColumn>,
    pub filters: Vec<QueryFilter>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryDef {
    pub id: i64,
    pub name: String,
    pub dataitems: Vec<QueryDataItem>,
    /// OrderBy: (column name, descending).
    pub order_by: Vec<(String, bool)>,
    pub top: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionSetDef {
    pub id: i64,
    pub name: String,
    pub caption: String,
    pub assignable: bool,
    #[serde(skip)]
    pub file: Arc<PathBuf>,
    /// `Permissions`, `IncludedPermissionSets`, `ExcludedPermissionSets` (raw tokens).
    #[serde(skip)]
    pub permissions: Vec<Token>,
    #[serde(skip)]
    pub included: Vec<Token>,
    #[serde(skip)]
    pub excluded: Vec<Token>,
}

/// A PermissionSetExtension object: more permissions and included sets for `extends`.
#[derive(Debug, Clone)]
pub struct PermissionSetExtDef {
    pub id: i64,
    pub name: String,
    pub extends: String,
    pub permissions: Vec<Token>,
    pub included: Vec<Token>,
    pub excluded: Vec<Token>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileDef {
    pub name: String,
    pub caption: String,
    pub description: String,
    pub role_center: Option<i64>,
    pub enabled: bool,
    pub promoted: bool,
}

#[derive(Default, Debug, Serialize, Clone)]
pub struct LoadStats {
    pub files: usize,
    pub lines: usize,
    pub objects: usize,
    pub parse_errors: usize,
    pub parse_ms: u128,
    pub build_ms: u128,
}

fn proc_ref(code: u32, proc: u32) -> ProcRef {
    ProcRef { code, proc }
}

impl Catalog {
    pub fn load(dirs: &[PathBuf]) -> anyhow::Result<Catalog> {
        let t0 = std::time::Instant::now();
        let mut files = Vec::new();
        for d in dirs {
            crate::loader::collect_al_files(d, &mut files);
        }
        // BC_AL_SYMBOLS=A,B: preprocessor symbols defined for every file (an app.json's
        // preprocessorSymbols, or a build's /define, e.g. BC27PLUS,BC28PLUS,BC29PLUS)
        let syms: HashSet<String> = std::env::var("BC_AL_SYMBOLS").map(|v| v.split(',').map(|x| x.trim().to_ascii_uppercase()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
        let parsed: Vec<(PathBuf, usize, Result<SourceFile, SyntaxError>)> = files
            .into_par_iter()
            .map(|f| {
                let src = std::fs::read_to_string(&f).unwrap_or_default();
                let lines = src.lines().count();
                let r = parse_file(&src, &syms);
                (f, lines, r)
            })
            .collect();
        let parse_ms = t0.elapsed().as_millis();
        let t1 = std::time::Instant::now();
        let mut cat = Catalog::default();
        let mut ok = Vec::new();
        for (f, lines, r) in parsed {
            cat.stats.files += 1;
            cat.stats.lines += lines;
            match r {
                Ok(sf) => ok.push((Arc::new(f), sf)),
                Err(e) => {
                    cat.stats.parse_errors += 1;
                    cat.warnings.push(format!("{}:{}", f.display(), e));
                }
            }
        }
        cat.app_dirs = dirs.to_vec();
        cat.build(ok);
        cat.stats.parse_ms = parse_ms;
        cat.stats.build_ms = t1.elapsed().as_millis();
        Ok(cat)
    }

    pub fn id_of(&self, kind: ObjectKind, name_or_id: &str) -> Option<i64> {
        if let Ok(id) = name_or_id.parse::<i64>() {
            return Some(id);
        }
        let lower = name_or_id.to_ascii_lowercase();
        if let Some(scope) = &self.build_scope {
            if let Some(c) = self.name_cands.get(&(kind, lower.clone())).and_then(|c| self.app_graph.scoped_pick(scope, c)) {
                return Some(c.id);
            }
        }
        if let Some(id) = self.names.get(&(kind, lower.clone())) {
            return Some(*id);
        }
        // namespace-qualified: Microsoft.Finance.Dimension."Dimension Set Entry" names exactly one
        // object; else strip leading segments
        if lower.contains('.') {
            if let Some(id) = self.qualified_names.get(&(kind, lower.clone())) {
                return Some(*id);
            }
        }
        let mut rest = lower.as_str();
        while let Some(i) = rest.find('.') {
            rest = &rest[i + 1..];
            if let Some(id) = self.names.get(&(kind, rest.to_string())) {
                return Some(*id);
            }
        }
        None
    }
    pub fn table_by_name(&self, name: &str) -> Option<&TableDef> {
        self.id_of(ObjectKind::Table, name).and_then(|id| self.tables.get(&id))
    }
    pub fn page_by_name(&self, name: &str) -> Option<&PageDef> {
        self.id_of(ObjectKind::Page, name).and_then(|id| self.pages.get(&id))
    }
    pub fn enum_by_name(&self, name: &str) -> Option<&EnumDef> {
        self.id_of(ObjectKind::Enum, name).and_then(|id| self.enums.get(&id))
    }
    pub fn code_of(&self, kind: ObjectKind, id: i64) -> Option<&CodeObject> {
        self.code_by_key.get(&(kind, id)).map(|&i| &self.code[i as usize])
    }
    pub fn proc(&self, r: ProcRef) -> &Arc<Procedure> {
        &self.code[r.code as usize].procs[r.proc as usize]
    }

    fn build(&mut self, files: Vec<(Arc<PathBuf>, SourceFile)>) {
        // pass 1: names
        let apps = crate::apps::apps_of_dirs(&self.app_dirs);
        self.app_graph = crate::names::AppGraph::new(&apps);
        let mut objs: Vec<(Arc<PathBuf>, Option<String>, Arc<crate::names::NameScope>, Object)> = Vec::new();
        for (path, sf) in files {
            let scope = Arc::new(crate::names::NameScope::new(crate::apps::app_of_file(&apps, &path), sf.namespace.as_deref(), &sf.usings));
            for o in sf.objects {
                objs.push((path.clone(), sf.namespace.clone(), scope.clone(), o));
            }
        }
        self.stats.objects = objs.len();
        objs.retain(|(_, _, _, o)| !prop_word(&o.body, "ObsoleteState").is_some_and(|s| s.eq_ignore_ascii_case("moved")));
        self.register_names(&objs);
        // references to a shared name that resolves differently in their file than context-free:
        // pin them to the object id
        if !self.name_cands.is_empty() {
            let (cands, graph, names) = (&self.name_cands, &self.app_graph, &self.names);
            let shared: Vec<&str> = cands.keys().map(|(_, n)| n.as_str()).collect();
            objs.par_iter_mut().for_each(|(_, _, scope, o)| {
                crate::names::pin_object_refs(o, &mut |kind, name| {
                    if !shared.iter().any(|s| s.eq_ignore_ascii_case(name)) {
                        return None;
                    }
                    with_lower(name, |l| {
                        let key = (kind, l.to_string());
                        let pick = graph.scoped_pick(scope, cands.get(&key)?)?;
                        (names.get(&key) != Some(&pick.id)).then(|| pick.id.to_string())
                    })
                });
            });
        }
        // interfaces get synthetic ids
        let mut next_iface = 1_000_000_000i64;
        for (_, _, _, o) in &objs {
            if o.kind == ObjectKind::Interface {
                self.names.insert((o.kind, o.name.to_ascii_lowercase()), next_iface);
                next_iface += 1;
            }
        }
        // order: base objects before extensions; table and enum extensions before page (and
        // report) extensions, whose controls bind to the fields table extensions add
        objs.sort_by_key(|(_, _, _, o)| match o.kind {
            ObjectKind::Table | ObjectKind::Enum | ObjectKind::Interface => 0,
            ObjectKind::Page | ObjectKind::Codeunit | ObjectKind::Report | ObjectKind::Query | ObjectKind::XmlPort => 1,
            ObjectKind::TableExtension | ObjectKind::EnumExtension => 2,
            _ => 3,
        });
        for (path, ns, scope, o) in objs {
            // names in properties (SourceTable, TableNo, extends, ...) resolve in the file's scope
            self.build_scope = Some(scope);
            self.add_object(path, ns, o);
        }
        self.build_scope = None;
        self.bind_extension_fields();
        self.index_subscribers();
    }

    /// Fill `names` (context-free), `name_cands` (names several objects of a kind share) and
    /// `qualified_names` from the objects with ids (crate::names).
    fn register_names(&mut self, objs: &[(Arc<PathBuf>, Option<String>, Arc<crate::names::NameScope>, Object)]) {
        let mut all: HashMap<(ObjectKind, String), Vec<crate::names::NameCand>> = HashMap::new();
        for (seq, (_, _, scope, o)) in objs.iter().enumerate() {
            let Some(id) = o.id else { continue };
            let removed = prop_word(&o.body, "ObsoleteState").is_some_and(|s| s.eq_ignore_ascii_case("removed"));
            let lname = o.name.to_ascii_lowercase();
            if let Some(ns) = &scope.namespace {
                let q = self.qualified_names.entry((o.kind, format!("{ns}.{lname}"))).or_insert(id);
                if !removed {
                    *q = id;
                }
            }
            all.entry((o.kind, lname)).or_default().push(crate::names::NameCand {
                id,
                app: scope.app,
                namespace: scope.namespace.clone(),
                removed,
                seq: seq as u32,
            });
        }
        for (key, cands) in all {
            // the same object listed twice (an app folder loaded twice) is no collision
            if cands.iter().all(|c| c.id == cands[0].id) {
                self.names.insert(key, cands[0].id);
                continue;
            }
            self.names.insert(key.clone(), self.app_graph.global_pick(&cands).id);
            self.name_cands.insert(key, cands);
        }
    }

    /// Bind page field controls whose field a table extension adds: base pages are built before
    /// table extensions (see the load order), so `Rec."Ext Field"` found no field then.
    fn bind_extension_fields(&mut self) {
        fn walk(cs: &mut [Control], t: &TableDef) {
            for c in cs.iter_mut() {
                if c.kind == "field" && c.field.is_none() {
                    let fname = match &c.source_expr {
                        Some(Expr::Member(base, f)) if matches!(&**base, Expr::Ident(r) if r.eq_ignore_ascii_case("rec")) => Some(f.to_string()),
                        Some(Expr::Ident(f)) => Some(f.to_string()),
                        _ => None,
                    };
                    if let Some(fname) = fname {
                        c.field = t.field(&fname);
                    }
                }
                walk(&mut c.children, t);
            }
        }
        let tables = &self.tables;
        for page in self.pages.values_mut() {
            if let Some(t) = page.source_table.and_then(|tid| tables.get(&tid)) {
                walk(&mut page.layout, t);
            }
        }
    }

    fn new_code(&mut self, o: &Object, path: Arc<PathBuf>, ns: Option<String>, table: Option<i64>, extends: Option<ObjKey>) -> u32 {
        let id = match o.kind {
            ObjectKind::Interface => self.names.get(&(o.kind, o.name.to_ascii_lowercase())).copied().unwrap_or(0),
            _ => o.id.unwrap_or(0),
        };
        let mut co = CodeObject {
            kind: o.kind,
            id,
            name: o.name.clone(),
            namespace: ns,
            table,
            extends,
            globals: vec![],
            procs: vec![],
            by_name: HashMap::new(),
            single_instance: prop_bool(&o.body, "SingleInstance").unwrap_or(false),
            subtype: prop_word(&o.body, "Subtype"),
            implements: o.implements.clone(),
            file: path,
            manual_subscriber: prop_word(&o.body, "EventSubscriberInstance").is_some_and(|s| s.eq_ignore_ascii_case("manual")),
        };
        for m in &o.body.members {
            match m {
                Member::Vars { vars, .. } => co.globals.extend(vars.iter().cloned()),
                Member::Procedure(p) => {
                    let i = co.procs.len() as u32;
                    co.by_name.entry(p.name.to_ascii_lowercase()).or_default().push(i);
                    co.procs.push(Arc::new(p.clone()));
                }
                _ => {}
            }
        }
        let idx = self.code.len() as u32;
        self.code_by_key.insert((o.kind, id), idx);
        if o.kind == ObjectKind::Interface {
            self.interfaces.insert(o.name.to_ascii_lowercase(), idx);
        }
        self.code.push(co);
        idx
    }

    /// Register a nested trigger (field/control/action) as a non-callable procedure.
    pub(crate) fn add_nested_trigger(&mut self, code: u32, p: &Procedure) -> ProcRef {
        let co = &mut self.code[code as usize];
        let i = co.procs.len() as u32;
        co.procs.push(Arc::new(p.clone()));
        proc_ref(code, i)
    }

    pub(crate) fn nested_triggers(body: &Body) -> impl Iterator<Item = &Procedure> {
        body.members.iter().filter_map(|m| match m {
            Member::Procedure(p) if p.kind == ProcKind::Trigger => Some(p),
            _ => None,
        })
    }

    fn add_object(&mut self, path: Arc<PathBuf>, ns: Option<String>, o: Object) {
        // schema stubs left behind when an object moved to another app
        if prop_word(&o.body, "ObsoleteState").is_some_and(|s| s.eq_ignore_ascii_case("moved")) {
            return;
        }
        // indirect and inherent permissions of the object (al-runtime permissions.rs)
        if let (Some(id), false) = (o.id, matches!(o.kind, ObjectKind::PermissionSet | ObjectKind::PermissionSetExtension)) {
            if let Some(p) = prop(&o.body, "Permissions") {
                self.object_permissions.insert((o.kind, id), p.value.clone());
            }
            if let Some(p) = prop(&o.body, "InherentPermissions") {
                self.inherent_permissions.insert((o.kind, id), p.value.clone());
            }
        }
        match o.kind {
            ObjectKind::Table => self.add_table(path, ns, &o),
            ObjectKind::TableExtension => self.add_table_ext(path, ns, &o),
            ObjectKind::Enum => self.add_enum(&o),
            ObjectKind::EnumExtension => self.add_enum_ext(&o),
            ObjectKind::Page => self.add_page(path, ns, &o),
            ObjectKind::PageExtension => self.add_page_ext(path, ns, &o),
            ObjectKind::Query => {
                self.add_query(&o);
                self.new_code(&o, path, ns, None, None);
            }
            ObjectKind::Codeunit | ObjectKind::Report | ObjectKind::XmlPort | ObjectKind::Interface => {
                let table = match o.kind {
                    ObjectKind::Codeunit => prop_word(&o.body, "TableNo").and_then(|t| self.id_of(ObjectKind::Table, &t)),
                    _ => None,
                };
                let code = self.new_code(&o, path, ns, table, None);
                if o.kind == ObjectKind::Report {
                    self.add_report(code, &o);
                }
                if o.kind == ObjectKind::XmlPort {
                    self.add_xmlport(code, &o);
                }
            }
            ObjectKind::PermissionSet => {
                self.permission_sets.push(PermissionSetDef {
                    id: o.id.unwrap_or(0),
                    caption: prop_str(&o.body, "Caption").unwrap_or_else(|| o.name.clone()),
                    name: o.name.clone(),
                    assignable: prop_bool(&o.body, "Assignable").unwrap_or(true),
                    file: path.clone(),
                    permissions: prop_tokens(&o.body, "Permissions"),
                    included: prop_tokens(&o.body, "IncludedPermissionSets"),
                    excluded: prop_tokens(&o.body, "ExcludedPermissionSets"),
                });
            }
            ObjectKind::PermissionSetExtension => {
                self.permission_set_exts.push(PermissionSetExtDef {
                    id: o.id.unwrap_or(0),
                    name: o.name.clone(),
                    extends: o.extends.first().cloned().unwrap_or_default(),
                    permissions: prop_tokens(&o.body, "Permissions"),
                    included: prop_tokens(&o.body, "IncludedPermissionSets"),
                    excluded: prop_tokens(&o.body, "ExcludedPermissionSets"),
                });
            }
            ObjectKind::Profile => {
                let b = &o.body;
                let role_center = prop_word(b, "RoleCenter").and_then(|p| self.id_of(ObjectKind::Page, &p));
                self.profiles.push(ProfileDef {
                    caption: prop_str(b, "Caption").unwrap_or_else(|| o.name.clone()),
                    name: o.name.clone(),
                    description: prop_str(b, "ProfileDescription").unwrap_or_default(),
                    role_center,
                    enabled: prop_bool(b, "Enabled").unwrap_or(true),
                    promoted: prop_bool(b, "Promoted").unwrap_or(false),
                });
            }
            ObjectKind::ReportExtension => {
                let base = o.extends.first().and_then(|b| self.id_of(ObjectKind::Report, b)).map(|id| (ObjectKind::Report, id));
                let code = self.new_code(&o, path, ns, None, base);
                self.add_report_ext(code, &o);
            }
            ObjectKind::DotNet => self.add_dotnet(&o.body),
            _ => {}
        }
    }

    /// `assembly("A") { type("Full.Type.Name"; Alias) { } }`: remember alias → type name. When two
    /// apps declare the same alias for different types, the first declaration wins.
    fn add_dotnet(&mut self, body: &Body) {
        for asm in body.members.iter().filter_map(|m| match m {
            Member::Element(e) if e.kind == "assembly" => Some(e),
            _ => None,
        }) {
            for m in &asm.body.members {
                let Member::Element(t) = m else { continue };
                if t.kind != "type" || t.args.len() < 2 {
                    continue;
                }
                let full: String = t.args[0].iter().map(|k| k.text.as_ref()).collect();
                let alias: String = t.args[1].iter().map(|k| k.text.as_ref()).collect();
                if full.is_empty() || alias.is_empty() {
                    continue;
                }
                self.dotnet_types.entry(alias.to_ascii_lowercase()).or_insert(full);
            }
        }
    }

    // ------------------------------------------------------------------ tables

    fn field_type(t: &TypeRef) -> FieldType {
        match t {
            TypeRef::Simple { name, len } => match name.to_ascii_lowercase().as_str() {
                "integer" => FieldType::Integer,
                "biginteger" => FieldType::BigInteger,
                "decimal" => FieldType::Decimal,
                "boolean" => FieldType::Boolean,
                "code" => FieldType::Code(len.unwrap_or(250)),
                "text" => FieldType::Text(len.unwrap_or(2048)),
                "date" => FieldType::Date,
                "time" => FieldType::Time,
                "datetime" => FieldType::DateTime,
                "duration" => FieldType::Duration,
                "dateformula" => FieldType::DateFormula,
                "guid" => FieldType::Guid,
                "blob" => FieldType::Blob,
                "media" => FieldType::Media,
                "mediaset" => FieldType::MediaSet,
                "recordid" => FieldType::RecordId,
                "tablefilter" => FieldType::TableFilter,
                o => FieldType::Other(o.to_string()),
            },
            TypeRef::Option(_) => FieldType::Option,
            TypeRef::Object { kind, target, .. } if kind == "enum" => FieldType::Enum(target.clone()),
            other => FieldType::Other(other.base_name()),
        }
    }

    fn make_field(&mut self, el: &Element, code: u32, ext: Option<i64>) -> Option<FieldDef> {
        let id: i64 = el.args.first()?.first()?.text.parse().ok()?;
        let name = arg_name(el, 1)?;
        let ty = el.args.get(2).and_then(|a| parse_type(a).ok()).map(|t| Self::field_type(&t)).unwrap_or(FieldType::Other("?".into()));
        let b = &el.body;
        let class = match prop_word(b, "FieldClass").map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("flowfield") => FieldClass::FlowField,
            Some("flowfilter") => FieldClass::FlowFilter,
            _ => FieldClass::Normal,
        };
        let mut f = FieldDef {
            id,
            caption: prop_str(b, "Caption").unwrap_or_else(|| name.clone()),
            name,
            ty,
            class,
            calc: prop(b, "CalcFormula").and_then(|p| parse_calc_formula(&p.value)),
            init_value: prop(b, "InitValue").map(|p| tokens_text(&p.value)),
            options: prop(b, "OptionMembers").map(|p| split_commas(&p.value).into_iter().map(tokens_text).collect()).unwrap_or_default(),
            option_captions: prop_str(b, "OptionCaption").map(|s| s.split(',').map(|x| x.to_string()).collect()).unwrap_or_default(),
            relation: prop(b, "TableRelation").map(|p| parse_table_relation(&p.value)).unwrap_or_default(),
            validate_relation: prop_bool(b, "ValidateTableRelation").unwrap_or(true),
            editable: prop_bool(b, "Editable").unwrap_or(true),
            not_blank: prop_bool(b, "NotBlank").unwrap_or(false),
            auto_increment: prop_bool(b, "AutoIncrement").unwrap_or(false),
            blank_zero: prop_bool(b, "BlankZero").unwrap_or(false),
            tooltip: prop_str(b, "ToolTip"),
            decimal_places: prop(b, "DecimalPlaces").map(|p| tokens_text(&p.value)),
            on_validate: None,
            before_validate: vec![],
            after_validate: vec![],
            on_lookup: None,
            from_extension: ext,
            prop_src: props(b).map(|p| (p.name.to_ascii_lowercase(), prop_value_src(p))).collect(),
        };
        if f.option_captions.is_empty() {
            f.option_captions = f.options.clone();
        }
        let trig: Vec<Procedure> = Self::nested_triggers(b).cloned().collect();
        for t in trig {
            let r = self.add_nested_trigger(code, &t);
            match t.name.to_ascii_lowercase().as_str() {
                "onvalidate" => f.on_validate = Some(r),
                "onlookup" => f.on_lookup = Some(r),
                _ => {}
            }
        }
        Some(f)
    }

    fn is_removed(b: &Body) -> bool {
        prop_word(b, "ObsoleteState").is_some_and(|s| s.eq_ignore_ascii_case("removed"))
    }

    fn add_table(&mut self, path: Arc<PathBuf>, ns: Option<String>, o: &Object) {
        let Some(id) = o.id else { return };
        let code = self.new_code(o, path, ns.clone(), Some(id), None);
        let b = &o.body;
        let mut t = TableDef {
            id,
            name: o.name.clone(),
            caption: prop_str(b, "Caption").unwrap_or_else(|| o.name.clone()),
            namespace: ns,
            code,
            ext_code: vec![],
            fields: vec![],
            field_by_name: HashMap::new(),
            field_by_id: HashMap::new(),
            keys: vec![],
            table_type: prop_word(b, "TableType").unwrap_or_else(|| "Normal".into()),
            data_per_company: prop_bool(b, "DataPerCompany").unwrap_or(true),
            temporary_only: prop_word(b, "TableType").is_some_and(|s| s.eq_ignore_ascii_case("temporary")),
            lookup_page: prop_word(b, "LookupPageID").or_else(|| prop_word(b, "LookupPageId")),
            drilldown_page: prop_word(b, "DrillDownPageID").or_else(|| prop_word(b, "DrillDownPageId")),
            data_caption_fields: prop_list(b, "DataCaptionFields"),
            triggers: TableTriggers::default(),
            obsolete_removed: Self::is_removed(b),
            field_groups: vec![],
            prop_src: props(b).map(|p| (p.name.to_ascii_lowercase(), prop_value_src(p))).collect(),
        };
        let co = &self.code[code as usize];
        for (name, slot) in [
            ("oninsert", &mut t.triggers.on_insert),
            ("onmodify", &mut t.triggers.on_modify),
            ("ondelete", &mut t.triggers.on_delete),
            ("onrename", &mut t.triggers.on_rename),
        ] {
            *slot = co.trigger(name).map(|i| proc_ref(code, i));
        }
        let mut key_els = Vec::new();
        for el in elements(b) {
            match el.kind.as_str() {
                "fields" => {
                    for fe in elements(&el.body) {
                        if fe.kind == "field" && !Self::is_removed(&fe.body) {
                            if let Some(f) = self.make_field(fe, code, None) {
                                t.add_field(f);
                            }
                        }
                    }
                }
                "keys" => key_els.extend(elements(&el.body).filter(|k| k.kind == "key").cloned()),
                "fieldgroups" => {
                    for g in elements(&el.body).filter(|g| g.kind == "fieldgroup") {
                        t.field_groups.push(FieldGroupDef {
                            name: arg_name(g, 0).unwrap_or_default(),
                            caption: prop_str(&g.body, "Caption"),
                            fields: field_group_names(g),
                        });
                    }
                }
                _ => {}
            }
        }
        for (fid, name, ty) in [
            (2000000000i64, "SystemId", FieldType::Guid),
            (2000000001, "SystemCreatedAt", FieldType::DateTime),
            (2000000002, "SystemCreatedBy", FieldType::Guid),
            (2000000003, "SystemModifiedAt", FieldType::DateTime),
            (2000000004, "SystemModifiedBy", FieldType::Guid),
            (2000000005, "SystemRowVersion", FieldType::BigInteger),
        ] {
            if t.field(name).is_none() {
                t.add_field(FieldDef::system(fid, name, ty));
            }
        }
        for k in &key_els {
            t.add_key(k, &mut self.warnings);
        }
        if t.keys.is_empty() && !t.fields.is_empty() {
            t.keys.push(KeyDef {
                name: "PK".into(),
                fields: vec![0],
                sum_fields: vec![],
                clustered: true,
                unique: true,
                enabled: true,
                maintain_sql_index: true,
                maintain_sift_index: true,
            });
        }
        self.tables.insert(id, t);
    }

    fn add_table_ext(&mut self, path: Arc<PathBuf>, ns: Option<String>, o: &Object) {
        let Some(base) = o.extends.first().and_then(|b| self.id_of(ObjectKind::Table, b)) else { return };
        if !self.tables.contains_key(&base) {
            self.warnings.push(format!("tableextension {} extends unknown table {base}", o.name));
            return;
        }
        let code = self.new_code(o, path, ns, Some(base), Some((ObjectKind::Table, base)));
        let mut new_fields = Vec::new();
        let mut mods: Vec<(String, Vec<(String, ProcRef)>)> = Vec::new();
        let mut key_els = Vec::new();
        let mut group_edits: Vec<(String, String, Option<String>, Vec<String>)> = Vec::new();
        for el in elements(&o.body) {
            match el.kind.as_str() {
                "fields" => {
                    for fe in elements(&el.body) {
                        if fe.kind == "field" && !Self::is_removed(&fe.body) {
                            if let Some(f) = self.make_field(fe, code, o.id) {
                                new_fields.push(f);
                            }
                        } else if fe.kind == "modify" {
                            let fname = arg_name(fe, 0).unwrap_or_default();
                            let trig: Vec<Procedure> = Self::nested_triggers(&fe.body).cloned().collect();
                            let refs = trig.iter().map(|t| (t.name.to_ascii_lowercase(), self.add_nested_trigger(code, t))).collect();
                            mods.push((fname, refs));
                        }
                    }
                }
                "keys" => key_els.extend(elements(&el.body).filter(|k| k.kind == "key").cloned()),
                "fieldgroups" => {
                    // addlast(DropDown; "Field") / addfirst / addafter / addbefore, or new groups
                    for g in elements(&el.body) {
                        group_edits.push((g.kind.clone(), arg_name(g, 0).unwrap_or_default(), prop_str(&g.body, "Caption"), field_group_names(g)));
                    }
                }
                _ => {}
            }
        }
        let co = &self.code[code as usize];
        let ext_trigs: Vec<(String, ProcRef)> = co
            .procs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kind == ProcKind::Trigger)
            .map(|(i, p)| (p.name.to_ascii_lowercase(), proc_ref(code, i as u32)))
            .collect();
        let t = self.tables.get_mut(&base).unwrap();
        t.ext_code.push(code);
        for f in new_fields {
            t.add_field(f);
        }
        for (fname, refs) in mods {
            if let Some(fi) = t.field(&fname) {
                for (tn, r) in refs {
                    match tn.as_str() {
                        "onbeforevalidate" => t.fields[fi].before_validate.push(r),
                        "onaftervalidate" => t.fields[fi].after_validate.push(r),
                        _ => {}
                    }
                }
            }
        }
        t.triggers.ext.extend(ext_trigs);
        for k in &key_els {
            t.add_key(k, &mut self.warnings);
        }
        for (kind, name, caption, fields) in group_edits {
            match t.field_groups.iter_mut().find(|g| g.name.eq_ignore_ascii_case(&name)) {
                Some(g) if kind == "addfirst" => {
                    g.fields.splice(0..0, fields);
                }
                Some(g) => g.fields.extend(fields),
                None => t.field_groups.push(FieldGroupDef { name, caption, fields }),
            }
        }
    }

    // ------------------------------------------------------------------ enums

    fn enum_values(b: &Body) -> Vec<EnumValue> {
        elements(b)
            .filter(|e| e.kind == "value")
            .filter_map(|e| {
                let ordinal = e.args.first()?.first()?.text.parse().ok()?;
                let name = arg_name(e, 1)?;
                let implementations = prop(&e.body, "Implementation").map(|p| impl_pairs(&p.value)).unwrap_or_default();
                Some(EnumValue { ordinal, caption: prop_str(&e.body, "Caption").unwrap_or_else(|| name.clone()), name, implementations })
            })
            .collect()
    }

    fn add_enum(&mut self, o: &Object) {
        let Some(id) = o.id else { return };
        let b = &o.body;
        self.enums.insert(
            id,
            EnumDef {
                id,
                name: o.name.clone(),
                caption: prop_str(b, "Caption").unwrap_or_else(|| o.name.clone()),
                extensible: prop_bool(b, "Extensible").unwrap_or(false),
                implements: o.implements.clone(),
                values: Self::enum_values(b),
                default_implementations: prop(b, "DefaultImplementation").map(|p| impl_pairs(&p.value)).unwrap_or_default(),
            },
        );
    }

    fn add_enum_ext(&mut self, o: &Object) {
        let Some(base) = o.extends.first().and_then(|b| self.id_of(ObjectKind::Enum, b)) else { return };
        let vals = Self::enum_values(&o.body);
        if let Some(e) = self.enums.get_mut(&base) {
            e.values.extend(vals);
        }
    }

    // ------------------------------------------------------------------ pages

    pub(crate) fn control(&mut self, el: &Element, code: u32, table: Option<i64>) -> Control {
        let mut c = Control { kind: el.kind.clone(), name: arg_name(el, 0).unwrap_or_default(), ..Default::default() };
        c.caption = prop_str(&el.body, "Caption");
        match el.kind.as_str() {
            "field" => {
                if let Some(a) = el.args.get(1) {
                    c.source = Some(tokens_text(a));
                    c.source_expr = parse_expr(a).ok();
                    if let (Some(e), Some(tid)) = (&c.source_expr, table) {
                        let fname = match e {
                            Expr::Member(base, f) if matches!(&**base, Expr::Ident(r) if r.eq_ignore_ascii_case("rec")) => Some(f.to_string()),
                            Expr::Ident(f) => Some(f.to_string()),
                            _ => None,
                        };
                        if let Some(fname) = fname {
                            c.field = self.tables.get(&tid).and_then(|t| t.field(&fname));
                        }
                    }
                }
            }
            "part" | "systempart" => {
                c.part_page = arg_name(el, 1);
                c.sub_page_link = prop(&el.body, "SubPageLink").map(|p| parse_conds(&p.value)).unwrap_or_default();
            }
            _ => c.target = arg_name(el, 1),
        }
        for p in props(&el.body) {
            if !p.name.eq_ignore_ascii_case("caption") {
                let v = if p.value.len() == 1 && p.value[0].kind == Tk::Str { p.value[0].text.to_string() } else { tokens_text(&p.value) };
                c.props.insert(p.name.to_ascii_lowercase(), v);
                c.prop_src.insert(p.name.to_ascii_lowercase(), prop_value_src(p));
            }
        }
        let trig: Vec<Procedure> = Self::nested_triggers(&el.body).cloned().collect();
        for t in trig {
            let r = self.add_nested_trigger(code, &t);
            c.triggers.insert(t.name.to_ascii_lowercase(), r);
        }
        let kids: Vec<Element> = elements(&el.body).cloned().collect();
        for k in &kids {
            c.children.push(self.control(k, code, table));
        }
        c
    }

    pub(crate) fn page_sections(&mut self, b: &Body, code: u32, table: Option<i64>) -> (Vec<Control>, Vec<Control>) {
        let mut layout = vec![];
        let mut actions = vec![];
        let els: Vec<Element> = elements(b).cloned().collect();
        for el in &els {
            if el.kind != "layout" && el.kind != "actions" {
                continue;
            }
            let kids: Vec<Element> = elements(&el.body).cloned().collect();
            let built: Vec<Control> = kids.iter().map(|k| self.control(k, code, table)).collect();
            if el.kind == "layout" {
                layout.extend(built);
            } else {
                actions.extend(built);
            }
        }
        (layout, actions)
    }

    fn add_page(&mut self, path: Arc<PathBuf>, ns: Option<String>, o: &Object) {
        let Some(id) = o.id else { return };
        let b = &o.body;
        let table = prop_word(b, "SourceTable").and_then(|t| self.id_of(ObjectKind::Table, &t));
        let code = self.new_code(o, path, ns, table, None);
        let (layout, actions) = self.page_sections(b, code, table);
        let mut pp = HashMap::new();
        for p in props(b) {
            let v = if p.value.len() == 1 && p.value[0].kind == Tk::Str { p.value[0].text.to_string() } else { tokens_text(&p.value) };
            pp.insert(p.name.to_ascii_lowercase(), v);
        }
        self.pages.insert(
            id,
            PageDef {
                id,
                name: o.name.clone(),
                caption: prop_str(b, "Caption").unwrap_or_else(|| o.name.clone()),
                page_type: prop_word(b, "PageType").unwrap_or_else(|| "Card".into()),
                source_table: table,
                code,
                ext_code: vec![],
                card_page: prop_word(b, "CardPageID").or_else(|| prop_word(b, "CardPageId")),
                props: pp,
                view: prop(b, "SourceTableView").map(|p| parse_table_view(&p.value)).unwrap_or_default(),
                layout,
                actions,
                prop_src: props(b).map(|p| (p.name.to_ascii_lowercase(), prop_value_src(p))).collect(),
            },
        );
    }

    fn add_page_ext(&mut self, path: Arc<PathBuf>, ns: Option<String>, o: &Object) {
        let Some(base) = o.extends.first().and_then(|b| self.id_of(ObjectKind::Page, b)) else { return };
        let Some(table) = self.pages.get(&base).map(|p| p.source_table) else { return };
        let code = self.new_code(o, path, ns, table, Some((ObjectKind::Page, base)));
        let els: Vec<Element> = elements(&o.body).cloned().collect();
        let mut edits: Vec<(bool, Element, Vec<Control>)> = vec![];
        for el in &els {
            let is_layout = match el.kind.as_str() {
                "layout" => true,
                "actions" => false,
                _ => continue,
            };
            for change in elements(&el.body) {
                let kids: Vec<Element> = elements(&change.body).cloned().collect();
                let built = kids.iter().map(|k| self.control(k, code, table)).collect();
                edits.push((is_layout, change.clone(), built));
            }
        }
        let page = self.pages.get_mut(&base).unwrap();
        page.ext_code.push(code);
        for (is_layout, change, built) in edits {
            let tree = if is_layout { &mut page.layout } else { &mut page.actions };
            let anchor = arg_name(&change, 0).unwrap_or_default();
            match change.kind.as_str() {
                "addfirst" | "addlast" | "addafter" | "addbefore" => {
                    let mut items = Some(built);
                    let std_area = matches!(
                        anchor.to_ascii_lowercase().as_str(),
                        "content" | "factboxes" | "processing" | "navigation" | "reporting" | "creation" | "promoted" | "rolecenter" | "sections" | "embedding" | "prompting" | "systemactions"
                    );
                    // an area the base page does not declare is created (BC: addfirst(content) on a
                    // page with only factboxes, addlast(processing) on a page without actions)
                    if !apply_add(tree, &change.kind, &anchor, &mut items) && std_area && matches!(change.kind.as_str(), "addfirst" | "addlast") {
                        tree.push(Control { kind: "area".into(), name: anchor.clone(), children: items.take().unwrap_or_default(), ..Default::default() });
                    }
                }
                "modify" => {
                    if let Some(c) = find_control(tree, &anchor) {
                        for p in props(&change.body) {
                            let v = if p.value.len() == 1 && p.value[0].kind == Tk::Str { p.value[0].text.to_string() } else { tokens_text(&p.value) };
                            if p.name.eq_ignore_ascii_case("caption") {
                                c.caption = Some(v);
                            } else {
                                c.props.insert(p.name.to_ascii_lowercase(), v);
                                c.prop_src.insert(p.name.to_ascii_lowercase(), prop_value_src(p));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------ queries

    fn query_items(el: &Element, parent: Option<usize>, out: &mut Vec<QueryDataItem>) {
        let b = &el.body;
        let links = prop(b, "DataItemLink")
            .map(|p| {
                split_commas(&p.value)
                    .into_iter()
                    .filter_map(|g| {
                        let eq = g.iter().position(|t| t.kind == Tk::Eq)?;
                        let rhs = &g[eq + 1..];
                        let dot = rhs.iter().position(|t| t.kind == Tk::Dot)?;
                        Some((tokens_text(&g[..eq]), tokens_text(&rhs[..dot]), tokens_text(&rhs[dot + 1..])))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut item = QueryDataItem {
            name: arg_name(el, 0).unwrap_or_default(),
            table: arg_name(el, 1).unwrap_or_default(),
            parent,
            links,
            join: prop_word(b, "SqlJoinType").map(|s| s.to_ascii_lowercase()).unwrap_or_else(|| "leftouterjoin".into()),
            table_filter: prop(b, "DataItemTableFilter").map(|p| parse_conds(&p.value)).unwrap_or_default(),
            columns: vec![],
            filters: vec![],
        };
        let mut children = vec![];
        for c in elements(b) {
            match c.kind.as_str() {
                "column" => item.columns.push(QueryColumn {
                    name: arg_name(c, 0).unwrap_or_default(),
                    field: arg_name(c, 1).unwrap_or_default(),
                    method: prop_word(&c.body, "Method").map(|s| s.to_ascii_lowercase()),
                    reverse_sign: prop_bool(&c.body, "ReverseSign").unwrap_or(false),
                    filter: prop(&c.body, "ColumnFilter").map(|p| column_filter_text(&p.value)),
                    caption: prop_str(&c.body, "Caption"),
                }),
                "filter" => item.filters.push(QueryFilter {
                    name: arg_name(c, 0).unwrap_or_default(),
                    field: arg_name(c, 1).unwrap_or_default(),
                    filter: prop(&c.body, "ColumnFilter").map(|p| column_filter_text(&p.value)),
                }),
                "dataitem" => children.push(c),
                _ => {}
            }
        }
        let me = out.len();
        out.push(item);
        for c in children {
            Self::query_items(c, Some(me), out);
        }
    }

    fn add_query(&mut self, o: &Object) {
        let Some(id) = o.id else { return };
        let mut dataitems = vec![];
        for el in elements(&o.body) {
            if el.kind == "elements" {
                for d in elements(&el.body).filter(|d| d.kind == "dataitem") {
                    let parent = if dataitems.is_empty() { None } else { Some(0) };
                    Self::query_items(d, parent, &mut dataitems);
                }
            }
        }
        // OrderBy = ascending(A, B), descending(C)
        let mut order_by = vec![];
        if let Some(p) = prop(&o.body, "OrderBy") {
            let mut desc = false;
            for t in p.value.iter() {
                match t.kind {
                    Tk::Ident if t.text.eq_ignore_ascii_case("ascending") => desc = false,
                    Tk::Ident if t.text.eq_ignore_ascii_case("descending") => desc = true,
                    Tk::Ident | Tk::QIdent => order_by.push((t.text.to_string(), desc)),
                    _ => {}
                }
            }
        }
        let top = prop(&o.body, "TopNumberOfRows").and_then(|p| tokens_text(&p.value).parse().ok());
        self.queries.insert(id, QueryDef { id, name: o.name.clone(), dataitems, order_by, top });
    }

    // ------------------------------------------------------------------ events

    fn index_subscribers(&mut self) {
        let mut subs = Vec::new();
        for (ci, co) in self.code.iter().enumerate() {
            if co.kind != ObjectKind::Codeunit {
                continue;
            }
            for (pi, p) in co.procs.iter().enumerate() {
                for a in &p.attributes {
                    if !a.name.eq_ignore_ascii_case("eventsubscriber") || a.args.len() < 3 {
                        continue;
                    }
                    let kind = match &a.args[0] {
                        Expr::Scope(_, k) => k.to_ascii_lowercase(),
                        _ => continue,
                    };
                    let pkind = match kind.as_str() {
                        "table" => ObjectKind::Table,
                        "codeunit" => ObjectKind::Codeunit,
                        "page" => ObjectKind::Page,
                        "report" => ObjectKind::Report,
                        "xmlport" => ObjectKind::XmlPort,
                        "query" => ObjectKind::Query,
                        _ => continue,
                    };
                    // Codeunit::"X", or namespace-qualified Codeunit::System.Globalization.Language
                    let pname = match &a.args[1] {
                        Expr::Scope(_, n) | Expr::Member(_, n) => n.to_string(),
                        _ => continue,
                    };
                    // the event and element names may be quoted ('OnX'), bare (OnX) or "OnX"
                    let event = match &a.args[2] {
                        Expr::Str(s) | Expr::Ident(s) => s.to_ascii_lowercase(),
                        _ => continue,
                    };
                    let element = match a.args.get(3) {
                        Some(Expr::Str(s) | Expr::Ident(s)) => s.to_ascii_lowercase(),
                        _ => String::new(),
                    };
                    let skip = matches!(a.args.get(4), Some(Expr::Bool(true)));
                    subs.push((pkind, pname, event, element, ProcRef { code: ci as u32, proc: pi as u32 }, skip));
                }
            }
        }
        for (pkind, pname, event, element, r, skip) in subs {
            match self.id_of(pkind, &pname) {
                Some(pid) => self
                    .subscribers
                    .entry((pkind, pid, event, element))
                    .or_default()
                    .push(Subscriber { proc: r, skip_missing_license: skip }),
                None => self.warnings.push(format!("subscriber to unknown publisher {pname}")),
            }
        }
    }
}

impl FieldDef {
    pub fn system(id: i64, name: &str, ty: FieldType) -> FieldDef {
        FieldDef {
            id,
            name: name.into(),
            caption: name.into(),
            ty,
            class: FieldClass::Normal,
            calc: None,
            init_value: None,
            options: vec![],
            option_captions: vec![],
            relation: vec![],
            validate_relation: true,
            editable: false,
            not_blank: false,
            auto_increment: false,
            blank_zero: false,
            tooltip: None,
            decimal_places: None,
            on_validate: None,
            before_validate: vec![],
            after_validate: vec![],
            on_lookup: None,
            from_extension: None,
            prop_src: HashMap::new(),
        }
    }
    pub fn is_system(&self) -> bool {
        self.id >= 2000000000
    }
}

impl TableDef {
    fn add_field(&mut self, f: FieldDef) {
        // a field that moved into an extension replaces the base table's stub with the same id
        if let Some(&i) = self.field_by_id.get(&f.id) {
            let old = self.fields[i].name.to_ascii_lowercase();
            self.field_by_name.remove(&old);
            self.field_by_name.insert(f.name.to_ascii_lowercase(), i);
            self.fields[i] = f;
            return;
        }
        let i = self.fields.len();
        self.field_by_name.insert(f.name.to_ascii_lowercase(), i);
        self.field_by_id.insert(f.id, i);
        self.fields.push(f);
    }

    fn add_key(&mut self, k: &Element, warnings: &mut Vec<String>) {
        let name = arg_name(k, 0).unwrap_or_default();
        let mut fields = vec![];
        if let Some(a) = k.args.get(1) {
            for g in split_commas(a) {
                let fname = tokens_text(g);
                match self.field(&fname) {
                    Some(i) => fields.push(i),
                    None => {
                        warnings.push(format!("table {}: key {name} references unknown field {fname}", self.name));
                        return;
                    }
                }
            }
        }
        let b = &k.body;
        let sum_fields = prop_list(b, "SumIndexFields").iter().filter_map(|f| self.field(f)).collect();
        self.keys.push(KeyDef {
            name,
            fields,
            sum_fields,
            clustered: prop_bool(b, "Clustered").unwrap_or(false),
            unique: prop_bool(b, "Unique").unwrap_or(false),
            enabled: prop_bool(b, "Enabled").unwrap_or(true),
            maintain_sql_index: prop_bool(b, "MaintainSqlIndex").unwrap_or(true),
            maintain_sift_index: prop_bool(b, "MaintainSiftIndex").unwrap_or(true),
        });
    }
}

/// Field names of `fieldgroup(Name; "A", B)` (second element argument).
fn field_group_names(g: &Element) -> Vec<String> {
    g.args.get(1).map(|a| split_commas(a).into_iter().filter(|t| !t.is_empty()).map(tokens_text).collect()).unwrap_or_default()
}

/// ColumnFilter = Col = filter(<>''), ... → the filter text inside filter()/const().
fn column_filter_text(toks: &[Token]) -> String {
    let conds = parse_conds(toks);
    match conds.first() {
        Some(c) => c.value.clone(),
        None => tokens_text(toks),
    }
}

fn impl_pairs(toks: &[Token]) -> Vec<(String, String)> {
    split_commas(toks)
        .into_iter()
        .filter_map(|g| {
            let eq = g.iter().position(|t| t.kind == Tk::Eq)?;
            Some((tokens_text(&g[..eq]), tokens_text(&g[eq + 1..])))
        })
        .collect()
}

pub fn find_control<'a>(tree: &'a mut [Control], name: &str) -> Option<&'a mut Control> {
    for c in tree.iter_mut() {
        if c.name.eq_ignore_ascii_case(name) {
            return Some(c);
        }
        if let Some(x) = find_control(&mut c.children, name) {
            return Some(x);
        }
    }
    None
}

pub(crate) fn apply_add(tree: &mut Vec<Control>, how: &str, anchor: &str, items: &mut Option<Vec<Control>>) -> bool {
    if let Some(pos) = tree.iter().position(|c| c.name.eq_ignore_ascii_case(anchor)) {
        let items = items.take().unwrap_or_default();
        match how {
            "addfirst" => {
                let c = &mut tree[pos].children;
                for (i, it) in items.into_iter().enumerate() {
                    c.insert(i, it);
                }
            }
            "addlast" => tree[pos].children.extend(items),
            "addafter" => {
                for (i, it) in items.into_iter().enumerate() {
                    tree.insert(pos + 1 + i, it);
                }
            }
            _ => {
                for (i, it) in items.into_iter().enumerate() {
                    tree.insert(pos + i, it);
                }
            }
        }
        return true;
    }
    tree.iter_mut().any(|c| apply_add(&mut c.children, how, anchor, items))
}

/// The Test Framework: Library Assert, Any, Library - Variable Storage, Permissions Mock, Test Runner.
pub fn test_framework_dirs(root: &Path) -> Vec<PathBuf> {
    let fw = root.join("vendor/bcapps/src/Tools/Test Framework");
    vec![
        fw.join("Test Libraries/Assert"),
        fw.join("Test Libraries/Any"),
        fw.join("Test Libraries/Variable Storage"),
        fw.join("Test Libraries/Permissions Mock"),
        fw.join("Test Runner"),
    ]
}

/// Test suites that can be loaded next to the default apps.
pub const TEST_SUITES: &[&str] = &["system", "foundation", "baseapp"];

/// Test apps (and the test libraries they need) for a suite: `system` (System Application tests),
/// `foundation` (Business Foundation tests) or `baseapp` (the W1 Base App tests). `baseapp:ERM,SCM`
/// limits the Base App tests to those areas under Layers/W1/Tests. Returns (library dirs, test dirs).
pub fn test_suite_dirs(root: &Path, suite: &str) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let s = root.join("vendor/bcapps/src");
    let sys_lib = s.join("System Application/Test Library");
    let bf_lib = s.join("Business Foundation/Test Library");
    let (name, areas) = suite.split_once(':').unwrap_or((suite, ""));
    match name.to_ascii_lowercase().as_str() {
        "system" => (vec![sys_lib], vec![s.join("System Application/Test")]),
        "foundation" => (vec![sys_lib, bf_lib], vec![s.join("Business Foundation/Test")]),
        "baseapp" => {
            let tests = s.join("Layers/W1/Tests");
            let libs = vec![sys_lib, bf_lib, tests.join("ApplicationTestLibrary"), tests.join("TestLibraries")];
            let mut dirs = vec![];
            if areas.is_empty() {
                let mut all: Vec<PathBuf> = std::fs::read_dir(&tests).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
                all.sort();
                dirs.extend(all.into_iter().filter(|p| !libs.contains(p)));
            } else {
                dirs.extend(areas.split(',').map(|a| tests.join(a.trim())));
            }
            (libs, dirs)
        }
        _ => (vec![], vec![]),
    }
}

/// `default_app_dirs` plus extra app directories (deduplicated, order kept).
pub fn app_dirs_with(root: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = default_app_dirs(root);
    for d in extra {
        if !out.contains(d) {
            out.push(d.clone());
        }
    }
    out
}

/// Microsoft's demo-data tool: the W1 "Contoso Coffee Demo Dataset" app. Its codeunit 5193
/// "Contoso Demo Tool".CreateAllDemoData() is what Microsoft's build scripts run to create the
/// CRONUS demo company (all modules: Foundation, Finance, Bank, CRM, Inventory, Purchase, Sales,
/// Warehouse, Manufacturing, Service, Fixed Assets, HR, Jobs, EService, Analytics).
pub fn demo_tool_dirs(root: &Path) -> Vec<PathBuf> {
    vec![root.join("vendor/bcapps/src/Apps/W1/ContosoCoffeeDemoDataset/app")]
}

/// Microsoft's legacy W1 demo-data tool ("DemoTool" app, Layers/W1/DemoTool): codeunit 101899
/// "Create Demo Data from Config" runs codeunit 101900 "Create Demonstration Data". BCApps' CI
/// creates the "CRONUS International Ltd." company of its Legacy test runs (all the Tests-* apps
/// under Layers/W1/Tests) with it, data type Extended; its resource files (DemoDataConfig.xml,
/// Pictures, o365) are in `w1_demo_tool_resources`.
pub fn w1_demo_tool_dirs(root: &Path) -> Vec<PathBuf> {
    vec![root.join("vendor/bcapps/src/Layers/W1/DemoTool")]
}

/// The legacy W1 demo tool's resource folder (src/DemoTool: DemoDataConfig.xml, Pictures, o365),
/// which BCApps' build stages into the session's temp folder before running the tool.
pub fn w1_demo_tool_resources(root: &Path) -> PathBuf {
    root.join("vendor/bcapps/src/DemoTool")
}

pub fn default_app_dirs(root: &Path) -> Vec<PathBuf> {
    let s = root.join("vendor/bcapps/src");
    vec![
        root.join("platform"),
        s.join("System Application/App"),
        s.join("Business Foundation/App"),
        s.join("Layers/W1/BaseApp"),
        s.join("Apps/W1/APIV2/app"),
    ]
}
