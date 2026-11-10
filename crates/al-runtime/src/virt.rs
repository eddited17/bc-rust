//! Virtual system tables computed on the fly: Integer (2000000026) and Date (2000000007).
//! Rows are generated lazily in primary-key order within the bounds of the record's filters.

use crate::filter::*;
use crate::value::*;
use bc_store::{Row, TableSchema, Value};
use chrono::Datelike;

pub const INTEGER: i64 = 2000000026;
pub const DATE: i64 = 2000000007;

/// Upper bound on generated candidate rows per request (guards filters that match nothing).
const MAX_SCAN: usize = 5_000_000;

pub fn is_virtual(table: i64) -> bool {
    table == INTEGER || table == DATE
}

/// Inclusive bounds implied by the filters on field `fi`, as i64 via `num`.
fn bounds(filters: &[FieldFilter], fi: usize, num: &dyn Fn(&Value) -> Option<i64>, lo0: i64, hi0: i64) -> (i64, i64) {
    let (mut lo, mut hi) = (lo0, hi0);
    for f in filters.iter().filter(|f| f.field == fi) {
        let (a, b) = match &f.node {
            FNode::Gt(v) => (num(v).map(|x| x + 1), None),
            FNode::Lt(v) => (None, num(v).map(|x| x - 1)),
            n => match n.range() {
                Some((a, b)) => (a.and_then(num), b.and_then(num)),
                None => (None, None),
            },
        };
        if let Some(a) = a {
            lo = lo.max(a);
        }
        if let Some(b) = b {
            hi = hi.min(b);
        }
    }
    (lo, hi)
}

fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i as i64),
        Value::BigInt(i) => Some(*i),
        Value::Option(i) => Some(*i as i64),
        Value::Date(d) => Some(*d as i64),
        _ => None,
    }
}

/// Day number (num_days_from_ce) of a stored date; closing dates count as their day; the undefined
/// date (0D) is day 0, before every period.
fn day_of(v: &Value) -> Option<i64> {
    match v {
        Value::Date(d) if *d > 0 => Some((*d / 2) as i64),
        Value::Date(_) => Some(0),
        _ => None,
    }
}

fn naive(day: i64) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::from_num_days_from_ce_opt(day as i32)
}

fn day_num(n: chrono::NaiveDate) -> i64 {
    n.num_days_from_ce() as i64
}

const MIN_DAY: i64 = 1; // 0001-01-01
const MAX_DAY: i64 = 3_652_059; // 9999-12-31

/// First and last Period Start of period type `t` (BC DateTimeHelper.datePeriodStartMinimumDate /
/// datePeriodStartMaximumDate): Date 0001-01-03, Week 0001-01-08, Month 0001-02-01, Quarter
/// 0001-04-01, Year 0002-01-01, up to 9999-12-31 / 9999-12-20 / 9999-12-01 / 9999-10-01 / 9999-01-01.
fn type_bounds(t: i32) -> (i64, i64) {
    let d = |y, m, dd| chrono::NaiveDate::from_ymd_opt(y, m, dd).map(day_num).unwrap_or(MIN_DAY);
    match t {
        0 => (d(1, 1, 3), d(9999, 12, 31)),
        1 => (d(1, 1, 8), d(9999, 12, 20)),
        2 => (d(1, 2, 1), d(9999, 12, 1)),
        3 => (d(1, 4, 1), d(9999, 10, 1)),
        _ => (d(2, 1, 1), d(9999, 1, 1)),
    }
}

/// First day of the period of type `t` (0 Date, 1 Week, 2 Month, 3 Quarter, 4 Year) starting at or after `day`.
fn first_start_at_or_after(t: i32, day: i64) -> Option<i64> {
    let n = naive(day)?;
    let s = start_of(t, n);
    if day_num(s) >= day {
        Some(day_num(s))
    } else {
        next_start(t, day_num(s))
    }
}

fn start_of(t: i32, n: chrono::NaiveDate) -> chrono::NaiveDate {
    match t {
        1 => n - chrono::Duration::days(n.weekday().num_days_from_monday() as i64),
        2 => n.with_day(1).unwrap(),
        3 => chrono::NaiveDate::from_ymd_opt(n.year(), n.month0() / 3 * 3 + 1, 1).unwrap(),
        4 => chrono::NaiveDate::from_ymd_opt(n.year(), 1, 1).unwrap(),
        _ => n,
    }
}

fn next_start(t: i32, start: i64) -> Option<i64> {
    let n = naive(start)?;
    let next = match t {
        1 => n + chrono::Duration::days(7),
        2 => add_months(n, 1)?,
        3 => add_months(n, 3)?,
        4 => chrono::NaiveDate::from_ymd_opt(n.year() + 1, 1, 1)?,
        _ => n + chrono::Duration::days(1),
    };
    Some(day_num(next))
}

fn prev_start(t: i32, start: i64) -> Option<i64> {
    let n = naive(start)?;
    let prev = match t {
        1 => n - chrono::Duration::days(7),
        2 => add_months(n, -1)?,
        3 => add_months(n, -3)?,
        4 => chrono::NaiveDate::from_ymd_opt(n.year() - 1, 1, 1)?,
        _ => n - chrono::Duration::days(1),
    };
    Some(day_num(prev))
}

fn add_months(n: chrono::NaiveDate, m: i32) -> Option<chrono::NaiveDate> {
    let total = n.year() * 12 + n.month0() as i32 + m;
    chrono::NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1)
}

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// The Date row for the period of type `t` starting on `start`.
fn date_row(schema: &TableSchema, t: i32, start: i64) -> Option<Row> {
    let s = naive(start)?;
    let end = match next_start(t, start) {
        Some(ns) => ns - 1,
        None => MAX_DAY,
    };
    let (no, name) = match t {
        0 => (s.weekday().number_from_monday() as i32, DAYS[s.weekday().num_days_from_monday() as usize].to_string()),
        1 => {
            let w = s.iso_week().week() as i32;
            (w, w.to_string())
        }
        2 => (s.month() as i32, MONTHS[s.month0() as usize].to_string()),
        3 => {
            let q = (s.month0() / 3 + 1) as i32;
            (q, q.to_string())
        }
        _ => (s.year(), s.year().to_string()),
    };
    let mut row = schema.new_row();
    // by field number: 1 Period Type, 2 Period Start, 3 Period End, 4 Period No., 5 Period Name, 6 Period Invariant Name
    let vals = [Value::Option(t), Value::Date(start as i32 * 2), Value::Date(end as i32 * 2 + 1), Value::Int(no), Value::Text(name.clone()), Value::Text(name)];
    for (k, v) in vals.into_iter().enumerate() {
        if let Some(i) = schema.field_ids.iter().position(|id| *id == k as i64 + 1) {
            row[i] = v;
        }
    }
    Some(row)
}

