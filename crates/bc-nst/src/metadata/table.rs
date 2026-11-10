//! `MetaTable` / `MetaField` / `MetaKey` / `MetaFieldGroup` (plain JSON with C# member
//! names, no `$polyType`), plus the field-type helpers the page solver shares.

use super::alprop::{self, enum_val};
use al_model::{CalcMethod, Catalog, Cond, CondKind, EnumDef, FieldClass, FieldDef, FieldType, RelationBranch, TableDef};
use al_syntax::ObjectKind;
use serde_json::{json, Map, Value};

// NavType values (Microsoft.Dynamics.Nav.Types.NavType).
pub(crate) const META_OPTION: i64 = 35584;
pub(crate) const META_INTEGER: i64 = 34560;
pub(crate) const META_BIGINTEGER: i64 = 36096;
pub(crate) const META_DECIMAL: i64 = 12800;
pub(crate) const META_DATE: i64 = 11776;
pub(crate) const META_TIME: i64 = 11777;
pub(crate) const META_DATETIME: i64 = 37376;
pub(crate) const META_DURATION: i64 = 36864;

/// `MetaField.Type`. Enum fields go out as Option (35584) carrying `EnumTypeName`: the web
/// client only ever matches `NavType.Option` columns (ExpressionEngine, SolveDropDown) and
/// never `NavType.Enum`.
pub(crate) fn meta_type_code(ty: &FieldType) -> i64 {
    match ty {
        FieldType::Integer => META_INTEGER,
        FieldType::BigInteger => META_BIGINTEGER,
        FieldType::Decimal => META_DECIMAL,
        FieldType::Boolean => 34048,
        FieldType::Code(_) => 31490,
        FieldType::Text(_) => 31489,
        FieldType::Date => META_DATE,
        FieldType::Time => META_TIME,
        FieldType::DateTime => META_DATETIME,
        FieldType::Duration => META_DURATION,
        FieldType::DateFormula => 11798,
        FieldType::Guid => 37120,
        FieldType::Option | FieldType::Enum(_) => META_OPTION,
        FieldType::Blob => 33794,
        FieldType::Media => 26208,
        FieldType::MediaSet => 26209,
        FieldType::RecordId => 4989,
        FieldType::TableFilter => 4913,
        FieldType::Other(_) => 31489,
    }
}

/// `MetaField.ClrType`, resolved by the web server with `Type.GetType`. Media types need the
/// assembly-qualified name because they live in Microsoft.Dynamics.Nav.Types.
pub(crate) fn clr_type(ty: &FieldType) -> &'static str {
    match ty {
        FieldType::Integer | FieldType::Option | FieldType::Enum(_) => "System.Int32",
        FieldType::BigInteger => "System.Int64",
        FieldType::Decimal => "System.Decimal",
        FieldType::Boolean => "System.Boolean",
        FieldType::Date | FieldType::Time | FieldType::DateTime => "System.DateTime",
        FieldType::Duration => "System.TimeSpan",
        FieldType::Guid => "System.Guid",
        FieldType::Blob | FieldType::RecordId => "System.Byte[]",
        FieldType::Media => "Microsoft.Dynamics.Nav.Types.Media.NSMediaWrapper, Microsoft.Dynamics.Nav.Types",
        FieldType::MediaSet => "Microsoft.Dynamics.Nav.Types.Media.NSMediaSetWrapper, Microsoft.Dynamics.Nav.Types",
        _ => "System.String",
    }
}

/// `DataType` numbering used by `DataFieldDefinition.Datatype` (DFD_2).
pub(crate) fn data_type(ty: &FieldType) -> i64 {
    match ty {
        FieldType::Integer => 0,
        FieldType::Decimal => 5,
        FieldType::Option | FieldType::Enum(_) => 6,
        FieldType::Boolean => 7,
        FieldType::Date => 9,
        FieldType::Time => 10,
        FieldType::Blob => 12,
        FieldType::DateFormula => 13,
        FieldType::TableFilter => 14,
        FieldType::BigInteger => 15,
        FieldType::Duration => 16,
        FieldType::Guid => 17,
        FieldType::RecordId => 18,
        FieldType::DateTime => 19,
        FieldType::Text(_) | FieldType::Other(_) => 21,
        FieldType::Code(_) => 23,
        FieldType::Media => 25,
        FieldType::MediaSet => 26,
    }
}