/// Rows of a virtual table matching `filters`, in primary-key order (`reverse` for descending),
/// strictly after `after` in that order, at most `limit`.
pub fn rows(schema: &TableSchema, filters: &[FieldFilter], reverse: bool, after: Option<&Row>, limit: usize) -> Vec<Row> {
    let mut out = vec![];
    let matches = |row: &Row| filters.iter().all(|f| f.node.matches(&row[f.field]));
    let mut scanned = 0usize;
    match schema.id {
        INTEGER => {
            let (lo, hi) = bounds(filters, 0, &int_of, i32::MIN as i64, i32::MAX as i64);
            let (mut lo, mut hi) = (lo, hi);
            if let Some(a) = after.and_then(|r| int_of(&r[0])) {
                if reverse {
                    hi = hi.min(a - 1);
                } else {
                    lo = lo.max(a + 1);
                }
            }
            let mut i = if reverse { hi } else { lo };
            while lo <= hi && i >= lo && i <= hi && out.len() < limit && scanned < MAX_SCAN {
                scanned += 1;
                let mut row = schema.new_row();
                row[0] = Value::Int(i as i32);
                if matches(&row) {
                    out.push(row);
                }
                i += if reverse { -1 } else { 1 };
            }
        }
        DATE => {
            let (tlo, thi) = bounds(filters, 0, &int_of, 0, 4);
            let (dlo, dhi) = bounds(filters, 1, &day_of, MIN_DAY, MAX_DAY);
            let after_key = after.and_then(|r| Some((int_of(&r[0])? as i32, day_of(&r[1])?)));
            let types: Vec<i32> = if reverse { (tlo as i32..=thi as i32).rev().collect() } else { (tlo as i32..=thi as i32).collect() };
            for t in types {
                if out.len() >= limit || scanned >= MAX_SCAN {
                    break;
                }
                if let Some((at, _)) = after_key {
                    if (!reverse && t < at) || (reverse && t > at) {
                        continue;
                    }
                }
                let (tmin, tmax) = type_bounds(t);
                let (dlo, dhi) = (dlo.max(tmin), dhi.min(tmax));
                if dlo > dhi {
                    continue;
                }
                let mut cur = if reverse {
                    naive(dhi).map(|n| day_num(start_of(t, n))).filter(|s| *s >= dlo)
                } else {
                    first_start_at_or_after(t, dlo)
                };
                while let Some(start) = cur {
                    if start < dlo || start > dhi || out.len() >= limit || scanned >= MAX_SCAN {
                        break;
                    }
                    scanned += 1;
                    let skip = matches!(after_key, Some((at, ad)) if at == t && (if reverse { start >= ad } else { start <= ad }));
                    if !skip {
                        if let Some(row) = date_row(schema, t, start) {
                            if matches(&row) {
                                out.push(row);
                            }
                        }
                    }
                    cur = if reverse { prev_start(t, start) } else { next_start(t, start) };
                }
            }
        }
        _ => {}
    }
    out
}

/// The row with the primary key of `key` if it exists in the virtual table.
pub fn get(schema: &TableSchema, key: &Row) -> Option<Row> {
    match schema.id {
        INTEGER => {
            let mut row = schema.new_row();
            row[0] = key[0].clone();
            Some(row)
        }
        DATE => {
            let t = int_of(&key[0])? as i32;
            let start = day_of(&key[1]).filter(|d| *d >= MIN_DAY)?;
            let n = naive(start)?;
            if !(0..=4).contains(&t) || day_num(start_of(t, n)) != start {
                return None;
            }
            let (tmin, tmax) = type_bounds(t);
            if start < tmin || start > tmax {
                return None;
            }
            date_row(schema, t, start)
        }
        _ => None,
    }
}

pub fn _unused(_: Val) {}

// ---------------------------------------------------------------- tables computed from the catalog

pub const FIELD: i64 = 2000000041;
pub const ALLOBJ: i64 = 2000000038;
pub const ALLOBJ_CAPTION: i64 = 2000000058;
pub const TABLE_METADATA: i64 = 2000000136;
pub const CODEUNIT_METADATA: i64 = 2000000137;
pub const PAGE_METADATA: i64 = 2000000138;
pub const REPORT_METADATA: i64 = 2000000139;
pub const WINDOWS_LANGUAGE: i64 = 2000000045;
/// Table 2000000164 "Time Zone": the server's time zones (Ncl TimeZoneDataProvider:
/// TimeZoneInfo.GetSystemTimeZones() in order, numbered from 1, Id and DisplayName).
pub const TIME_ZONE: i64 = 2000000164;

/// The Windows time zones of a BC 29.1 service tier (Windows Server), in GetSystemTimeZones order:
/// (Id, DisplayName). The same list the Linux web server shim offers (tools/msweb-linux).
pub fn windows_time_zones() -> impl Iterator<Item = (&'static str, &'static str)> {
    include_str!("../../../tools/msweb-linux/LinuxHostingStartup/windows-time-zones.tsv")
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split_once('\t'))
}
pub const LICENSE_PERMISSION: i64 = 2000000043;
pub const PUBLISHED_APPLICATION: i64 = 2000000206;
pub const NAV_APP_EXTRA: i64 = 2000000157;
pub const NAV_APP_INSTALLED_APP: i64 = 2000000153;
pub const INSTALLED_APPLICATION: i64 = 2000000212;
pub const APPLICATION_OBJECT_METADATA: i64 = 2000000207;
pub const REPORT_LAYOUT_LIST: i64 = 2000000234;
pub const API_WEBHOOK_ENTITY: i64 = 2000000097;
pub const TABLE_RELATIONS_METADATA: i64 = 2000000141;
/// Table 2000000192 "Page Control Field": the data-bound controls of every page (Ncl
/// PageControlFieldDataProvider).
pub const PAGE_CONTROL_FIELD: i64 = 2000000192;

/// Table 2000000232 "Tenant Report Layout": the user-defined layouts Report Layout List adds.
pub const TENANT_REPORT_LAYOUT: i64 = 2000000232;

/// Table 2000000211 "Feature Key": the platform's feature keys (Ncl FeatureKeyDataProvider), with
/// the state a Modify stored (recops: the stored rows win, a first Modify inserts the row).
pub const FEATURE_KEY: i64 = 2000000211;
/// Permission tables computed from the permission model (crate::permissions::catalog_rows).
pub const METADATA_PERMISSION: i64 = crate::permissions::METADATA_PERMISSION;
pub const METADATA_PERMISSION_SET_REL: i64 = crate::permissions::METADATA_PERMISSION_SET_REL;
pub const EXPANDED_PERMISSION: i64 = crate::permissions::EXPANDED_PERMISSION;

/// The platform feature keys: ID, enabled for all users, Learn More Link, Mandatory By, Mandatory By
/// Version, Can Try, Is One Way, Data Update Required, Description. The descriptions are bc-rust's
/// own short wording [src:A14].
pub const PLATFORM_FEATURE_KEYS: &[(&str, bool, &str, &str, &str, bool, bool, bool, &str)] = &[
    ("SalesPrices", false, "https://go.microsoft.com/fwlink/?linkid=2136722", W31_2027, "31.0", false, true, true, "New sales pricing"),
    ("SaveValueToDatabasePromptly", false, "https://go.microsoft.com/fwlink/?linkid=2212032", W30_2027, "30.0", true, false, false, "Save each field change right away"),
    ("ConcurrentInventoryPosting", false, "https://go.microsoft.com/fwlink/?linkid=2299833", W31_2027, "31.0", false, false, false, "Concurrent posting of item and value entries"),
    ("ConcurrentJobPosting", false, "https://go.microsoft.com/fwlink/?linkid=2299833", W31_2027, "31.0", false, false, false, "Concurrent posting of project ledger entries"),
    ("ConcurrentResourcePosting", false, "https://go.microsoft.com/fwlink/?linkid=2299833", W31_2027, "31.0", false, false, false, "Concurrent posting of resource ledger entries"),
    ("FullTextSearch", false, "https://go.microsoft.com/fwlink/?linkid=2280603", W30_2027, "30.0", true, false, false, "Faster text search in lists"),
    ("SemanticMetadataSearch", true, "https://go.microsoft.com/fwlink/?linkid=2312404", W30_2027, "30.0", false, false, false, "Search application metadata by meaning (preview)"),
    ("AdvancedTellMe", true, "https://go.microsoft.com/fwlink/?linkid=2331650", W30_2027, "30.0", false, false, false, "Extended search for pages and reports"),
    ("DisableSoapWebServicesOnMicrosoftUIPages", false, "https://go.microsoft.com/fwlink/?linkid=2312202", W29_2026, "29.0", false, false, false, "No SOAP web services on Microsoft UI pages"),
    (
        "FinancialReportDefaults",
        false,
        "https://learn.microsoft.com/en-us/dynamics365/release-plan/2025wave2/smb/dynamics365-business-central/use-enhanced-financial-reporting",
        W29_2026,
        "29.0",
        true,
        false,
        false,
        "Default values for financial reports",
    ),
    ("AcceleratedDepreciation", false, "TODO", W31_2027, "31.0", false, true, true, "Accelerated depreciation"),
    ("EXRPerformantTrialBalance", false, "https://learn.microsoft.com/dynamics365/business-central/reports/report-4405", W31_2027, "31.0", true, false, false, "Faster trial balance Excel reports"),
    ("DocumentReportExperience", false, "https://learn.microsoft.com/en-us/dynamics365/business-central/ui-manage-report-layouts", W31_2027, "31.0", false, false, false, "Document reports, new layout handling"),
    ("NewNotificationBar", false, "https://go.microsoft.com/fwlink/?LinkId=2384311", W30_2027, "30.0", true, false, false, "Notification bar, new design"),
];
const W29_2026: &str = "2026 Wave 2 (from October 2026 to March 2027)";
const W30_2027: &str = "2027 Wave 1 (from April to September 2027)";
const W31_2027: &str = "2027 Wave 2 (from October 2027 to March 2028)";

/// A user-defined layout as a Report Layout List row (Ncl ReportLayoutDataProvider
/// .GetTenantReportLayouts): Caption is the layout's Name, Runtime Package ID its App ID,
/// Application ID and Layout Publisher blank, User Defined set, Report Name the report's caption
/// ("Not Installed" when the report isn't loaded), ReportIsInstalled whether it is, header/footer
/// and theme parts blank; the other columns and the system fields come from the layout row.
pub fn tenant_layout_row(cat: &'static al_model::Catalog, tdef: &al_model::TableDef, schema: &TableSchema, trl: &al_model::TableDef, src: &Row) -> Row {
    let get = |n: &str| trl.field(n).and_then(|i| src.get(i).cloned());
    let mut row = schema.new_row();
    let mut put = |n: &str, v: Option<Value>| {
        if let (Some(i), Some(v)) = (tdef.field(n), v) {
            row[i] = v;
        }
    };
    let report = match get("Report ID") {
        Some(Value::Int(id)) => cat.reports.get(&(id as i64)),
        _ => None,
    };
    let report_name = report.map(|r| if r.caption.is_empty() { r.name.clone() } else { r.caption.clone() }).unwrap_or_else(|| "Not Installed".into());
    put("Report ID", get("Report ID"));
    put("Name", get("Name"));
    put("Caption", get("Name"));
    put("Runtime Package ID", get("App ID"));
    for n in ["Layout", "Layout Format", "Custom Type", "MIME Type", "Description", "IsObsolete", "ExcelLayoutMultipleDataSheets", "Layout Status", "Layout Subtype"] {
        put(n, get(n));
    }
    put("Report Name", Some(Value::Text(report_name)));
    put("User Defined", Some(Value::Bool(true)));
    put("ReportIsInstalled", Some(Value::Bool(report.is_some())));
    for n in ["SystemId", "SystemCreatedAt", "SystemCreatedBy", "SystemModifiedAt", "SystemModifiedBy"] {
        put(n, get(n));
    }
    row
}

pub fn is_catalog_table(table: i64) -> bool {
    matches!(
        table,
        METADATA_PERMISSION | METADATA_PERMISSION_SET_REL | EXPANDED_PERMISSION
            | FIELD | ALLOBJ | ALLOBJ_CAPTION | TABLE_METADATA | CODEUNIT_METADATA | PAGE_METADATA | REPORT_METADATA | WINDOWS_LANGUAGE | TIME_ZONE | LICENSE_PERMISSION | PUBLISHED_APPLICATION | NAV_APP_INSTALLED_APP | INSTALLED_APPLICATION | APPLICATION_OBJECT_METADATA
            | NAV_APP_EXTRA | REPORT_LAYOUT_LIST | API_WEBHOOK_ENTITY | TABLE_RELATIONS_METADATA | FEATURE_KEY | PAGE_CONTROL_FIELD
    )
}

/// Field.DataClassification as BC reports it (NCLMetaField.DataClassification): FlowFields and
/// FlowFilters are SystemMetadata; system tables use the platform's list
/// (SystemTablesDataClassification, else SystemMetadata); other fields their DataClassification
/// property, else the table's, else ToBeClassified; the platform fields are classified by the
/// platform (SystemFieldsHelper).
fn data_classification(t: &al_model::TableDef, f: &al_model::FieldDef) -> &'static str {
    const NAMES: [&str; 7] = [
        "CustomerContent",
        "ToBeClassified",
        "EndUserIdentifiableInformation",
        "AccountData",
        "EndUserPseudonymousIdentifiers",
        "OrganizationIdentifiableInformation",
        "SystemMetadata",
    ];
    if f.class != al_model::FieldClass::Normal {
        return "SystemMetadata";
    }
    if t.id >= 2000000001 {
        if matches!(f.id, 2000000000 | 2000000002 | 2000000004) {
            return "EndUserPseudonymousIdentifiers";
        }
        return SYSTEM_TABLE_DATA_CLASSIFICATION.iter().find(|(tb, fl, _)| *tb == t.id && *fl == f.id).map(|x| x.2).unwrap_or("SystemMetadata");
    }
    match f.id {
        // SystemCreatedByUserName ... SystemModifiedByFullName are FlowFields in BC
        0 | 2000000001 | 2000000003 | 2000000005..=2000000008 => return "SystemMetadata",
        2000000000 | 2000000002 | 2000000004 => return "EndUserPseudonymousIdentifiers",
        _ => {}
    }
    let own = f.prop_src.get("dataclassification");
    let prop = if f.from_extension.is_some() { own } else { own.or_else(|| t.prop_src.get("dataclassification")) };
    prop.and_then(|p| {
        let p = p.rsplit("::").next().unwrap_or(p).trim().trim_matches('"');
        NAMES.iter().find(|n| n.eq_ignore_ascii_case(p)).copied()
    })
    .unwrap_or("ToBeClassified")
}

/// Field.ObsoleteState: the field's ObsoleteState, or the table's when that is further
/// (FieldDataProvider.GetObsoleteState), so the fields of a removed table read Removed.
fn obsolete_state(t: &al_model::TableDef, f: &al_model::FieldDef) -> &'static str {
    let level = |props: &std::collections::HashMap<String, String>| match props.get("obsoletestate").map(|p| p.rsplit("::").next().unwrap_or(p).trim().to_ascii_lowercase()) {
        Some(p) if p == "pending" => 1,
        Some(p) if p == "removed" => 2,
        _ => 0,
    };
    let table = if t.obsolete_removed { 2 } else { level(&t.prop_src) };
    match level(&f.prop_src).max(table) {
        2 => "Removed",
        1 => "Pending",
        _ => "No",
    }
}