pub(crate) fn field_length(ty: &FieldType) -> u32 {
    match ty {
        FieldType::Code(n) | FieldType::Text(n) => *n,
        _ => 0,
    }
}

/// Option metadata of an Option or Enum field: names (`OptionString`), captions
/// (`OptionCaption`) and ordinals.
#[derive(Debug, Clone)]
pub(crate) struct OptionInfo<'a> {
    pub names: Vec<String>,
    pub captions: Vec<String>,
    pub values: Vec<i64>,
    pub enum_def: Option<&'a EnumDef>,
}

pub(crate) fn option_info<'a>(cat: &'a Catalog, f: &FieldDef) -> Option<OptionInfo<'a>> {
    match &f.ty {
        FieldType::Option => {
            Some(OptionInfo { names: f.options.clone(), captions: f.option_captions.clone(), values: (0..f.options.len() as i64).collect(), enum_def: None })
        }
        FieldType::Enum(name) => {
            let e = cat.enum_by_name(name)?;
            Some(enum_options(e))
        }
        _ => None,
    }
}

pub(crate) fn enum_options(e: &EnumDef) -> OptionInfo<'_> {
    OptionInfo {
        names: e.values.iter().map(|v| v.name.clone()).collect(),
        captions: e.values.iter().map(|v| v.caption.clone()).collect(),
        values: e.values.iter().map(|v| v.ordinal).collect(),
        enum_def: Some(e),
    }
}

pub(crate) fn fprop<'a>(f: &'a FieldDef, k: &str) -> Option<&'a str> {
    f.prop_src.get(k).map(|s| s.as_str())
}

pub(crate) fn fbool(f: &FieldDef, k: &str) -> Option<bool> {
    fprop(f, k).and_then(alprop::bool_lit)
}

pub(crate) fn table_of<'a>(cat: &'a Catalog, name: &str) -> Option<&'a TableDef> {
    cat.table_by_name(name)
}

pub(crate) fn field_of<'a>(t: &'a TableDef, name: &str) -> Option<&'a FieldDef> {
    t.field(name).map(|i| &t.fields[i])
}

/// Target table and field of a `TableRelation` branch. Without an explicit field the
/// relation points at the last primary-key field of the target table.
pub(crate) fn relation_target<'a>(cat: &'a Catalog, b: &RelationBranch) -> Option<(&'a TableDef, Option<&'a FieldDef>)> {
    let t = table_of(cat, &b.table)?;
    let f = match &b.field {
        Some(n) => field_of(t, n),
        None => t.keys.first().and_then(|k| k.fields.last()).map(|&i| &t.fields[i]),
    };
    Some((t, f))
}

/// `FilterType`: CONST 0, FILTER 1, FIELD 2.
pub(crate) fn filter_type(k: &CondKind) -> i64 {
    match k {
        CondKind::Const => 0,
        CondKind::Filter => 1,
        CondKind::Field => 2,
    }
}

/// Wire value of one condition. CONST values on Option/Enum fields become the ordinal
/// (the web client compares a CONST condition against the row's int value as a string),
/// `Database::X` becomes the table id. FIELD values are the id of the field in `source`
/// as a decimal string (the client looks the value up as a column of the providing row).
pub(crate) fn cond_value(cat: &Catalog, c: &Cond, target: Option<&FieldDef>, source: Option<&TableDef>) -> String {
    match c.kind {
        CondKind::Field => source.and_then(|t| field_of(t, &c.value)).map(|f| f.id.to_string()).unwrap_or_else(|| c.value.clone()),
        CondKind::Filter => c.value.clone(),
        CondKind::Const => {
            let v = alprop::resolve_scope_const(cat, &c.value);
            if let Some(o) = target.and_then(|f| option_info(cat, f)) {
                let name = v.trim().trim_matches('"').trim_matches('\'');
                if let Some(i) = o.names.iter().position(|n| n.trim().eq_ignore_ascii_case(name.trim())) {
                    return o.values[i].to_string();
                }
            }
            v
        }
    }
}