/// Platform classifications of system-table fields (Types SystemTablesDataClassification).
const SYSTEM_TABLE_DATA_CLASSIFICATION: &[(i64, i64, &str)] = &[
    (2000000001, 50, "EndUserIdentifiableInformation"),
    (2000000009, 2, "EndUserIdentifiableInformation"),
    (2000000053, 1, "EndUserIdentifiableInformation"),
    (2000000055, 1, "EndUserPseudonymousIdentifiers"),
    (2000000055, 2, "EndUserIdentifiableInformation"),
    (2000000067, 1, "EndUserIdentifiableInformation"),
    (2000000068, 3, "CustomerContent"),
    (2000000068, 4, "CustomerContent"),
    (2000000068, 5, "CustomerContent"),
    (2000000068, 6, "CustomerContent"),
    (2000000068, 7, "CustomerContent"),
    (2000000068, 9, "CustomerContent"),
    (2000000068, 11, "EndUserIdentifiableInformation"),
    (2000000068, 14, "EndUserIdentifiableInformation"),
    (2000000073, 3, "EndUserPseudonymousIdentifiers"),
    (2000000073, 6, "EndUserIdentifiableInformation"),
    (2000000075, 3, "EndUserPseudonymousIdentifiers"),
    (2000000075, 6, "EndUserIdentifiableInformation"),
    (2000000080, 3, "EndUserPseudonymousIdentifiers"),
    (2000000080, 6, "EndUserIdentifiableInformation"),
    (2000000110, 6, "EndUserIdentifiableInformation"),
    (2000000110, 1, "EndUserPseudonymousIdentifiers"),
    (2000000111, 9, "EndUserIdentifiableInformation"),
    (2000000111, 1, "EndUserPseudonymousIdentifiers"),
    (2000000114, 7, "EndUserIdentifiableInformation"),
    (2000000114, 9, "CustomerContent"),
    (2000000120, 1, "EndUserPseudonymousIdentifiers"),
    (2000000120, 2, "EndUserIdentifiableInformation"),
    (2000000120, 3, "EndUserIdentifiableInformation"),
    (2000000120, 7, "EndUserPseudonymousIdentifiers"),
    (2000000120, 11, "EndUserIdentifiableInformation"),
    (2000000120, 14, "EndUserIdentifiableInformation"),
    (2000000120, 15, "EndUserPseudonymousIdentifiers"),
    (2000000121, 1, "EndUserPseudonymousIdentifiers"),
    (2000000121, 2, "CustomerContent"),
    (2000000121, 4, "CustomerContent"),
    (2000000121, 5, "CustomerContent"),
    (2000000121, 3, "EndUserIdentifiableInformation"),
    (2000000121, 10, "EndUserPseudonymousIdentifiers"),
    (2000000151, 1, "OrganizationIdentifiableInformation"),
    (2000000212, 3, "OrganizationIdentifiableInformation"),
    (2000000154, 6, "EndUserIdentifiableInformation"),
    (2000000160, 28, "OrganizationIdentifiableInformation"),
    (2000000206, 12, "OrganizationIdentifiableInformation"),
    (2000000206, 22, "CustomerContent"),
    (2000000206, 15, "CustomerContent"),
    (2000000206, 14, "CustomerContent"),
    (2000000206, 20, "CustomerContent"),
    (2000000206, 34, "CustomerContent"),
    (2000000206, 21, "CustomerContent"),
    (2000000206, 35, "CustomerContent"),
    (2000000206, 32, "OrganizationIdentifiableInformation"),
    (2000000206, 19, "CustomerContent"),
    (2000000206, 17, "CustomerContent"),
    (2000000206, 33, "OrganizationIdentifiableInformation"),
    (2000000175, 2, "EndUserIdentifiableInformation"),
    (2000000175, 3, "EndUserIdentifiableInformation"),
    (2000000181, 13, "EndUserIdentifiableInformation"),
    (2000000184, 13, "EndUserIdentifiableInformation"),
    (2000000188, 1, "EndUserPseudonymousIdentifiers"),
    (2000000189, 4, "EndUserPseudonymousIdentifiers"),
    (2000000196, 9, "EndUserIdentifiableInformation"),
    (2000000197, 1, "EndUserPseudonymousIdentifiers"),
    (2000000197, 2, "EndUserIdentifiableInformation"),
    (2000000197, 3, "OrganizationIdentifiableInformation"),
    (2000000197, 6, "EndUserIdentifiableInformation"),
    (2000000197, 8, "EndUserIdentifiableInformation"),
    (2000000197, 9, "EndUserIdentifiableInformation"),
    (2000000197, 7, "OrganizationIdentifiableInformation"),
    (2000000095, 8, "EndUserPseudonymousIdentifiers"),
    (2000000095, 10, "CustomerContent"),
    (2000000096, 3, "EndUserPseudonymousIdentifiers"),
    (2000000098, 3, "EndUserPseudonymousIdentifiers"),
    (2000000201, 2, "CustomerContent"),
    (2000000107, 4, "EndUserIdentifiableInformation"),
    (2000000107, 5, "CustomerContent"),
    (2000000107, 6, "CustomerContent"),
    (2000000213, 3, "CustomerContent"),
    (2000000216, 5, "CustomerContent"),
    (2000000217, 5, "CustomerContent"),
    (2000000218, 4, "CustomerContent"),
    (2000000219, 6, "CustomerContent"),
    (2000000239, 11, "CustomerContent"),
    (2000000240, 5, "EndUserPseudonymousIdentifiers"),
    (2000000240, 8, "CustomerContent"),
    (2000000241, 6, "EndUserPseudonymousIdentifiers"),
    (2000000258, 1, "EndUserPseudonymousIdentifiers"),
    (2000000258, 3, "CustomerContent"),
    (2000000258, 9, "CustomerContent"),
    (2000000259, 1, "EndUserPseudonymousIdentifiers"),
    (2000000259, 2, "CustomerContent"),
    (2000000158, 2, "EndUserPseudonymousIdentifiers"),
    (2000000158, 3, "EndUserPseudonymousIdentifiers"),
    (2000000158, 4, "OrganizationIdentifiableInformation"),
    (2000000297, 2, "EndUserPseudonymousIdentifiers"),
    (2000000297, 5, "OrganizationIdentifiableInformation"),
    (2000000297, 6, "CustomerContent"),
    (2000000267, 2, "EndUserIdentifiableInformation"),
    (2000000267, 3, "EndUserIdentifiableInformation"),
    (2000000267, 9, "CustomerContent"),
    (2000000268, 5, "CustomerContent"),
    (2000000268, 12, "CustomerContent"),
    (2000000268, 11, "CustomerContent"),
    (2000000269, 3, "EndUserIdentifiableInformation"),
    (2000000269, 6, "CustomerContent"),
    (2000000269, 5, "CustomerContent"),
    (2000000269, 8, "CustomerContent"),
    (2000000271, 3, "CustomerContent"),
    (2000000271, 4, "CustomerContent"),
    (2000000271, 7, "CustomerContent"),
    (2000000271, 8, "CustomerContent"),
    (2000000272, 6, "CustomerContent"),
    (2000000298, 4, "CustomerContent"),
    (2000000281, 3, "EndUserIdentifiableInformation"),
    (2000000281, 6, "CustomerContent"),
    (2000000281, 5, "CustomerContent"),
    (2000000281, 9, "CustomerContent"),
    (2000000295, 3, "EndUserPseudonymousIdentifiers"),
    (2000000295, 6, "OrganizationIdentifiableInformation"),
    (2000000295, 14, "OrganizationIdentifiableInformation"),
    (2000000295, 16, "OrganizationIdentifiableInformation"),
    (2000000123, 11, "EndUserPseudonymousIdentifiers"),
    (2000000123, 12, "OrganizationIdentifiableInformation"),
    (2000000123, 13, "OrganizationIdentifiableInformation"),
    (2000000247, 3, "EndUserPseudonymousIdentifiers"),
    (2000000247, 4, "OrganizationIdentifiableInformation"),
    (2000000010, 2, "OrganizationIdentifiableInformation"),
    (2000000010, 3, "OrganizationIdentifiableInformation"),
    (2000000010, 4, "OrganizationIdentifiableInformation"),
    (2000000010, 5, "OrganizationIdentifiableInformation"),
    (2000000010, 6, "OrganizationIdentifiableInformation"),
    (2000000013, 6, "CustomerContent"),
    (2000000014, 15, "CustomerContent"),
];

/// Field."Type" option member for a field type.
fn field_type_member(t: &al_model::FieldType) -> &'static str {
    use al_model::FieldType as T;
    match t {
        T::TableFilter => "TableFilter",
        T::RecordId => "RecordID",
        T::Date => "Date",
        T::Time => "Time",
        T::DateFormula => "DateFormula",
        T::Decimal => "Decimal",
        T::Media => "Media",
        T::MediaSet => "MediaSet",
        T::Text(_) | T::Other(_) => "Text",
        T::Code(_) => "Code",
        T::Blob => "BLOB",
        T::Boolean => "Boolean",
        T::Integer => "Integer",
        T::Option | T::Enum(_) => "Option",
        T::BigInteger => "BigInteger",
        T::Duration => "Duration",
        T::Guid => "GUID",
        T::DateTime => "DateTime",
    }
}

fn type_name(t: &al_model::FieldType) -> String {
    use al_model::FieldType as T;
    match t {
        T::Code(n) => format!("Code{n}"),
        T::Text(n) => format!("Text{n}"),
        T::Enum(_) => "Option".into(),
        T::Other(o) => o.clone(),
        o => format!("{o:?}"),
    }
}

/// AllObj "Object Type" option member.
fn object_type_member(k: al_syntax::ObjectKind) -> Option<&'static str> {
    use al_syntax::ObjectKind as K;
    Some(match k {
        K::Table => "Table",
        K::Report => "Report",
        K::Codeunit => "Codeunit",
        K::XmlPort => "XMLport",
        K::Page => "Page",
        K::Query => "Query",
        K::PageExtension => "PageExtension",
        K::TableExtension => "TableExtension",
        K::Enum => "Enum",
        K::EnumExtension => "EnumExtension",
        K::Profile => "Profile",
        K::ReportExtension => "ReportExtension",
        K::PermissionSet => "PermissionSet",
        K::PermissionSetExtension => "PermissionSetExtension",
        _ => return None,
    })
}

struct RowBuilder<'a> {
    tdef: &'a al_model::TableDef,
    row: Row,
}

impl<'a> RowBuilder<'a> {
    fn new(tdef: &'a al_model::TableDef, schema: &TableSchema) -> Self {
        RowBuilder { tdef, row: schema.new_row() }
    }
    fn set(mut self, field: &str, v: Val) -> Self {
        if let Some(i) = self.tdef.field(field) {
            let fd = &self.tdef.fields[i];
            let v = match (&fd.ty, v) {
                (al_model::FieldType::Text(n) | al_model::FieldType::Code(n), Val::Text(s)) => Val::Text(s.chars().take(*n as usize).collect()),
                (_, v) => v,
            };
            if let Ok(sv) = crate::record::to_store(fd, &v) {
                self.row[i] = sv;
            }
        }
        self
    }
    /// Set an Option field by member name (ordinals come from the table's OptionMembers).
    fn opt(mut self, field: &str, member: &str) -> Self {
        if let Some(i) = self.tdef.field(field) {
            let fd = &self.tdef.fields[i];
            if let Some(o) = fd.options.iter().position(|m| m.trim().trim_matches('"').eq_ignore_ascii_case(member)) {
                self.row[i] = Value::Option(o as i32);
            }
        }
        self
    }
}

/// "NAV App Installed App" and "Installed Application": every loaded app is installed in the
/// tenant (fields a table does not have are skipped).
fn installed_app_rows(cat: &'static al_model::Catalog, tdef: &al_model::TableDef, schema: &TableSchema, put: &mut dyn FnMut(Row)) {
    for a in &crate::modules::modules(cat).apps {
        put(RowBuilder::new(tdef, schema)
            .set("App ID", Val::Guid(a.id))
            .set("Runtime Package ID", Val::Guid(a.runtime_package_id))
            .set("Package ID", Val::Guid(a.package_id))
            .set("Name", Val::text(&a.name))
            .set("Publisher", Val::text(&a.publisher))
            .set("Version Major", Val::Int(a.version[0]))
            .set("Version Minor", Val::Int(a.version[1]))
            .set("Version Build", Val::Int(a.version[2]))
            .set("Version Revision", Val::Int(a.version[3]))
            .opt("Published As", "Global")
            .set("Tenant ID", Val::text("default"))
            .row);
    }
}