/// `MetaFilter` list (relations and CalcFormula): `FieldId` is a field of `target`.
fn meta_filters(cat: &Catalog, conds: &[Cond], target: &TableDef, source: &TableDef) -> Vec<Value> {
    conds
        .iter()
        .filter_map(|c| {
            let tf = field_of(target, &c.field)?;
            let mut m = Map::new();
            m.insert("FieldId".into(), json!(tf.id));
            let ft = filter_type(&c.kind);
            if ft != 0 {
                m.insert("FilterType".into(), json!(ft));
            }
            m.insert("FilterValue".into(), json!(cond_value(cat, c, Some(tf), Some(source))));
            if c.as_filter {
                m.insert("ValueIsFilter".into(), json!(true));
            }
            if c.upper_limit {
                m.insert("OnlyMaxLimit".into(), json!(true));
            }
            Some(Value::Object(m))
        })
        .collect()
}

/// `MetaCondition` list of an `if (...)` branch: `FieldId` is a field of the table itself.
fn meta_conditions(cat: &Catalog, conds: &[Cond], t: &TableDef) -> Vec<Value> {
    conds
        .iter()
        .filter_map(|c| {
            let f = field_of(t, &c.field)?;
            let mut m = Map::new();
            m.insert("FieldId".into(), json!(f.id));
            let ft = filter_type(&c.kind);
            if ft != 0 {
                m.insert("ConditionType".into(), json!(ft));
            }
            m.insert("ConditionValue".into(), json!(cond_value(cat, c, Some(f), Some(t))));
            Some(Value::Object(m))
        })
        .collect()
}

fn relations(cat: &Catalog, t: &TableDef, f: &FieldDef) -> Vec<Value> {
    f.relation
        .iter()
        .filter_map(|b| {
            let (tt, tf) = relation_target(cat, b)?;
            let mut m = Map::new();
            m.insert("TableId".into(), json!(tt.id));
            m.insert("TableName".into(), json!(tt.name));
            if let Some(tf) = tf {
                m.insert("FieldId".into(), json!(tf.id));
            }
            let filters = meta_filters(cat, &b.conds, tt, t);
            if !filters.is_empty() {
                m.insert("Filters".into(), Value::Array(filters));
            }
            let conds = meta_conditions(cat, &b.when, t);
            if !conds.is_empty() {
                m.insert("Conditions".into(), Value::Array(conds));
            }
            Some(Value::Object(m))
        })
        .collect()
}

pub(crate) fn flow_field_type(m: CalcMethod) -> &'static str {
    match m {
        CalcMethod::Sum => "Sum",
        CalcMethod::Count => "Count",
        CalcMethod::Exist => "Exist",
        CalcMethod::Lookup => "Lookup",
        CalcMethod::Min => "Min",
        CalcMethod::Max => "Max",
        CalcMethod::Average => "Average",
    }
}

/// Table id of a FlowField's CalcFormula.
pub(crate) fn calc_table<'a>(cat: &'a Catalog, f: &FieldDef) -> Option<&'a TableDef> {
    f.calc.as_ref().and_then(|c| table_of(cat, &c.table))
}

fn calc_formula(cat: &Catalog, t: &TableDef, f: &FieldDef) -> Option<Value> {
    let c = f.calc.as_ref()?;
    let ct = table_of(cat, &c.table)?;
    let mut m = Map::new();
    m.insert("TableId".into(), json!(ct.id));
    if let Some(cf) = c.field.as_deref().and_then(|n| field_of(ct, n)) {
        m.insert("FieldId".into(), json!(cf.id));
    }
    let filters = meta_filters(cat, &c.conds, ct, t);
    if !filters.is_empty() {
        m.insert("Filters".into(), Value::Array(filters));
    }
    m.insert("FlowFieldType".into(), json!(flow_field_type(c.method)));
    if c.reverse_sign {
        m.insert("ReverseSign".into(), json!(true));
    }
    Some(Value::Object(m))
}

/// True when changing the field must go to the server: an OnValidate trigger, extension
/// validate triggers or event subscribers, or a table relation to check.
pub(crate) fn has_validation(cat: &Catalog, t: &TableDef, f: &FieldDef) -> bool {
    if f.on_validate.is_some() || !f.before_validate.is_empty() || !f.after_validate.is_empty() || !f.relation.is_empty() {
        return true;
    }
    let el = f.name.to_ascii_lowercase();
    ["onbeforevalidateevent", "onaftervalidateevent"].iter().any(|ev| cat.subscribers.contains_key(&(ObjectKind::Table, t.id, ev.to_string(), el.clone())))
}

/// `DataClassification` of a field: its own property, else the table's, else ToBeClassified.
fn data_classification(t: &TableDef, f: &FieldDef) -> i64 {
    fprop(f, "dataclassification")
        .or_else(|| t.prop_src.get("dataclassification").map(|s| s.as_str()))
        .and_then(|s| enum_val(alprop::DATA_CLASSIFICATIONS, s))
        .unwrap_or(1)
}

fn put_str(m: &mut Map<String, Value>, k: &str, v: Option<&str>) {
    if let Some(v) = v.filter(|v| !v.is_empty()) {
        m.insert(k.into(), json!(v));
    }
}

fn meta_field(cat: &Catalog, t: &TableDef, f: &FieldDef) -> Value {
    let mut m = Map::new();
    m.insert("Id".into(), json!(f.id));
    m.insert("Name".into(), json!(f.name));
    m.insert("Caption".into(), json!(f.caption));
    m.insert("Type".into(), json!(meta_type_code(&f.ty)));
    m.insert("ClrType".into(), json!(clr_type(&f.ty)));
    let len = field_length(&f.ty);
    if len > 0 {
        m.insert("Length".into(), json!(len));
    }
    m.insert("DataColumnName".into(), json!(f.id.to_string()));
    match f.class {
        FieldClass::Normal => {}
        FieldClass::FlowField => {
            m.insert("FieldClass".into(), json!(1));
        }
        FieldClass::FlowFilter => {
            m.insert("FieldClass".into(), json!(2));
        }
    }
    if !f.editable {
        m.insert("Editable".into(), json!(false));
    }
    if fbool(f, "enabled") == Some(false) {
        m.insert("Enabled".into(), json!(false));
    }
    if let Some(o) = option_info(cat, f) {
        m.insert("OptionString".into(), json!(o.names.join(",")));
        m.insert("OptionCaption".into(), json!(o.captions.join(",")));
        m.insert("OptionValues".into(), json!(o.values));
        if let Some(e) = o.enum_def {
            m.insert("OptionOrdinalValues".into(), json!(o.values));
            m.insert("EnumTypeId".into(), json!(e.id));
            m.insert("EnumTypeName".into(), json!(e.name));
            if e.extensible {
                m.insert("Extensible".into(), json!(true));
            }
        }
    }
    let rels = relations(cat, t, f);
    if !rels.is_empty() {
        m.insert("Relations".into(), Value::Array(rels));
    }
    if let Some(c) = calc_formula(cat, t, f) {
        m.insert("CalcFormula".into(), c);
    }
    if fbool(f, "validatetablerelation") == Some(false) {
        m.insert("ValidateRelation".into(), json!(false));
    }
    if f.on_lookup.is_some() {
        m.insert("Lookup".into(), json!(true));
    }
    if has_validation(cat, t, f) {
        m.insert("Validation".into(), json!(true));
    }
    if f.not_blank {
        m.insert("NotBlank".into(), json!(true));
    }
    if f.blank_zero {
        m.insert("BlankZero".into(), json!(true));
    }
    if let Some(b) = fprop(f, "blanknumbers").and_then(|s| enum_val(alprop::BLANK_NUMBERS, s)).filter(|&b| b != 0) {
        m.insert("BlankNumbers".into(), json!(b));
    }
    put_str(&mut m, "DecimalPlaces", f.decimal_places.as_deref());
    if fbool(f, "closingdates") == Some(true) {
        m.insert("ClosingDates".into(), json!(true));
    }
    if fbool(f, "numeric") == Some(true) {
        m.insert("Numeric".into(), json!(true));
    }
    put_str(&mut m, "MinValue", fprop(f, "minvalue"));
    put_str(&mut m, "MaxValue", fprop(f, "maxvalue"));
    put_str(&mut m, "InitValue", f.init_value.as_deref());
    put_str(&mut m, "CharAllowed", fprop(f, "charallowed"));
    put_str(&mut m, "ValuesAllowed", fprop(f, "valuesallowed"));
    put_str(&mut m, "CaptionClass", fprop(f, "captionclass"));
    // ExtendedDatatype defaults to None (1) in MetaField, so the NST writes 1 for every field
    m.insert("ExtendedDatatype".into(), json!(fprop(f, "extendeddatatype").and_then(|s| enum_val(alprop::EXTENDED_DATATYPES, s)).unwrap_or(1)));
    if let Some(mt) = fprop(f, "masktype").and_then(|s| enum_val(alprop::MASK_TYPES, s)).filter(|&v| v != 0) {
        m.insert("MaskType".into(), json!(mt));
    }
    if let Some(w) = fprop(f, "width").and_then(|s| s.parse::<i64>().ok()) {
        m.insert("Width".into(), json!(w));
    }
    if let Some(b) = fprop(f, "subtype").and_then(|s| enum_val(alprop::BLOB_SUBTYPES, s)).filter(|&v| v != 0) {
        m.insert("BlobSubType".into(), json!(b));
    }
    if f.auto_increment {
        m.insert("AutoIncrement".into(), json!(true));
    }
    put_str(&mut m, "ToolTip", f.tooltip.as_deref());
    if let Some((ty, id, mask)) = fprop(f, "accessbypermission").and_then(|s| alprop::access_by_permission(cat, s)) {
        m.insert("AccessByObjectType".into(), json!(ty));
        m.insert("AccessByObjectID".into(), json!(id));
        m.insert("AccessByPermissionMask".into(), json!(mask));
    }
    if let Some(s) = fprop(f, "obsoletestate").and_then(|s| enum_val(alprop::OBSOLETE_STATES, s)).filter(|&v| v != 0) {
        m.insert("ObsoleteState".into(), json!(s));
        put_str(&mut m, "ObsoleteReason", fprop(f, "obsoletereason"));
    }
    if let Some(a) = fprop(f, "access").and_then(|s| enum_val(alprop::ACCESS, s)).filter(|&v| v != 0) {
        m.insert("Access".into(), json!(a));
    }
    let dc = data_classification(t, f);
    if dc != 0 {
        m.insert("DataClassification".into(), json!(dc));
    }
    if fbool(f, "optimizefortextsearch") == Some(true) {
        m.insert("OptimizeForTextSearch".into(), json!(true));
    }
    Value::Object(m)
}