/// All rows of a catalog-computed system table (Field, AllObj, AllObjWithCaption, Table, CodeUnit,
/// Page and Report Metadata, Windows Language).
pub fn catalog_table(cat: &'static al_model::Catalog, tdef: &al_model::TableDef, schema: std::sync::Arc<TableSchema>) -> bc_store::TableData {
    let mut td = bc_store::TableData::new(schema.clone());
    let mut put = |row: Row| {
        let _ = td.insert(row);
    };
    match tdef.id {
        FIELD => {
            for t in cat.tables.values() {
                let pk = &t.pk().fields;
                for (i, f) in t.fields.iter().enumerate() {
                    let rel = f.relation.first().and_then(|r| cat.table_by_name(&r.table));
                    // a relation without a field is to the first primary-key field (NCLMetaFieldRelation)
                    let rel_field = rel.and_then(|rt| f.relation[0].field.as_ref().and_then(|x| rt.field(x)).or(rt.pk().fields.first().copied()).map(|fi| rt.fields[fi].id));
                    let len = match f.ty {
                        al_model::FieldType::Code(n) | al_model::FieldType::Text(n) => n as i32,
                        al_model::FieldType::Integer | al_model::FieldType::Option | al_model::FieldType::Enum(_) | al_model::FieldType::Boolean => 4,
                        al_model::FieldType::Decimal => 12,
                        al_model::FieldType::BigInteger | al_model::FieldType::Duration => 8,
                        al_model::FieldType::Guid => 16,
                        _ => 8,
                    };
                    let class = match f.class {
                        al_model::FieldClass::Normal => "Normal",
                        al_model::FieldClass::FlowField => "FlowField",
                        al_model::FieldClass::FlowFilter => "FlowFilter",
                    };
                    put(RowBuilder::new(tdef, &schema)
                        .set("TableNo", Val::Int(t.id as i32))
                        .set("No.", Val::Int(f.id as i32))
                        .set("TableName", Val::text(&t.name))
                        .set("FieldName", Val::text(&f.name))
                        .opt("Type", field_type_member(&f.ty))
                        .set("Len", Val::Int(len))
                        .opt("Class", class)
                        .set("Enabled", Val::Bool(true))
                        .set("Type Name", Val::text(type_name(&f.ty)))
                        .set("Field Caption", Val::text(&f.caption))
                        .set("RelationTableNo", Val::Int(rel.map(|r| r.id as i32).unwrap_or(0)))
                        .set("RelationFieldNo", Val::Int(rel_field.unwrap_or(0) as i32))
                        .set("OptionString", Val::text(f.options.join(",")))
                        .opt("ObsoleteState", obsolete_state(t, f))
                        .opt("DataClassification", data_classification(t, f))
                        .set("IsPartOfPrimaryKey", Val::Bool(pk.contains(&i)))
                        .row);
                }
            }
        }
        TABLE_RELATIONS_METADATA => {
            for t in cat.tables.values() {
                for f in &t.fields {
                    for (bi, b) in f.relation.iter().enumerate() {
                        let Some(rt) = cat.table_by_name(&b.table) else { continue };
                        // a relation without a field is to the first primary-key field
                        let rf = b.field.as_ref().and_then(|x| rt.field(x)).or(rt.pk().fields.first().copied());
                        let (rf_id, rf_name) = rf.map(|i| (rt.fields[i].id as i32, rt.fields[i].name.clone())).unwrap_or((0, String::new()));
                        let conds: Vec<Option<&al_model::Cond>> = if b.when.is_empty() { vec![None] } else { b.when.iter().map(Some).collect() };
                        for (ci, c) in conds.iter().enumerate() {
                            let mut rb = RowBuilder::new(tdef, &schema)
                                .set("Table ID", Val::Int(t.id as i32))
                                .set("Field No.", Val::Int(f.id as i32))
                                .set("Relation No.", Val::Int(bi as i32 + 1))
                                .set("Condition No.", Val::Int(if c.is_some() { ci as i32 + 1 } else { 0 }))
                                .set("Related Table ID", Val::Int(rt.id as i32))
                                .set("Related Field No.", Val::Int(rf_id))
                                .set("Test Table Relation", Val::Bool(true))
                                .set("Validate Table Relation", Val::Bool(f.validate_relation))
                                .set("Table Name", Val::text(&t.name))
                                .set("Field Name", Val::text(&f.name))
                                .set("Related Table Name", Val::text(&rt.name))
                                .set("Related Field Name", Val::text(&rf_name));
                            if let Some(c) = c {
                                let cf = t.field(&c.field).map(|i| t.fields[i].id as i32).unwrap_or(0);
                                rb = rb
                                    .opt("Condition Type", if matches!(c.kind, al_model::CondKind::Filter) { "FILTER" } else { "CONST" })
                                    .set("Condition Field No.", Val::Int(cf))
                                    .set("Condition Value", Val::text(&c.value));
                            }
                            put(rb.row);
                        }
                    }
                }
            }
        }
        ALLOBJ | ALLOBJ_CAPTION => {
            // App Package ID / App Runtime Package ID: the app the object belongs to (modules.rs)
            let mods = crate::modules::modules(cat);
            let mut add = |ty: &str, id: i64, name: &str, caption: &str, subtype: &str, app: Option<&crate::modules::AppInfo>| {
                put(RowBuilder::new(tdef, &schema)
                    .opt("Object Type", ty)
                    .set("Name", Val::text(name))
                    .set("Object ID", Val::Int(id as i32))
                    .set("Object Name", Val::text(name))
                    .set("Object Caption", Val::text(caption))
                    .set("Object Subtype", Val::text(subtype))
                    .set("App Package ID", Val::Guid(app.map(|a| a.package_id).unwrap_or([0; 16])))
                    .set("App Runtime Package ID", Val::Guid(app.map(|a| a.runtime_package_id).unwrap_or([0; 16])))
                    .row);
            };
            for t in cat.tables.values() {
                add("TableData", t.id, &t.name, &t.caption, "", mods.app_of_table(cat, t.id));
            }
            for (ci, co) in cat.code.iter().enumerate() {
                let Some(ty) = object_type_member(co.kind) else { continue };
                let caption = match co.kind {
                    al_syntax::ObjectKind::Table => cat.tables.get(&co.id).map(|t| t.caption.clone()),
                    al_syntax::ObjectKind::Page => cat.pages.get(&co.id).map(|p| p.caption.clone()),
                    _ => None,
                }
                .unwrap_or_else(|| co.name.clone());
                // a page's subtype is its PageType (Card when not set)
                let page_type = match co.kind {
                    al_syntax::ObjectKind::Page => cat.pages.get(&co.id).map(|p| if p.page_type.is_empty() { "Card".to_string() } else { p.page_type.clone() }),
                    _ => None,
                };
                add(ty, co.id, &co.name, &caption, page_type.as_deref().or(co.subtype.as_deref()).unwrap_or("Normal"), mods.app_of_code(ci as u32));
            }
            for e in cat.enums.values() {
                add("Enum", e.id, &e.name, &e.caption, "", None);
            }
        }
        APPLICATION_OBJECT_METADATA => {
            // one row per application object, as in BC; the Metadata and User Code blobs (BC's
            // compiled object metadata) stay empty, so AL that reads them sees HasValue = false
            let mods = crate::modules::modules(cat);
            for (ci, co) in cat.code.iter().enumerate() {
                let Some(ty) = object_type_member(co.kind) else { continue };
                let app = mods.app_of_code(ci as u32);
                put(RowBuilder::new(tdef, &schema)
                    .set("Runtime Package ID", Val::Guid(app.map(|a| a.runtime_package_id).unwrap_or([0; 16])))
                    .set("Package ID", Val::Guid(app.map(|a| a.package_id).unwrap_or([0; 16])))
                    .opt("Object Type", ty)
                    .set("Object ID", Val::Int(co.id as i32))
                    .opt("Metadata Format", "Full")
                    .set("Object Subtype", Val::text(co.subtype.as_deref().unwrap_or("Normal")))
                    .set("Object Name", Val::text(&co.name))
                    .row);
            }
        }
        TABLE_METADATA => {
            for t in cat.tables.values() {
                let page = |p: &Option<String>| p.as_deref().and_then(|n| cat.id_of(al_syntax::ObjectKind::Page, n)).unwrap_or(0) as i32;
                put(RowBuilder::new(tdef, &schema)
                    .set("ID", Val::Int(t.id as i32))
                    .set("Name", Val::text(&t.name))
                    .set("Caption", Val::text(&t.caption))
                    .set("DataPerCompany", Val::Bool(t.data_per_company))
                    .set("LookupPageID", Val::Int(page(&t.lookup_page)))
                    .set("DrillDownPageId", Val::Int(page(&t.drilldown_page)))
                    .opt("TableType", match t.table_type.to_ascii_lowercase().as_str() { "" => "Normal", "cds" => "CRM", _ => &t.table_type })
                    .set("DataIsExternal", Val::Bool(matches!(t.table_type.to_ascii_lowercase().as_str(), "crm" | "cds" | "externalsql" | "exchange" | "microsoftgraph")))
                    .row);
            }
        }
        CODEUNIT_METADATA => {
            for co in cat.code.iter().filter(|c| c.kind == al_syntax::ObjectKind::Codeunit) {
                put(RowBuilder::new(tdef, &schema)
                    .set("ID", Val::Int(co.id as i32))
                    .set("Name", Val::text(&co.name))
                    .set("TableNo", Val::Int(co.table.unwrap_or(0) as i32))
                    .set("SingleInstance", Val::Bool(co.single_instance))
                    .opt("SubType", co.subtype.as_deref().unwrap_or("Normal"))
                    .row);
            }
        }
        PAGE_METADATA => {
            for p in cat.pages.values() {
                let card = p.card_page.as_deref().and_then(|n| cat.id_of(al_syntax::ObjectKind::Page, n)).unwrap_or(0);
                let flag = |name: &str, default: bool| p.props.get(name).map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(default);
                put(RowBuilder::new(tdef, &schema)
                    .set("ID", Val::Int(p.id as i32))
                    .set("Name", Val::text(&p.name))
                    .set("Caption", Val::text(&p.caption))
                    .set("Editable", Val::Bool(flag("editable", true)))
                    .opt("PageType", if p.page_type.is_empty() { "Card" } else { &p.page_type })
                    .set("CardPageID", Val::Int(card as i32))
                    .set("SourceTable", Val::Int(p.source_table.unwrap_or(0) as i32))
                    .set("InsertAllowed", Val::Bool(flag("insertallowed", true)))
                    .set("ModifyAllowed", Val::Bool(flag("modifyallowed", true)))
                    .set("DeleteAllowed", Val::Bool(flag("deleteallowed", true)))
                    .set("DelayedInsert", Val::Bool(flag("delayedinsert", false)))
                    .set("AutoSplitKey", Val::Bool(flag("autosplitkey", false)))
                    .set("SourceTableTemporary", Val::Bool(flag("sourcetabletemporary", false)))
                    .row);
            }
        }
        PAGE_CONTROL_FIELD => {
            // a page's ControlDefinitions (field, label, usercontrol) in definition order; the
            // control ID numbers every layout element in that same order
            fn walk<'c>(cs: &'c [al_model::Control], id: &mut i32, out: &mut Vec<(i32, &'c al_model::Control)>) {
                for c in cs {
                    *id += 1;
                    if matches!(c.kind.as_str(), "field" | "label" | "usercontrol") {
                        out.push((*id, c));
                    }
                    walk(&c.children, id, out);
                }
            }
            for p in cat.pages.values() {
                let table = p.source_table.and_then(|t| cat.tables.get(&t));
                let mut ctls = Vec::new();
                walk(&p.layout, &mut 0, &mut ctls);
                for (seq, (id, c)) in ctls.into_iter().enumerate() {
                    let field = c.field.and_then(|fi| table.and_then(|t| t.fields.get(fi)));
                    let (expr, options) = match field {
                        Some(f) => {
                            let options = match &f.ty {
                                al_model::FieldType::Option => f.options.iter().map(|o| o.trim_matches('"')).collect::<Vec<_>>().join(","),
                                _ => String::new(),
                            };
                            (f.name.clone(), options)
                        }
                        None => (c.source.clone().unwrap_or_default(), String::new()),
                    };
                    let prop = |name: &str, default: &str| c.prop_src.get(name).cloned().unwrap_or_else(|| default.to_string());
                    put(RowBuilder::new(tdef, &schema)
                        .set("PageNo", Val::Int(p.id as i32))
                        .set("ControlId", Val::Int(id))
                        .set("ControlName", Val::text(&c.name))
                        .set("TableNo", Val::Int(p.source_table.unwrap_or(0) as i32))
                        .set("FieldNo", Val::Int(field.map(|f| f.id as i32).unwrap_or(0)))
                        .set("Enabled", Val::text(&prop("enabled", "true")))
                        .set("Editable", Val::text(&prop("editable", "")))
                        .set("Visible", Val::text(&prop("visible", "true")))
                        .set("SourceExpression", Val::text(&expr))
                        .set("OptionString", Val::text(&options))
                        .set("Sequence", Val::Int(seq as i32))
                        .row);
                }
            }
        }
        PUBLISHED_APPLICATION => {
            // every loaded app is published (global) and installed
            for a in &crate::modules::modules(cat).apps {
                put(RowBuilder::new(tdef, &schema)
                    .set("Runtime Package ID", Val::Guid(a.runtime_package_id))
                    .set("Package ID", Val::Guid(a.package_id))
                    .set("ID", Val::Guid(a.id))
                    .set("Name", Val::text(&a.name))
                    .set("Publisher", Val::text(&a.publisher))
                    .set("Version Major", Val::Int(a.version[0]))
                    .set("Version Minor", Val::Int(a.version[1]))
                    .set("Version Build", Val::Int(a.version[2]))
                    .set("Version Revision", Val::Int(a.version[3]))
                    .opt("Package Type", "Extension")
                    .opt("Published As", "Global")
                    .set("Tenant ID", Val::text(""))
                    .set("Installed", Val::Bool(true))
                    .row);
            }
        }
        NAV_APP_INSTALLED_APP | INSTALLED_APPLICATION => installed_app_rows(cat, tdef, &schema, &mut put),
        METADATA_PERMISSION | METADATA_PERMISSION_SET_REL | EXPANDED_PERMISSION => crate::permissions::catalog_rows(cat, tdef, &schema, &mut put),
        NAV_APP_EXTRA => {
            // NavAppExtraDataProvider: every loaded app is published Global (visible to the
            // tenant) and installed
            for a in &crate::modules::modules(cat).apps {
                put(RowBuilder::new(tdef, &schema)
                    .set("Runtime Package ID", Val::Guid(a.runtime_package_id))
                    .set("Tenant Visible", Val::Bool(true))
                    .set("PerTenant Or Installed", Val::Bool(true))
                    .set("Package ID", Val::Guid(a.package_id))
                    .row);
            }
        }
        LICENSE_PERMISSION => {
            // no licensing in bc-rust: every object is licensed with full permissions
            let mut add = |ty: &str, id: i64| {
                put(RowBuilder::new(tdef, &schema)
                    .opt("Object Type", ty)
                    .set("Object Number", Val::Int(id as i32))
                    .opt("Read Permission", "Yes")
                    .opt("Insert Permission", "Yes")
                    .opt("Modify Permission", "Yes")
                    .opt("Delete Permission", "Yes")
                    .opt("Execute Permission", "Yes")
                    .row);
            };
            for t in cat.tables.values() {
                add("TableData", t.id);
            }
            for co in &cat.code {
                if let Some(ty) = object_type_member(co.kind) {
                    add(ty, co.id);
                }
            }
        }
        TIME_ZONE => {
            for (n, (id, name)) in windows_time_zones().enumerate() {
                put(RowBuilder::new(tdef, &schema)
                    .set("No.", Val::Int(n as i32 + 1))
                    .set("ID", Val::text(id.chars().take(180).collect::<String>()))
                    .set("Display Name", Val::text(name.chars().take(250).collect::<String>()))
                    .row);
            }
        }
        WINDOWS_LANGUAGE => {
            for &(lcid, abbr, tag, name, cp) in crate::locales::WINDOWS_LANGUAGES {
                // W1 ships the English (United States) translation only
                let installed = lcid == 1033;
                put(RowBuilder::new(tdef, &schema)
                    .set("Language ID", Val::Int(lcid))
                    .set("Primary Language ID", Val::Int(lcid & 0x3ff))
                    .set("Name", Val::text(name))
                    .set("Abbreviated Name", Val::text(abbr))
                    .set("Enabled", Val::Bool(true))
                    .set("Globally Enabled", Val::Bool(true))
                    .set("Form Enabled", Val::Bool(true))
                    .set("Report Enabled", Val::Bool(true))
                    .set("XMLport Enabled", Val::Bool(true))
                    .set("Primary CodePage", Val::text(cp.to_string()))
                    .set("STX File Exist", Val::Bool(installed))
                    .set("ETX File Exist", Val::Bool(installed))
                    .set("Localization Exist", Val::Bool(installed))
                    .set("Language Tag", Val::text(tag))
                    .row);
            }
        }
        FEATURE_KEY => {
            for (id, on, link, by, by_version, can_try, one_way, update, desc) in PLATFORM_FEATURE_KEYS {
                put(RowBuilder::new(tdef, &schema)
                    .set("ID", Val::text(*id))
                    .opt("Enabled", if *on { "All Users" } else { "None" })
                    .set("Description", Val::text(*desc))
                    .set("Learn More Link", Val::text(*link))
                    .set("Mandatory By", Val::text(*by))
                    .set("Can Try", Val::Bool(*can_try))
                    .set("Is One Way", Val::Bool(*one_way))
                    .set("Data Update Required", Val::Bool(*update))
                    .set("Mandatory By Version", Val::text(*by_version))
                    .set("Description In English", Val::text(*desc))
                    .row);
            }
        }
        REPORT_LAYOUT_LIST => {
            // the layouts the loaded reports and report extensions define (Ncl ReportLayoutList):
            // one row per layout, owned by the app of the object that defines it
            let mods = crate::modules::modules(cat);
            for r in cat.reports.values() {
                for l in &r.layout_list {
                    let app = mods.app_of_code(l.code);
                    put(RowBuilder::new(tdef, &schema)
                        .set("Report ID", Val::Int(r.id as i32))
                        .set("Name", Val::text(&l.name))
                        .set("Caption", Val::text(&l.caption))
                        .set("Runtime Package ID", Val::Guid(app.map(|a| a.runtime_package_id).unwrap_or_default()))
                        .opt("Layout Format", if l.kind.eq_ignore_ascii_case("custom") { "Custom" } else { &l.kind })
                        .set("MIME Type", Val::text(&l.mime_type))
                        .set("Description", Val::text(&l.summary))
                        .set("Application ID", Val::Guid(app.map(|a| a.id).unwrap_or_default()))
                        .set("Report Name", Val::text(&r.name))
                        .row);
                }
            }
        }
        API_WEBHOOK_ENTITY => {
            // one row per API page and API version (Ncl ApiWebServiceEntryHelper): Name is the
            // entity set name; Key Fields the ODataKeyFields or else the primary-key field numbers;
            // the first page of an API path wins
            let mods = crate::modules::modules(cat);
            let mut pages: Vec<&al_model::PageDef> = cat.pages.values().filter(|p| p.page_type.eq_ignore_ascii_case("api")).collect();
            pages.sort_by_key(|p| p.id);
            let mut paths = std::collections::HashSet::new();
            let unquote = |v: &str| v.trim().trim_matches('\'').trim_matches('"').to_string();
            for p in pages {
                let prop = |n: &str| p.props.get(n).map(|v| unquote(v)).unwrap_or_default();
                let (entity, set) = (prop("entityname"), prop("entitysetname"));
                let Some(versions) = p.props.get("apiversion") else { continue };
                if entity.is_empty() || set.is_empty() {
                    continue;
                }
                let (publisher, group) = (prop("apipublisher"), prop("apigroup"));
                let table = p.source_table.unwrap_or(0);
                let tdef_src = cat.tables.get(&table);
                let odata_keys: Vec<String> = p.props.get("odatakeyfields").map(|v| v.split(',').map(unquote).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
                let key_no = |names: &[String]| -> String {
                    let Some(t) = tdef_src else { return String::new() };
                    names.iter().filter_map(|n| t.field(n).map(|i| t.fields[i].id.to_string())).collect::<Vec<_>>().join(",")
                };
                let keys = match tdef_src {
                    Some(t) if odata_keys.is_empty() => t.pk().fields.iter().map(|&i| t.fields[i].id.to_string()).collect::<Vec<_>>().join(","),
                    _ => key_no(&odata_keys),
                };
                let temp = p.props.get("sourcetabletemporary").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
                let package = mods.app_of_code(p.code).map(|a| a.package_id).unwrap_or_default();
                for v in versions.split(',').map(unquote).filter(|v| !v.is_empty()) {
                    if !paths.insert(format!("{publisher}/{group}/{v}/{set}").to_ascii_lowercase()) {
                        continue;
                    }
                    put(RowBuilder::new(tdef, &schema)
                        .set("Publisher", Val::text(&publisher))
                        .set("Group", Val::text(&group))
                        .set("Version", Val::text(&v))
                        .set("Name", Val::text(&set))
                        .set("Table No.", Val::Int(table as i32))
                        .set("Table Temporary", Val::Bool(temp))
                        .set("Key Fields", Val::text(&keys))
                        .set("OData Key Specified", Val::Bool(!odata_keys.is_empty()))
                        .opt("Object Type", "Page")
                        .set("Object ID", Val::Int(p.id as i32))
                        .set("Package ID", Val::Guid(package))
                        .row);
                }
            }
        }
        REPORT_METADATA => {
            // the report's properties (Ncl report metadata): DefaultLayout as the rendering uses it
            let mods = crate::modules::modules(cat);
            let unq = |v: &str| v.trim().trim_matches('"').trim_matches('\'').to_string();
            for co in cat.code.iter().filter(|c| c.kind == al_syntax::ObjectKind::Report) {
                let mut b = RowBuilder::new(tdef, &schema).set("ID", Val::Int(co.id as i32)).set("Name", Val::text(&co.name));
                match cat.reports.get(&co.id) {
                    Some(rd) => {
                        let kind = match crate::report::default_layout_kind(rd).as_str() {
                            "word" => "Word",
                            "excel" => "Excel",
                            "custom" | "external" => "Custom",
                            _ => "RDLC",
                        };
                        let first_table = rd.roots.first().and_then(|&i| rd.dataitems[i].table).unwrap_or(0);
                        let app = mods.app_of_code(rd.code);
                        b = b
                            .set("Caption", Val::text(if rd.caption.is_empty() { &co.name } else { &rd.caption }))
                            .set("UseRequestPage", Val::Bool(rd.use_request_page))
                            .set("ProcessingOnly", Val::Bool(rd.processing_only))
                            .opt("DefaultLayout", kind)
                            .set("DefaultLayoutName", Val::text(&rd.props.get("defaultrenderinglayout").map(|v| unq(v)).unwrap_or_default()))
                            .set("WordMergeDataItem", Val::text(&rd.props.get("wordmergedataitem").map(|v| unq(v)).unwrap_or_default()))
                            .set("FirstDataItemTableID", Val::Int(first_table as i32))
                            .set("App ID", Val::Guid(app.map(|a| a.id).unwrap_or_default()));
                    }
                    None => b = b.set("Caption", Val::text(&co.name)).set("UseRequestPage", Val::Bool(true)),
                }
                put(b.row);
            }
        }
        _ => {}
    }
    td
}