fn field_refs(t: &TableDef, idx: &[usize]) -> Vec<Value> {
    idx.iter().map(|&i| json!({ "Id": t.fields[i].id, "Name": t.fields[i].name })).collect()
}

fn meta_key(t: &TableDef, i: usize) -> Value {
    let k = &t.keys[i];
    // The primary key is clustered unless another key claims it, and always unique.
    let clustered = k.clustered || (i == 0 && !t.keys.iter().any(|k| k.clustered));
    let mut m = Map::new();
    m.insert("Name".into(), json!(k.name));
    m.insert("FieldRelations".into(), Value::Array(field_refs(t, &k.fields)));
    if !k.sum_fields.is_empty() {
        m.insert("SumIndexFields".into(), Value::Array(field_refs(t, &k.sum_fields)));
    }
    // these five are serialized without EmitDefaultValue=false
    m.insert("Enabled".into(), json!(k.enabled));
    m.insert("MaintainSqlIndex".into(), json!(k.maintain_sql_index));
    m.insert("MaintainSiftIndex".into(), json!(k.maintain_sift_index));
    m.insert("Clustered".into(), json!(clustered));
    m.insert("Unique".into(), json!(k.unique || i == 0));
    Value::Object(m)
}

fn field_groups(t: &TableDef) -> Vec<Value> {
    t.field_groups
        .iter()
        .map(|g| {
            let ids: Vec<i64> = g.fields.iter().filter_map(|n| field_of(t, n)).map(|f| f.id).collect();
            let mut m = Map::new();
            m.insert("Name".into(), json!(g.name));
            m.insert("FieldIds".into(), json!(ids));
            if let Some(c) = &g.caption {
                m.insert("Caption".into(), json!(c));
            }
            Value::Object(m)
        })
        .collect()
}

pub(crate) fn page_id_of(cat: &Catalog, name: Option<&str>) -> Option<i64> {
    name.and_then(|n| cat.id_of(ObjectKind::Page, n.trim_matches('"')))
}

/// The `MetaTable` JSON for a table.
pub(crate) fn meta_table(cat: &Catalog, t: &TableDef, token: i64) -> Value {
    let mut m = Map::new();
    m.insert("Id".into(), json!(t.id));
    m.insert("Name".into(), json!(t.name));
    if let Some(ns) = &t.namespace {
        m.insert("ALNamespace".into(), json!(ns));
    }
    m.insert("Caption".into(), json!(t.caption));
    m.insert("VersionNumber".into(), json!(token));
    if let Some(id) = page_id_of(cat, t.lookup_page.as_deref()) {
        m.insert("LookupFormId".into(), json!(id));
    }
    if let Some(id) = page_id_of(cat, t.drilldown_page.as_deref()) {
        m.insert("DrillDownFormId".into(), json!(id));
    }
    let dcf: Vec<i64> = t.data_caption_fields.iter().filter_map(|n| field_of(t, n)).map(|f| f.id).collect();
    if !dcf.is_empty() {
        m.insert("DataCaptionFields".into(), json!(dcf));
    }
    if t.data_per_company {
        m.insert("IsDataPerCompany".into(), json!(true));
    }
    if t.prop_src.get("pasteisvalid").and_then(|s| alprop::bool_lit(s)) == Some(false) {
        m.insert("PasteIsValid".into(), json!(false));
    }
    if let Some(tt) = enum_val(alprop::TABLE_TYPES, &t.table_type).filter(|&v| v != 0) {
        m.insert("TableType".into(), json!(tt));
    }
    if let Some(s) = t.prop_src.get("obsoletestate").and_then(|s| enum_val(alprop::OBSOLETE_STATES, s)).filter(|&v| v != 0) {
        m.insert("ObsoleteState".into(), json!(s));
    }
    if let Some(dc) = t.prop_src.get("dataclassification").and_then(|s| enum_val(alprop::DATA_CLASSIFICATIONS, s)).filter(|&v| v != 0) {
        m.insert("DataClassification".into(), json!(dc));
    }
    if let Some(a) = t.prop_src.get("access").and_then(|s| enum_val(alprop::ACCESS, s)).filter(|&v| v != 0) {
        m.insert("Access".into(), json!(a));
    }
    m.insert("Fields".into(), Value::Array(t.fields.iter().map(|f| meta_field(cat, t, f)).collect()));
    m.insert("Keys".into(), Value::Array((0..t.keys.len()).map(|i| meta_key(t, i)).collect()));
    let groups = field_groups(t);
    if !groups.is_empty() {
        m.insert("FieldGroups".into(), Value::Array(groups));
    }
    Value::Object(m)
}
