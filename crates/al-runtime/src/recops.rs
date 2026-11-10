//! Built-in Record methods.

use crate::filter::*;
use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_model::{CalcMethod, CondKind, FieldClass, FieldType};
use al_syntax::{Expr, ObjectKind};
use bc_store::{Row, Value};
use rust_decimal::prelude::*;
use std::rc::Rc;

const METHODS: &[&str] = &[
    "get", "find", "findfirst", "findlast", "findset", "next", "insert", "modify", "delete", "modifyall", "deleteall", "rename",
    "init", "reset", "setrange", "setfilter", "getfilter", "getfilters", "getrangemin", "getrangemax", "hasfilter", "copyfilters",
    "copyfilter", "filtergroup", "setcurrentkey", "currentkey", "ascending", "count", "countapprox", "isempty", "calcfields", "calcsums",
    "validate", "testfield", "fielderror", "fieldcaption", "fieldname", "fieldno", "tablecaption", "tablename", "transferfields", "copy",
    "istemporary", "locktable", "readisolation", "setloadfields", "addloadfields", "setautocalcfields", "mark", "markedonly", "clearmarks",
    "recordid", "getposition", "setposition", "getbysystemid", "changecompany", "consistent", "setrecfilter", "haslinks", "deletelinks",
    "copylinks", "addlink", "deletelink", "readpermission", "writepermission", "fieldactive", "securityfiltering", "currentcompany", "areFieldsLoaded",
    "loadfields", "isdirty", "setpermissionfilter", "relation", "testfieldsset", "systemid", "hasfilter", "setview", "getview",
    "setbaseloadfields", "setascending", "getascending",
];

/// A non-text value written into filter text: in single quotes, inner quotes doubled, when it
/// starts with a quote, is blank, or contains = < > ( ) & | or "..". A blank value is `''`
/// [src:A07.1]; the other cases are an open question [src:A07.3].
pub fn quote_filter_value(v: &str) -> String {
    let needs = v.starts_with('\'') || v.trim().is_empty() || v.contains(['=', '<', '>', '(', ')', '&', '|']) || v.contains("..");
    if needs {
        format!("'{}'", v.replace('\'', "''"))
    } else {
        v.to_string()
    }
}

/// A text value written into filter text: quoted when it starts with a quote or '@', starts or
/// ends with whitespace, is blank, holds a wildcard (* ?), = < > ( ) & | or ".." [src:A07.2].
pub fn quote_text_filter_value(v: &str) -> String {
    let needs = v.starts_with('\'')
        || v.starts_with('@')
        || v.starts_with(char::is_whitespace)
        || v.ends_with(char::is_whitespace)
        || v.trim().is_empty()
        || v.contains(['*', '?', '=', '<', '>', '(', ')', '&', '|'])
        || v.contains("..");
    if needs {
        format!("'{}'", v.replace('\'', "''"))
    } else {
        v.to_string()
    }
}

pub fn is_record_method(n: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::LazyLock;
    static SET: LazyLock<HashSet<String>> = LazyLock::new(|| METHODS.iter().map(|m| m.to_ascii_lowercase()).collect());
    al_model::with_lower(n, |l| SET.contains(l))
}

impl<'r> Session<'r> {
    fn field_arg(&mut self, h: &RecHandle, e: &Expr) -> R<usize> {
        let name: &str = match e {
            Expr::Ident(n) => n,
            Expr::Member(_, n) => n,
            other => {
                return err(format!("Expected a field reference, got {other:?}"));
            }
        };
        let t = h.borrow().tdef;
        match t.field(name) {
            Some(i) => Ok(i),
            None => err(format!("Field '{name}' does not exist in table '{}'", t.name)),
        }
    }

    pub fn company_of(&self, h: &RecHandle) -> String {
        h.borrow().company.clone().unwrap_or_else(|| self.company.clone())
    }

    fn not_found_msg(&self, h: &RecHandle) -> String {
        let r = h.borrow();
        format!("The {} does not exist. Identification fields and values: {}", r.tdef.caption, self.key_text(&r))
    }

    /// The primary key as record error texts show it: field captions and formatted values,
    /// `Document Type='Order',No.='1001'` [src:A10.1].
    pub fn key_text(&self, r: &RecState) -> String {
        r.tdef
            .pk()
            .fields
            .iter()
            .map(|&i| {
                let fd = &r.tdef.fields[i];
                format!("{}='{}'", fd.caption, self.format_field_val(fd, &r.get_val(i)))
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn store_err(e: bc_store::StoreError) -> Flow {
        Flow::Err(Box::new(AlError::new(e.to_string())))
    }

    /// Read rows matching the record's filters in its current key order.
    /// The view's next rows, shared with the store where it keeps rows in memory (see rec_get_pk).
    pub fn fetch(&mut self, h: &RecHandle, reverse: bool, after: Option<&Row>, limit: usize) -> R<Vec<crate::record::CowRow>> {
        if h.borrow().custom_key.is_some() {
            return self.fetch_custom(h, reverse, after, limit);
        }
        let (schema, key, rev, filters, temp, company, mark_only, cross, flow) = {
            let r = h.borrow();
            (r.schema.clone(), r.key.min(r.schema.keys.len() - 1), reverse ^ !r.ascending, r.stored_filters(), self.rec_mem_table(&r), r.company.clone(), r.mark_only, r.cross_filters(), r.flowfield_filters())
        };
        let req = bc_store::ScanReq { key, reverse: rev, filters: &filters, after, limit: if mark_only || !cross.is_empty() || !flow.is_empty() { usize::MAX } else { limit }, lock: false };
        use crate::record::CowRow;
        let mut rows: Vec<CowRow> = match temp {
            Some(t) => bc_store::backend::scan_table_shared(&t.borrow(), &req).into_iter().map(CowRow::from).collect(),
            None if crate::virt::is_virtual(schema.id) => crate::virt::rows(&schema, &filters, rev, after, req.limit).into_iter().map(CowRow::from).collect(),
            None if crate::virt::is_catalog_table(schema.id) => match self.catalog_rows(schema.id) {
                Some(t) => bc_store::backend::scan_table_shared(&t, &req).into_iter().map(CowRow::from).collect(),
                None => vec![],
            },
            None => {
                let c = company.unwrap_or_else(|| self.company.clone());
                self.db.scan_shared(&c, &schema, &req).map_err(Self::store_err)?.into_iter().map(CowRow::from).collect()
            }
        };
        // filter group -1 (cross-column search): any of its filters
        if !cross.is_empty() {
            rows.retain(|row| cross.iter().any(|f| f.node.matches(&row[f.field])));
            if !mark_only {
                rows.truncate(limit);
            }
        }
        if mark_only {
            let r = h.borrow();
            rows.retain(|row| r.marks.contains(&schema.pk(row)));
            rows.truncate(limit);
        }
        // filters on FlowFields: the FlowField of each row (with the record's FlowFilters)
        if !flow.is_empty() {
            let probe = crate::record::copy_record(h);
            let mut kept = Vec::new();
            for row in rows {
                probe.borrow_mut().row = row.clone().into();
                let mut ok = true;
                for f in &flow {
                    self.calc_field(&probe, f.field)?;
                    if !f.node.matches(&probe.borrow().row[f.field]) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    kept.push(row);
                    if kept.len() >= limit {
                        break;
                    }
                }
            }
            rows = kept;
        }
        Ok(rows)
    }

    /// Custom sorting (RecState::custom_key): the view's rows in primary key order, sorted here by
    /// the sort fields and then the primary key.
    fn fetch_custom(&mut self, h: &RecHandle, reverse: bool, after: Option<&Row>, limit: usize) -> R<Vec<crate::record::CowRow>> {
        let custom = h.borrow_mut().custom_key.take();
        let rows = self.fetch(h, false, None, usize::MAX);
        h.borrow_mut().custom_key = custom;
        let r = h.borrow();
        let rev = reverse ^ !r.ascending;
        let mut keyed: Vec<(Box<[u8]>, crate::record::CowRow)> = rows?.into_iter().map(|row| (r.order_key(&row), row)).collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        if rev {
            keyed.reverse();
        }
        if let Some(a) = after {
            let ka = r.order_key(a);
            keyed.retain(|(k, _)| if rev { *k < ka } else { *k > ka });
        }
        Ok(keyed.into_iter().take(limit).map(|(_, row)| row).collect())
    }

    /// Every row in the current view (ModifyAll, DeleteAll, ...).
    pub fn fetch_all(&mut self, h: &RecHandle) -> R<Vec<Row>> {
        // owned rows for the per-record paths (a copy where the row is shared, as before)
        Ok(self.fetch(h, false, None, usize::MAX)?.into_iter().map(crate::record::CowRow::into_row).collect())
    }

    fn load_row(&mut self, h: &RecHandle, row: crate::record::CowRow) -> R<()> {
        {
            // the record and its xRec row share the loaded row until one is written (CowRow)
            let mut r = h.borrow_mut();
            r.xrow = Some(row.clone());
            r.row = row;
        }
        let ac = h.borrow().auto_calc.clone();
        for fi in ac {
            self.calc_field(h, fi)?;
        }
        Ok(())
    }

    /// A database write through `h`: other records' read-ahead of the table is stale now (`next`);
    /// `h`'s own stays valid if it was (a loop that modifies or deletes its current record).
    fn note_table_write(&mut self, h: &RecHandle) {
        self.txn_mut().wrote = true;
        let t = h.borrow().schema.id;
        self.write_seq += 1;
        let last = self.table_writes.insert(t, self.write_seq).unwrap_or(0);
        let mut r = h.borrow_mut();
        if last <= r.prefetch.seq {
            r.prefetch.seq = self.write_seq;
        }
    }

    pub fn find_first(&mut self, h: &RecHandle, reverse: bool) -> R<bool> {
        h.borrow_mut().prefetch = Default::default();
        match self.fetch(h, reverse, None, 1)?.into_iter().next() {
            Some(r) => {
                self.load_row(h, r.into())?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn next(&mut self, h: &RecHandle, steps: i64) -> R<i64> {
        if steps == 0 {
            return Ok(0);
        }
        let reverse = steps < 0;
        let batch = if h.borrow().custom_key.is_some() {
            usize::MAX
        } else if self.rec_mem_table(&h.borrow()).is_some() {
            1
        } else {
            self.db.batch_hint()
        };
        let mut moved = 0;
        for _ in 0..steps.unsigned_abs() {
            let (cur, view, written) = {
                let r = h.borrow();
                (r.order_key(&r.row), r.view_fingerprint(), self.table_writes.get(&r.schema.id).copied().unwrap_or(0))
            };
            // serve from the prefetch buffer when the cursor and view are unchanged
            let cached = {
                let mut r = h.borrow_mut();
                let p = &mut r.prefetch;
                // ... and no other record wrote the table since the rows were read (BC reads again:
                // a loop must not visit a row another variable deleted or see its old values)
                if p.reverse == reverse && p.view == view && p.last_key.as_deref() == Some(&cur[..]) && written <= p.seq {
                    p.rows.pop_front()
                } else {
                    None
                }
            };
            let next_row = match cached {
                Some(row) => Some(row),
                None => {
                    let cur_row = h.borrow().row.clone();
                    let mut rows: std::collections::VecDeque<crate::record::CowRow> = self.fetch(h, reverse, Some(&cur_row), batch)?.into_iter().map(crate::record::CowRow::from).collect();
                    let first = rows.pop_front();
                    h.borrow_mut().prefetch = crate::record::Prefetch { rows, last_key: None, view: view.clone(), reverse, seq: self.write_seq };
                    first
                }
            };
            match next_row {
                Some(row) => {
                    let k = {
                        let r = h.borrow();
                        r.order_key(&row)
                    };
                    self.load_row(h, row)?;
                    h.borrow_mut().prefetch.last_key = Some(k);
                    moved += 1;
                }
                None => {
                    h.borrow_mut().prefetch = Default::default();
                    break;
                }
            }
        }
        Ok(if reverse { -moved } else { moved })
    }

    /// `rec_get_pk` for Get and Find('='): on PostgreSQL (bc_store::pg::table_locks) a database
    /// error (BC's lock timeout or deadlock, a lost connection) is an AL error, not "not found".
    /// The transaction is aborted then, and AL code must not go on as if the record were missing.
    pub fn rec_get_pk_r(&mut self, h: &RecHandle) -> R<Option<crate::record::CowRow>> {
        let db_row = {
            let r = h.borrow();
            bc_store::pg::table_locks() && !crate::virt::is_virtual(r.schema.id) && !crate::virt::is_catalog_table(r.schema.id)
        };
        if !db_row || { let r = h.borrow(); self.rec_mem_table(&r).is_some() } {
            return Ok(self.rec_get_pk(h));
        }
        let (schema, row, company) = {
            let r = h.borrow();
            (r.schema.clone(), r.row.clone(), r.company.clone())
        };
        let c = company.unwrap_or_else(|| self.company.clone());
        match self.db.get_shared(&c, &schema, &row, false) {
            Ok(r) => Ok(r.map(crate::record::CowRow::from)),
            Err(bc_store::StoreError::NotFound) => Ok(None),
            Err(e) => Err(Self::store_err(e)),
        }
    }

    /// The stored row with the record's primary key: shared with the store where it keeps rows in
    /// memory (temporary tables, the catalog tables, the memory backend); copied on the first write.
    pub fn rec_get_pk(&mut self, h: &RecHandle) -> Option<crate::record::CowRow> {
        let (schema, row, temp, company) = {
            let r = h.borrow();
            (r.schema.clone(), r.row.clone(), self.rec_mem_table(&r), r.company.clone())
        };
        match temp {
            Some(t) => t.borrow().get(&schema.pk(&row)).map(|r| crate::record::CowRow::from(r.clone())),
            None if crate::virt::is_virtual(schema.id) => crate::virt::get(&schema, &row).map(crate::record::CowRow::from),
            None if crate::virt::is_catalog_table(schema.id) => self.catalog_rows(schema.id).and_then(|t| t.get(&schema.pk(&row)).map(|r| crate::record::CowRow::from(r.clone()))),
            None => {
                let c = company.unwrap_or_else(|| self.company.clone());
                self.db.get_shared(&c, &schema, &row, false).ok().flatten().map(crate::record::CowRow::from)
            }
        }
    }

    /// Rows of a catalog-computed system table. Report Layout List also lists the user-defined
    /// layouts of table 2000000232 "Tenant Report Layout" whose Company Name is this company or
    /// blank (Ncl ReportLayoutDataProvider.FindInternal: report layout definitions, then tenant
    /// layouts, then platform layouts).
    /// Feature Key: the platform keys, each with the state a Modify stored (only platform keys are
    /// listed, as FeatureKeyDataProvider lists them).
    fn feature_key_rows(&mut self, base: std::sync::Arc<bc_store::TableData>) -> std::sync::Arc<bc_store::TableData> {
        let Some(schema) = self.rt.schemas.get(&crate::virt::FEATURE_KEY).cloned() else { return base };
        let req = bc_store::ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
        let company = self.company.clone();
        let rows = match self.db.scan(&company, &schema, &req) {
            Ok(r) if !r.is_empty() => r,
            _ => return base,
        };
        let mut t = (*base).clone();
        for r in rows {
            if t.get(&schema.pk(&r)).is_some() {
                let _ = t.modify(r.to_vec());
            }
        }
        std::sync::Arc::new(t)
    }

    fn catalog_rows(&mut self, id: i64) -> Option<std::sync::Arc<bc_store::TableData>> {
        let base = self.rt.catalog_table(id)?;
        if id == crate::virt::EXPANDED_PERMISSION {
            return Some(self.expanded_permission_rows(base));
        }
        if id == crate::virt::FEATURE_KEY {
            return Some(self.feature_key_rows(base));
        }
        if id != crate::virt::REPORT_LAYOUT_LIST {
            return Some(base);
        }
        let (Some(trl), Some(rll), Some(tschema)) = (self.cat.tables.get(&crate::virt::TENANT_REPORT_LAYOUT), self.cat.tables.get(&id), self.rt.schemas.get(&crate::virt::TENANT_REPORT_LAYOUT).cloned()) else {
            return Some(base);
        };
        let req = bc_store::ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
        let company = self.company.clone();
        let rows = match self.db.scan(&company, &tschema, &req) {
            Ok(r) if !r.is_empty() => r,
            _ => return Some(base),
        };
        let company_field = trl.field("Company Name");
        let mut t = (*base).clone();
        for r in rows {
            if let Some(ci) = company_field {
                let c = match &r[ci] {
                    bc_store::Value::Text(s) => s.clone(),
                    _ => String::new(),
                };
                if !(c.is_empty() || crate::filter::text_eq(&c, &company)) {
                    continue;
                }
            }
            let row = crate::virt::tenant_layout_row(self.cat, rll, &t.schema.clone(), trl, &r);
            let _ = t.insert(row);
        }
        Some(std::sync::Arc::new(t))
    }

    pub fn rec_count(&mut self, h: &RecHandle) -> R<usize> {
        let (schema, filters, temp, company, mark_only) = {
            let r = h.borrow();
            (r.schema.clone(), r.stored_filters(), self.rec_mem_table(&r), r.company.clone(), r.mark_only)
        };
        if mark_only || !h.borrow().cross_filters().is_empty() || !h.borrow().flowfield_filters().is_empty() || crate::virt::is_virtual(schema.id) || crate::virt::is_catalog_table(schema.id) {
            return Ok(self.fetch_all(h)?.len());
        }
        match temp {
            Some(t) => {
                let t = t.borrow();
                if filters.is_empty() {
                    return Ok(t.len());
                }
                let req = bc_store::ScanReq { key: 0, reverse: false, filters: &filters, after: None, limit: usize::MAX, lock: false };
                Ok(bc_store::backend::scan_table(&t, &req).len())
            }
            None => {
                let c = company.unwrap_or_else(|| self.company.clone());
                self.db.count(&c, &schema, &filters).map_err(Self::store_err)
            }
        }
    }

    /// Aggregate over a record's view (CalcSums, FlowFields, AutoIncrement).
    pub fn rec_aggregate(&mut self, h: &RecHandle, col: usize, agg: bc_store::Agg) -> R<bc_store::AggOut> {
        let (schema, filters, temp, company, mark_only) = {
            let r = h.borrow();
            (r.schema.clone(), r.stored_filters(), self.rec_mem_table(&r), r.company.clone(), r.mark_only)
        };
        if temp.is_some() || mark_only || !h.borrow().cross_filters().is_empty() || crate::virt::is_virtual(schema.id) || crate::virt::is_catalog_table(schema.id) {
            let rows = self.fetch_all(h)?;
            return Ok(bc_store::backend::aggregate_rows(rows.iter(), col, agg));
        }
        let c = company.unwrap_or_else(|| self.company.clone());
        self.db.aggregate(&c, &schema, &filters, col, agg).map_err(Self::store_err)
    }

    fn table_event(&mut self, h: &RecHandle, event: &str, element: &str, with_xrec: Option<RecHandle>, run_trigger: Option<bool>, curr_field: Option<i32>) -> R<()> {
        let tid = h.borrow().tdef.id;
        let key = (ObjectKind::Table, tid, event.to_ascii_lowercase(), element.to_ascii_lowercase());
        if !self.cat.subscribers.contains_key(&key) {
            return Ok(());
        }
        let mut args: Vec<(&str, Arg)> = vec![("Rec", Arg::Ref(LVal::Slot(slot(Val::Record(h.clone())), None)))];
        if let Some(x) = with_xrec {
            args.push(("xRec", Arg::Ref(LVal::Slot(slot(Val::Record(x)), None))));
        }
        if let Some(rt) = run_trigger {
            args.push(("RunTrigger", Arg::Val(Val::Bool(rt))));
        }
        if let Some(cf) = curr_field {
            args.push(("CurrFieldNo", Arg::Val(Val::Int(cf))));
        }
        self.raise_event(ObjectKind::Table, tid, event, element, args, None, Some(h.clone()))
    }

    fn run_table_trigger(&mut self, h: &RecHandle, base: Option<al_model::ProcRef>, ext_before: &str, ext_after: &str, xrec: Option<RecHandle>) -> R<()> {
        let tdef = h.borrow().tdef;
        for (n, pr) in &tdef.triggers.ext {
            if n == ext_before {
                self.invoke(*pr, vec![], None, Some(h.clone()), xrec.clone())?;
            }
        }
        if let Some(pr) = base {
            self.invoke(pr, vec![], None, Some(h.clone()), xrec.clone())?;
        }
        for (n, pr) in &tdef.triggers.ext {
            if n == ext_after {
                self.invoke(*pr, vec![], None, Some(h.clone()), xrec.clone())?;
            }
        }
        Ok(())
    }

    fn stamp(&mut self, h: &RecHandle, insert: bool) {
        let now = crate::clock::now_ms(crate::clock::at(self, self.steps));
        // SystemCreatedBy / SystemModifiedBy: the session's UserSecurityId()
        let user = Value::Guid(self.user_sid);
        let mut r = h.borrow_mut();
        let t = r.tdef;
        if let Some(i) = t.field("SystemId") {
            if insert && r.row[i] == Value::Guid([0; 16]) {
                r.row[i] = Value::Guid(crate::clock::new_guid());
            }
        }
        if insert {
            if let Some(i) = t.field("SystemCreatedAt") {
                r.row[i] = Value::DateTime(now);
            }
            if let Some(i) = t.field("SystemCreatedBy") {
                r.row[i] = user.clone();
            }
        }
        if let Some(i) = t.field("SystemModifiedAt") {
            r.row[i] = Value::DateTime(now);
        }
        if let Some(i) = t.field("SystemModifiedBy") {
            r.row[i] = user;
        }
        if let Some(i) = t.field("SystemRowVersion") {
            if let Value::BigInt(v) = r.row[i] {
                r.row[i] = Value::BigInt(v + 1);
            }
        }
    }

    /// Insert ('i'), Modify ('m') or Delete ('d') called from AL code: the triggers' xRec is a copy
    /// of the record as it is (BC assigns OldRecord from the in-memory record when the operation
    /// starts), and it is the record's OldRecord while the operation runs (nested Validates and
    /// procedures see it).
    pub fn rec_op_from_code(&mut self, h: &RecHandle, run_trigger: bool, op: char) -> R<Result<(), String>> {
        let x = crate::record::copy_record(h);
        self.sys.validate_x.push((h.clone(), x.clone()));
        let res = match op {
            'i' => self.rec_insert_x(h, run_trigger, Some(x)),
            'm' => self.rec_modify_x(h, run_trigger, Some(x)),
            _ => self.rec_delete_x(h, run_trigger, Some(x)),
        };
        self.sys.validate_x.pop();
        res
    }

    /// The next AutoIncrement value of a database table as SQL Server's IDENTITY gives it: a
    /// counter that a rollback doesn't reset (in BC a rolled-back insert has still used its
    /// number). It is kept in the internal number sequence "$AutoIncrement:<company>:<table>"
    /// and moved up to the table's maximum when rows were inserted with explicit values. On
    /// with `set_identity_autoincrement` (al-test, whose per-test sequence reset drops these
    /// counters); otherwise AutoIncrement takes the table's maximum plus one.
    fn identity_next(&mut self, h: &RecHandle, maxv: i64) -> R<i64> {
        use bc_store::SeqOp;
        let (company, tid, per_company) = {
            let r = h.borrow();
            (r.company.clone().unwrap_or_else(|| self.company.clone()), r.tdef.id, r.schema.per_company)
        };
        let name = format!("$AutoIncrement:{}:{tid}", if per_company { company.as_str() } else { "" });
        if self.db.seq(SeqOp::Exists(&name)).map_err(Self::store_err)? != Some(1) {
            self.db.seq(SeqOp::Create(&name, maxv + 1, 1)).map_err(Self::store_err)?;
        } else if self.db.seq(SeqOp::Current(&name)).map_err(Self::store_err)?.unwrap_or(0) < maxv {
            self.db.seq(SeqOp::Restart(&name, maxv + 1)).map_err(Self::store_err)?;
        }
        Ok(self.db.seq(SeqOp::Next(&name)).map_err(Self::store_err)?.unwrap_or(maxv + 1))
    }

    /// `ReadIsolation := level`: UpdLock makes the table's reads take row locks until the
    /// transaction ends, as the method form does (bc_store::pg::table_locks, docs/job-queue.md).
    pub fn rec_read_isolation(&mut self, h: &RecHandle, v: &Val) -> R<()> {
        if !bc_store::pg::table_locks() {
            return Ok(());
        }
        let upd = v.as_i64() == 4 || v.to_text().eq_ignore_ascii_case("updlock");
        let (schema, temp) = {
            let r = h.borrow();
            (r.schema.clone(), r.temp.is_some())
        };
        if upd && !temp {
            let c = self.company_of(h);
            self.db.lock_table(&c, &schema).map_err(Self::store_err)?;
        }
        Ok(())
    }

    pub fn rec_insert(&mut self, h: &RecHandle, run_trigger: bool) -> R<Result<(), String>> {
        self.rec_insert_x(h, run_trigger, None)
    }

    fn rec_insert_x(&mut self, h: &RecHandle, run_trigger: bool, xrec: Option<RecHandle>) -> R<Result<(), String>> {
        self.table_event(h, "OnBeforeInsertEvent", "", None, Some(run_trigger), None)?;
        if run_trigger {
            let t = h.borrow().tdef;
            self.run_table_trigger(h, t.triggers.on_insert, "onbeforeinsert", "onafterinsert", xrec)?;
        }
        self.stamp(h, true);
        // AutoIncrement: a feature of BC's SQL data provider (the identity column, Ncl
        // NavSqlDmlCommand), so temporary records don't get it: 0 is inserted as 0 (the Excel
        // sheet-name helper fills a temporary Name/Value Buffer with IDs 0, 1, ...)
        let auto = if h.borrow().is_temp() { None } else { h.borrow().tdef.fields.iter().position(|f| f.auto_increment) };
        if let Some(ai) = auto {
            let zero = matches!(h.borrow().row[ai], Value::Int(0) | Value::BigInt(0));
            if zero {
                let probe = crate::record::copy_record(h);
                {
                    let mut p = probe.borrow_mut();
                    p.filters.clear();
                    p.key = 0;
                    p.custom_key = None;
                    p.mark_only = false;
                }
                let maxv = match self.rec_aggregate(&probe, ai, bc_store::Agg::Max)? {
                    bc_store::AggOut::Value(Some(Value::Int(v))) => v as i64,
                    bc_store::AggOut::Value(Some(Value::BigInt(v))) => v,
                    _ => 0,
                };
                // also with PostgreSQL's locking contract: concurrent sessions need distinct numbers
                let identity = IDENTITY_AUTOINCREMENT.load(std::sync::atomic::Ordering::Relaxed) || bc_store::pg::table_locks();
                let next = if identity && !h.borrow().is_temp() {
                    self.identity_next(h, maxv)?
                } else {
                    maxv + 1
                };
                let mut r = h.borrow_mut();
                r.row[ai] = match r.row[ai] {
                    Value::BigInt(_) => Value::BigInt(next),
                    _ => Value::Int(next as i32),
                };
            }
        }
        let new_id = self.crm_test_new_id(h);
        let (schema, row, temp) = {
            let r = h.borrow();
            (r.schema.clone(), r.row.clone(), self.rec_mem_table(&r))
        };
        if self.sys.perm.active {
            self.check_pending_write(h)?;
        }
        let res = match temp {
            Some(t) => t.borrow_mut().insert(row.to_vec()).map(|_| ()),
            None => {
                let c = self.company_of(h);
                self.db.insert(&c, &schema, &row)
            }
        };
        if let Err(bc_store::StoreError::Other(m)) = res {
            return err(m);
        }
        if res.is_err() {
            let msg = {
                let r = h.borrow();
                format!("The {} already exists. Identification fields and values: {}", r.tdef.caption, self.key_text(&r))
            };
            if let Some(i) = new_id {
                h.borrow_mut().row[i] = Value::Guid([0; 16]);
            }
            return Ok(Err(msg));
        }
        if !h.borrow().is_temp() {
            note_own_write(self.id, &schema, &row, None);
            self.note_table_write(h);
        }
        h.borrow_mut().xrow = Some(row);
        self.table_event(h, "OnAfterInsertEvent", "", None, Some(run_trigger), None)?;
        Ok(Ok(()))
    }

    pub fn rec_modify(&mut self, h: &RecHandle, run_trigger: bool) -> R<Result<(), String>> {
        self.rec_modify_x(h, run_trigger, None)
    }

    /// The stored row for Modify, Delete and Rename, with BC's optimistic concurrency (Ncl
    /// DataAccess.PerformModifyAsync / HandleStaleRecordAsync): a record carries the version it was
    /// read or last written with (`xrow`), and a stored row that differs from it was changed after
    /// the record was retrieved: "Another user has modified the record ..." (DB:RecordChanged).
    /// The row is read with a row lock (PostgreSQL), so no other session changes it until commit.
    /// Records without a version (Init, then Modify) and temporary records are not checked.
    /// On by default; BC_STALE_CHECK=0 turns the check off (`stale_check`).
    fn rec_stored_for_write(&mut self, h: &RecHandle) -> R<Option<crate::record::CowRow>> {
        let plain = {
            let r = h.borrow();
            !stale_check() || r.xrow.is_none() || crate::virt::is_virtual(r.schema.id) || crate::virt::is_catalog_table(r.schema.id)
        };
        if plain || { let r = h.borrow(); self.rec_mem_table(&r).is_some() } {
            return self.rec_get_pk_r(h);
        }
        let (schema, row, company, xrow) = {
            let r = h.borrow();
            (r.schema.clone(), r.row.clone(), r.company.clone(), r.xrow.clone())
        };
        let c = company.unwrap_or_else(|| self.company.clone());
        let stored = match self.db.get_shared(&c, &schema, &row, true) {
            Ok(s) => s.map(crate::record::CowRow::from),
            Err(bc_store::StoreError::NotFound) => None,
            Err(e) => return Err(Self::store_err(e)),
        };
        // a version belongs to the row it was read from: a record whose primary key changed since
        // (DeleteAll/ModifyAll walking copies, TransferFields, key fields assigned) has none for this row
        let same_row = xrow.as_ref().is_some_and(|x| schema.pk(x) == schema.pk(&row));
        if let (Some(s), Some(x), true) = (&stored, &xrow, same_row) {
            let (mut a, mut b) = (s.to_vec(), x.to_vec());
            schema.round_datetimes(&mut a);
            schema.round_datetimes(&mut b);
            // BC: a newer version this session wrote itself (another variable of the same record)
            // is no conflict for a record read without a lock (Ncl PerformModifyAsync, timestamp log),
            // and only a NEWER stored version is stale (Ncl HandleStaleRecordAsync): a stored row
            // that the session's own rolled-back writes had replaced is older than the record's
            if a != b && !own_write(self.id, &schema, &a) && !rolled_back_past(self.id, &schema, &b, &a) {
                let caption = h.borrow().tdef.caption.clone();
                return err_code(
                    "DB:RecordChanged",
                    format!("Another user has modified the record for this {caption} after you retrieved it from the database. Enter your changes again in the updated window, or start the interrupted activity again."),
                );
            }
        }
        Ok(stored)
    }

    /// Modify; xRec `None`: the stored row (a page or API modifying what it read).
    fn rec_modify_x(&mut self, h: &RecHandle, run_trigger: bool, xrec: Option<RecHandle>) -> R<Result<(), String>> {
        let Some(stored) = self.rec_stored_for_write(h)? else { return Ok(Err(self.not_found_msg(h))) };
        // the version this Modify replaces (own-writes log: a rollback past it makes it current again)
        let pre = if stale_check() { Some(row_print(&h.borrow().schema, &stored)) } else { None };
        let x = match xrec {
            Some(x) => x,
            None => {
                let x = crate::record::copy_record(h);
                x.borrow_mut().row = (stored).into();
                x
            }
        };
        self.table_event(h, "OnBeforeModifyEvent", "", Some(x.clone()), Some(run_trigger), None)?;
        if run_trigger {
            let t = h.borrow().tdef;
            self.run_table_trigger(h, t.triggers.on_modify, "onbeforemodify", "onaftermodify", Some(x.clone()))?;
        }
        self.stamp(h, false);
        let (schema, row, temp) = {
            let r = h.borrow();
            (r.schema.clone(), r.row.clone(), self.rec_mem_table(&r))
        };
        if self.sys.perm.active {
            self.check_pending_write(h)?;
        }
        let res = match temp {
            Some(t) => t.borrow_mut().modify(row.to_vec()).map(|_| ()),
            None => {
                let c = self.company_of(h);
                let r = self.db.modify(&c, &schema, &row);
                // a platform feature key's first change stores its row (FeatureKeyDataProvider.Modify)
                if r.is_err() && schema.id == crate::virt::FEATURE_KEY {
                    self.db.insert(&c, &schema, &row)
                } else {
                    r
                }
            }
        };
        if let Err(bc_store::StoreError::Other(m)) = res {
            return err(m);
        }
        if res.is_err() {
            return Ok(Err(self.not_found_msg(h)));
        }
        if !h.borrow().is_temp() {
            note_own_write(self.id, &schema, &row, pre);
            self.note_table_write(h);
        }
        h.borrow_mut().xrow = Some(row);
        self.table_event(h, "OnAfterModifyEvent", "", Some(x), Some(run_trigger), None)?;
        Ok(Ok(()))
    }

    pub fn rec_delete(&mut self, h: &RecHandle, run_trigger: bool) -> R<Result<(), String>> {
        self.rec_delete_x(h, run_trigger, None)
    }

    fn rec_delete_x(&mut self, h: &RecHandle, run_trigger: bool, xrec: Option<RecHandle>) -> R<Result<(), String>> {
        if self.rec_stored_for_write(h)?.is_none() {
            return Ok(Err(self.not_found_msg(h)));
        }
        self.table_event(h, "OnBeforeDeleteEvent", "", None, Some(run_trigger), None)?;
        if run_trigger {
            let t = h.borrow().tdef;
            self.run_table_trigger(h, t.triggers.on_delete, "onbeforedelete", "onafterdelete", xrec)?;
        }
        let (schema, row, temp) = {
            let r = h.borrow();
            (r.schema.clone(), r.row.clone(), self.rec_mem_table(&r))
        };
        if self.sys.perm.active {
            self.check_pending_write(h)?;
        }
        let res = match temp {
            Some(t) => t.borrow_mut().delete(&schema.pk(&row)).map(|_| ()),
            None => {
                let c = self.company_of(h);
                self.db.delete(&c, &schema, &row)
            }
        };
        if let Err(bc_store::StoreError::Other(m)) = res {
            return err(m);
        }
        if res.is_err() {
            return Ok(Err(self.not_found_msg(h)));
        }
        if !h.borrow().is_temp() {
            self.note_table_write(h);
        }
        self.table_event(h, "OnAfterDeleteEvent", "", None, Some(run_trigger), None)?;
        Ok(Ok(()))
    }

    pub fn rec_validate(&mut self, h: &RecHandle, fi: usize, value: Option<Val>, curr_field: i32) -> R<()> {
        self.rec_validate_x(h, fi, value, curr_field, None)
    }

    /// Validate with the given xRec (None: a copy of the record before the new value).
    pub fn rec_validate_x(&mut self, h: &RecHandle, fi: usize, value: Option<Val>, curr_field: i32, xrec: Option<RecHandle>) -> R<()> {
        // A nested Validate (an outer Validate of this record is running) needs its own restore snapshot
        // only if an error trap opened since the outer one began: otherwise an error unwinds through the
        // outer Validate, which restores the record anyway. The snapshot shares the copy-on-write row, so
        // the next write copies the whole row; the outermost Validate's xRec shares it already.
        let addr = Rc::as_ptr(h) as usize;
        let outer = self.sys.validate_traps.iter().rev().find(|(a, _)| *a == addr).map(|(_, t)| *t);
        let snapshot = outer.is_none_or(|t| self.sys.traps_open > t);
        let x = xrec.unwrap_or_else(|| crate::record::copy_record(h));
        self.sys.validate_x.push((h.clone(), x.clone()));
        self.sys.validate_traps.push((addr, self.sys.traps_open));
        let before = if snapshot { Some(h.borrow().row.clone()) } else { None };
        let res = self.rec_validate_in(h, fi, value, curr_field, x);
        self.sys.validate_x.pop();
        self.sys.validate_traps.pop();
        // a Validate that raises an error restores the whole record [src:A11b.3]
        if let (true, Some(b)) = (matches!(res, Err(Flow::Err(_))), before) {
            h.borrow_mut().row = b;
        }
        res
    }

    fn rec_validate_in(&mut self, h: &RecHandle, fi: usize, value: Option<Val>, curr_field: i32, x: RecHandle) -> R<()> {
        if let Some(v) = value {
            let fd = h.borrow().field(fi);
            let sv = to_store(fd, &v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
            h.borrow_mut().row[fi] = sv;
        }
        let fd = h.borrow().field(fi);
        self.table_event(h, "OnBeforeValidateEvent", &fd.name, Some(x.clone()), None, Some(curr_field))?;
        for pr in &fd.before_validate {
            self.invoke(*pr, vec![], None, Some(h.clone()), Some(x.clone()))?;
        }
        self.check_relation(h, fi)?;
        if let Some(pr) = fd.on_validate {
            self.invoke(pr, vec![], None, Some(h.clone()), Some(x.clone()))?;
        }
        for pr in &fd.after_validate {
            self.invoke(*pr, vec![], None, Some(h.clone()), Some(x.clone()))?;
        }
        self.table_event(h, "OnAfterValidateEvent", &fd.name, Some(x), None, Some(curr_field))?;
        Ok(())
    }

    /// TableRelation validation. A conditional relation (`if (Type = const(Item)) Item where(...)
    /// else if ...`) checks the first branch whose conditions the record meets; none: no check.
    fn check_relation(&mut self, h: &RecHandle, fi: usize) -> R<()> {
        let fd = h.borrow().field(fi);
        if !fd.validate_relation || fd.relation.is_empty() {
            return Ok(());
        }
        let branch = fd.relation.iter().position(|b| {
            b.when.iter().all(|c| {
                let r = h.borrow();
                let Some(sfi) = r.tdef.field(&c.field) else { return false };
                let sfd = &r.tdef.fields[sfi];
                let ev = |n: &str| self.enum_ordinal_for_field(sfd, n);
                let node = match c.kind {
                    CondKind::Const => text_to_store(sfd, &c.value, &ev, self.work_date).map(FNode::Eq),
                    CondKind::Filter => {
                        let conv = |s: &str| text_to_store(sfd, s, &ev, self.work_date);
                        parse_filter(&c.value, &conv, matches!(sfd.ty, FieldType::Code(_) | FieldType::Text(_))).ok()
                    }
                    CondKind::Field => None,
                };
                node.is_some_and(|n| n.matches(&r.row[sfi]))
            })
        });
        let Some(branch) = branch else { return Ok(()) };
        let v = h.borrow().row[fi].clone();
        let blank = match &v {
            Value::Text(s) => s.is_empty(),
            Value::Int(0) | Value::BigInt(0) => true,
            _ => false,
        };
        if blank {
            return Ok(());
        }
        let rel = &fd.relation[branch];
        let Some(rt) = self.cat.table_by_name(&rel.table) else { return Ok(()) };
        // a relation to a table without a field is to its first primary-key field (open
        // question [src:A10.4])
        let target_field = match &rel.field {
            Some(f) => rt.field(f),
            None => rt.pk().fields.first().copied(),
        };
        let Some(tf) = target_field else { return Ok(()) };
        let probe = self.new_record(rt.id, false);
        {
            let mut p = probe.borrow_mut();
            p.filters.push(FieldFilter { field: tf, node: FNode::Eq(v.clone()), text: String::new(), group: 0 });
        }
        // the relation's conditions filter the lookup (BC applies the relation's ExpressionDictionary):
        // const() and filter() values, field() = the value of that field on this record
        for c in &rel.conds {
            let Some(cf) = rt.field(&c.field) else { continue };
            let fdef = &rt.fields[cf];
            let ev = |n: &str| self.enum_ordinal_for_field(fdef, n);
            let node = match c.kind {
                CondKind::Const => text_to_store(fdef, &c.value, &ev, self.work_date).map(FNode::Eq),
                CondKind::Filter => {
                    let conv = |s: &str| text_to_store(fdef, s, &ev, self.work_date);
                    parse_filter(&c.value, &conv, matches!(fdef.ty, FieldType::Code(_) | FieldType::Text(_))).ok()
                }
                CondKind::Field => {
                    let r = h.borrow();
                    match r.tdef.field(&c.value) {
                        Some(sfi) if !c.as_filter && !c.upper_limit && r.tdef.fields[sfi].class == FieldClass::Normal => Some(FNode::Eq(r.row[sfi].clone())),
                        _ => None,
                    }
                }
            };
            if let Some(n) = node {
                probe.borrow_mut().filters.push(FieldFilter { field: cf, node: n, text: String::new(), group: 0 });
            }
        }
        if !self.find_first(&probe, false)? {
            let code = {
                let p = probe.borrow();
                let mut ff: Vec<usize> = p.filters.iter().map(|x| x.field).collect();
                ff.sort_unstable();
                ff.dedup();
                let pk = &rt.pk().fields;
                if ff.is_empty() {
                    "DB:TableIsEmpty"
                } else if ff.len() == pk.len() && ff.iter().all(|f| pk.contains(f)) {
                    if pk.len() == 1 { "DB:PrimRecordNotFound" } else { "DB:RecordNotFound" }
                } else {
                    "DB:NothingInsideFilter"
                }
            };
            let r = h.borrow();
            return err_code(code, format!(
                "The field {} of table {} contains a value ({}) that cannot be found in the related table ({}).",
                fd.caption,
                r.tdef.caption,
                to_val(fd, &v).to_text(),
                rt.caption
            ));
        }
        Ok(())
    }

    /// Evaluate a FlowField into the record's row.
    pub fn calc_field(&mut self, h: &RecHandle, fi: usize) -> R<()> {
        let fd = h.borrow().field(fi);
        if fd.class != FieldClass::FlowField {
            return Ok(());
        }
        let Some(cf) = &fd.calc else { return Ok(()) };
        let Some(tt) = self.cat.table_by_name(&cf.table) else { return Ok(()) };
        let target = self.new_record(tt.id, false);
        if h.borrow().company.is_some() {
            target.borrow_mut().company = h.borrow().company.clone();
        }
        for c in &cf.conds {
            let Some(tfi) = tt.field(&c.field) else { continue };
            let tfd = &tt.fields[tfi];
            let nodes: Vec<FNode> = match c.kind {
                CondKind::Const => {
                    let ev = |n: &str| self.enum_ordinal_for_field(tfd, n);
                    text_to_store(tfd, &c.value, &ev, self.work_date).map(FNode::Eq).into_iter().collect()
                }
                CondKind::Filter => {
                    let ev = |n: &str| self.enum_ordinal_for_field(tfd, n);
                    let wd = self.work_date;
                    let conv = |s: &str| text_to_store(tfd, s, &ev, wd);
                    parse_filter(&c.value, &conv, matches!(tfd.ty, FieldType::Code(_) | FieldType::Text(_))).ok().into_iter().collect()
                }
                CondKind::Field => {
                    let r = h.borrow();
                    match r.tdef.field(&c.value) {
                        Some(sfi) if r.tdef.fields[sfi].class == FieldClass::FlowFilter || c.as_filter => {
                            // the filters set on the source FlowFilter field, in every filter group:
                            // BC combines the groups (a lot filter in groups 0 and 8 must meet both)
                            r.filters
                                .iter()
                                .filter(|f| f.field == sfi && f.group != -1)
                                .map(|f| {
                                    if c.upper_limit {
                                        match f.node.range() {
                                            Some((_, Some(hi))) => FNode::Range(None, Some(hi.clone())),
                                            _ => f.node.clone(),
                                        }
                                    } else {
                                        f.node.clone()
                                    }
                                })
                                .collect()
                        }
                        Some(sfi) => vec![FNode::Eq(r.row[sfi].clone())],
                        None => vec![],
                    }
                }
            };
            for n in nodes {
                target.borrow_mut().filters.push(FieldFilter { field: tfi, node: n, text: String::new(), group: 0 });
            }
        }
        // pick the key whose leading fields have the most equality filters
        {
            let mut t = target.borrow_mut();
            let mut best = (0usize, 0usize);
            for (ki, k) in t.schema.keys.iter().enumerate() {
                let n = k.iter().take_while(|kf| t.filters.iter().any(|f| f.field == **kf && f.node.eq_value().is_some())).count();
                if n > best.1 {
                    best = (ki, n);
                }
            }
            t.key = best.0;
        }
        let sum_field = cf.field.as_ref().and_then(|f| tt.field(f));
        let neg = cf.reverse_sign;
        let default_out = || {
            let d = h.borrow().schema.columns[fi];
            store_to_val(&Value::default_for(d))
        };
        use bc_store::{Agg, AggOut};
        let out = match (cf.method, sum_field) {
            (CalcMethod::Count, _) => match self.rec_aggregate(&target, 0, Agg::Count)? {
                AggOut::Count(n) => Val::Int(if neg { -(n as i32) } else { n as i32 }),
                _ => Val::Int(0),
            },
            // `-exist(...)` is the negation: e.g. Sales Invoice Header.Closed = no open ledger entry
            (CalcMethod::Exist, _) => match self.rec_aggregate(&target, 0, Agg::Exists)? {
                AggOut::Exists(b) => Val::Bool(b != neg),
                _ => Val::Bool(neg),
            },
            (CalcMethod::Sum, Some(sf)) | (CalcMethod::Average, Some(sf)) => {
                let sum = match self.rec_aggregate(&target, sf, Agg::Sum)? {
                    AggOut::Sum(d) => d,
                    _ => Decimal::ZERO,
                };
                if cf.method == CalcMethod::Average {
                    let n = match self.rec_aggregate(&target, sf, Agg::Count)? {
                        AggOut::Count(n) => n,
                        _ => 0,
                    };
                    Val::Dec(if n > 0 { sum / Decimal::from(n) } else { Decimal::ZERO })
                } else {
                    Val::Dec(if neg { -sum } else { sum })
                }
            }
            (CalcMethod::Min, Some(sf)) | (CalcMethod::Max, Some(sf)) => {
                let agg = if cf.method == CalcMethod::Min { Agg::Min } else { Agg::Max };
                match self.rec_aggregate(&target, sf, agg)? {
                    AggOut::Value(Some(v)) => store_to_val(&v),
                    _ => default_out(),
                }
            }
            (CalcMethod::Lookup, Some(sf)) => match self.fetch(&target, false, None, 1)?.into_iter().next() {
                Some(row) => store_to_val(&row[sf]),
                None => default_out(),
            },
            _ => default_out(),
        };
        let sv = to_store(fd, &out).unwrap_or_else(|_| Value::default_for(h.borrow().schema.columns[fi]));
        h.borrow_mut().row[fi] = sv;
        Ok(())
    }

    fn filter_node_from_vals(&self, fd: &al_model::FieldDef, from: Val, to: Option<Val>) -> R<FNode> {
        let a = to_store(fd, &from).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
        Ok(match to {
            None => FNode::Eq(a),
            Some(t) => {
                let b = to_store(fd, &t).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                FNode::Range(Some(a), Some(b))
            }
        })
    }

    pub fn parse_field_filter(&self, fd: &al_model::FieldDef, text: &str) -> R<FNode> {
        let ev = |n: &str| self.enum_ordinal_for_field(fd, n);
        let wd = self.work_date;
        let conv = |s: &str| text_to_store(fd, s, &ev, wd);
        parse_filter(text, &conv, matches!(fd.ty, FieldType::Code(_) | FieldType::Text(_)))
            .map_err(|m| Flow::Err(Box::new(AlError::new(format!("{m} (field {})", fd.caption)))))
    }

    pub fn record_method(&mut self, f: &mut Frame, h: &RecHandle, name: &str, args: &[Expr]) -> R<Val> {
        let discard = std::mem::take(&mut self.discard);
        let lname = name.to_ascii_lowercase();
        // table procedures (base + extensions)
        if !is_record_method(&lname) || lname == "relation" {
            let tdef = h.borrow().tdef;
            // page code calling a table procedure on the page's Rec: the table code's xRec is the
            // page's xRec (OnNewRecord → Rec.InitType() reads the line the user came from)
            let page_xrec = if f.rec.as_ref().is_some_and(|r| Rc::ptr_eq(r, h)) && matches!(self.code(f.code).kind, ObjectKind::Page | ObjectKind::PageExtension) {
                // on a new record the page started (never read, no operation running): the page's xRec
                f.xrec.clone().or_else(|| {
                    let none_running = !self.sys.validate_x.iter().any(|(x, _)| Rc::ptr_eq(x, h));
                    let px = f.inst.as_ref().and_then(|i| i.page_xrec.borrow().clone());
                    px.filter(|_| none_running && h.borrow().xrow.is_none()).map(|p| copy_record(&p))
                })
            } else {
                None
            };
            for code in std::iter::once(tdef.code).chain(tdef.ext_code.iter().copied()) {
                if let Some((pr, a)) = self.resolve_call(f, code, name, args)? {
                    return self.invoke(pr, a, None, Some(h.clone()), page_xrec);
                }
            }
            return self.recref_method(f, h, &lname, args);
        }
        let ev = |s: &mut Self, f: &mut Frame, i: usize| -> R<Val> { s.eval(f, &args[i]) };
        // a page background task's child session is read-only (crate::pbt)
        if self.sys.pbt.read_only && matches!(lname.as_str(), "insert" | "modify" | "modifyall" | "delete" | "deleteall" | "rename") && h.borrow().temp.is_none() && !h.borrow().tdef.table_type.eq_ignore_ascii_case("temporary") {
            return err(crate::pbt::READ_ONLY_TEXT);
        }
        // permissions (crate::permissions; off unless enforced: a test's effective sets or a non-SUPER user)
        if self.sys.perm.active {
            if let Some(v) = self.check_record_method(f, h, name, &lname, args, discard)? {
                return Ok(v);
            }
        }
        match lname.as_str() {
            "get" => {
                let mut vals = vec![];
                for i in 0..args.len() {
                    vals.push(ev(self, f, i)?);
                }
                if vals.len() == 1 {
                    if let Val::RecordId(t, k) = &vals[0] {
                        // RecordRef.Get(RecordId) opens the RecordId's table (an unopened RecordRef
                        // is a placeholder Integer record)
                        let (is_ref, cur) = (h.borrow().is_ref, h.borrow().tdef.id);
                        if is_ref && *t != cur && self.cat.tables.contains_key(t) {
                            let fresh = self.new_record(*t, false);
                            let mut st = fresh.borrow().clone();
                            st.is_ref = true;
                            st.company = h.borrow().company.clone();
                            *h.borrow_mut() = st;
                        }
                        // BC RecordImplementation.CheckRecordIdValidity: a Record given the RecordId of another table
                        // finds nothing when the result is used, else it is an error (message text
                        // not from BC's resources); its key values never reach this record
                        if !h.borrow().is_ref && *t != 0 && *t != h.borrow().tdef.id {
                            if discard {
                                let other = self.cat.tables.get(t).map(|x| x.caption.clone()).unwrap_or_else(|| t.to_string());
                                return err(format!("The record ID of table '{other}' cannot be used for table '{}'.", h.borrow().tdef.caption));
                            }
                            return Ok(Val::Bool(false));
                        }
                        let k = k.clone();
                        let pkf = h.borrow().tdef.pk().fields.clone();
                        for (j, &fi) in pkf.iter().enumerate() {
                            if let Some(v) = k.get(j) {
                                h.borrow_mut().row[fi] = v.clone();
                            }
                        }
                        vals.clear();
                    }
                }
                // a Get that finds nothing leaves the record as it was (BC); Sales Line.GetSKU relies
                // on it: a failed SKU.Get must not leave the searched key in the global SKU
                let before = (h.borrow().tdef.id, h.borrow().row.clone());
                {
                    let mut r = h.borrow_mut();
                    let pkf = r.tdef.pk().fields.clone();
                    let mut newrow = r.schema.new_row();
                    for (j, &fi) in pkf.iter().enumerate() {
                        if let Some(v) = vals.get(j) {
                            let fd = r.field(fi);
                            // BC checks the length only for a key value of the field's own type: a
                            // Guid for a Code key converts unchecked and finds nothing (GetRecordAsync)
                            newrow[fi] = match to_store(fd, v) {
                                Err(_) if matches!(fd.ty, FieldType::Code(_) | FieldType::Text(_)) && !matches!(v, Val::Text(_) | Val::Char(_)) => {
                                    Value::Text(if matches!(fd.ty, FieldType::Code(_)) { v.to_text().to_uppercase() } else { v.to_text() })
                                }
                                r => r.map_err(|m| Flow::Err(Box::new(AlError::new(m))))?,
                            };
                        } else if !vals.is_empty() || args.is_empty() {
                            newrow[fi] = Value::default_for(r.schema.columns[fi]);
                        } else {
                            newrow[fi] = r.row[fi].clone();
                        }
                    }
                    for (i, v) in newrow.into_iter().enumerate() {
                        if pkf.contains(&i) {
                            r.row[i] = v;
                        }
                    }
                }
                match self.rec_get_pk_r(h)? {
                    Some(row) => {
                        self.load_row(h, row.into())?;
                        Ok(Val::Bool(true))
                    }
                    None if discard => err_code("DB:RecordNotFound", self.not_found_msg(h)),
                    None => {
                        if h.borrow().tdef.id == before.0 {
                            h.borrow_mut().row = before.1;
                        }
                        Ok(Val::Bool(false))
                    }
                }
            }
            "find" | "findfirst" | "findlast" | "findset" => {
                let which = if lname == "find" {
                    if args.is_empty() { "=".to_string() } else { ev(self, f, 0)?.to_text() }
                } else {
                    String::new()
                };
                let found = match (lname.as_str(), which.as_str()) {
                    ("findlast", _) | ("find", "+") => self.find_first(h, true)?,
                    ("find", "=") => match self.rec_get_pk_r(h)? {
                        Some(row) if h.borrow().row_matches(&row) => {
                            self.load_row(h, row.into())?;
                            true
                        }
                        _ => false,
                    },
                    ("find", ">") => self.next(h, 1)? != 0,
                    ("find", "<") => self.next(h, -1)? != 0,
                    ("find", w) if w.len() > 1 && w.chars().all(|c| matches!(c, '=' | '<' | '>')) => {
                        let mut found = false;
                        for c in w.chars() {
                            found = match c {
                                '=' => match self.rec_get_pk_r(h)? {
                                    Some(row) if h.borrow().row_matches(&row) => {
                                        self.load_row(h, row.into())?;
                                        true
                                    }
                                    _ => false,
                                },
                                '>' => self.next(h, 1)? != 0,
                                _ => self.next(h, -1)? != 0,
                            };
                            if found {
                                break;
                            }
                        }
                        found
                    }
                    _ => self.find_first(h, false)?,
                };
                if !found && discard {
                    let r = h.borrow();
                    // a positioned Find ('=', '>', '<'): the record does not exist (BC: PositionedFindEmpty)
                    if !which.is_empty() && which.chars().all(|c| matches!(c, '=' | '<' | '>')) {
                        let how = if which == "=" { "" } else { which.as_str() };
                        return err_code(
                            "DB:RecordNotFound",
                            format!("The {} does not exist. {}\n\nIdentification fields and values:\n\n{}", r.tdef.caption, how, self.key_text(&r)),
                        );
                    }
                    // no filter in the current filter group: the table is empty (a SetRange to '' is a
                    // filter, though its text is empty here)
                    if !r.filters.iter().any(|x| x.group == r.filter_group) {
                        return err_code("DB:TableIsEmpty", format!("The {} table is empty.", r.tdef.caption));
                    }
                    let filters = self.filters_text(&r);
                    return err_code("DB:NothingInsideFilter", if filters.is_empty() {
                        format!("There is no {} within the filter.", r.tdef.caption)
                    } else {
                        format!("There is no {} within the filter.\n\nFilters: {}", r.tdef.caption, filters)
                    });
                }
                Ok(Val::Bool(found))
            }
            "next" => {
                let n = if args.is_empty() { 1 } else { ev(self, f, 0)?.as_i64() };
                Ok(Val::Int(self.next(h, n)? as i32))
            }
            "insert" | "modify" | "delete" => {
                let rt = if args.is_empty() { false } else { ev(self, f, 0)?.as_bool() };
                // Insert([RunTrigger [, InsertWithSystemId]]): BC gives a record that is not temporary a new
                // SystemId unless InsertWithSystemId is true (RecordImplementation.PopulateSystemdId), so a
                // Copy or TransferFields + Insert never duplicates the source's SystemId
                if lname == "insert" {
                    let with_id = args.len() > 1 && ev(self, f, 1)?.as_bool();
                    let mut r = h.borrow_mut();
                    if let (false, false, Some(i)) = (with_id, r.is_temp(), r.tdef.field("SystemId")) {
                        r.row[i] = Value::Guid([0; 16]);
                    }
                }
                let r = match lname.as_str() {
                    "insert" => self.rec_op_from_code(h, rt, 'i')?,
                    "modify" => self.rec_op_from_code(h, rt, 'm')?,
                    _ => self.rec_op_from_code(h, rt, 'd')?,
                };
                match r {
                    Ok(()) => Ok(Val::Bool(true)),
                    // the store-level failures: a duplicate key, or a record that does not exist
                    Err(m) if discard => err_code(if lname == "insert" { "DB:RecordExists" } else { "DB:RecordNotFound" }, m),
                    Err(_) => Ok(Val::Bool(false)),
                }
            }
            "modifyall" => {
                // ModifyAll(Field, Value [, RunTrigger]); also (F1, V1, F2, V2, ... [, RunTrigger])
                let mut sets = vec![];
                let mut i = 0;
                while i + 1 < args.len() {
                    let fi = self.field_arg(h, &args[i])?;
                    let v = ev(self, f, i + 1)?;
                    let fd = h.borrow().field(fi);
                    let sv = to_store(fd, &v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                    sets.push((fi, sv));
                    i += 2;
                }
                let rt = if i < args.len() { ev(self, f, i)?.as_bool() } else { false };
                let rows = self.fetch_all(h)?;
                let w = crate::record::copy_record(h);
                for row in rows {
                    // each row carries the version it was fetched with (optimistic concurrency)
                    {
                        let mut b = w.borrow_mut();
                        let row: crate::record::CowRow = row.into();
                        b.xrow = Some(row.clone());
                        b.row = row;
                    }
                    for (fi, sv) in &sets {
                        w.borrow_mut().row[*fi] = sv.clone();
                    }
                    // BC: per record SetFieldValue + ALModifyAsync (xRec = the in-memory record)
                    if let Err(m) = self.rec_op_from_code(&w, rt, 'm')? {
                        return err(m);
                    }
                }
                Ok(Val::Void)
            }
            "deleteall" => {
                let rt = if args.is_empty() { false } else { ev(self, f, 0)?.as_bool() };
                let w = crate::record::copy_record(h);
                if rt {
                    // BC: Find('-'), then Delete(true) and Next() on the database: a record an
                    // earlier record's OnDelete removed is not visited
                    if self.find_first(&w, false)? {
                        loop {
                            if let Err(m) = self.rec_delete(&w, rt)? {
                                return err(m);
                            }
                            // rows prefetched before the triggers ran may be gone now
                            w.borrow_mut().prefetch = Default::default();
                            if self.next(&w, 1)? == 0 {
                                break;
                            }
                        }
                    }
                    return Ok(Val::Void);
                }
                let rows = self.fetch_all(h)?;
                for row in rows {
                    // each row carries the version it was fetched with (optimistic concurrency)
                    {
                        let mut b = w.borrow_mut();
                        let row: crate::record::CowRow = row.into();
                        b.xrow = Some(row.clone());
                        b.row = row;
                    }
                    if let Err(m) = self.rec_delete(&w, rt)? {
                        return err(m);
                    }
                }
                Ok(Val::Void)
            }
            "rename" => {
                let mut vals = vec![];
                for i in 0..args.len() {
                    vals.push(ev(self, f, i)?);
                }
                let Some(old) = self.rec_stored_for_write(h)? else { return err_code("DB:RecordNotFound", self.not_found_msg(h)) };
                let x = crate::record::copy_record(h);
                x.borrow_mut().row = (old).into();
                let pkf = h.borrow().tdef.pk().fields.clone();
                for (j, &fi) in pkf.iter().enumerate() {
                    if let Some(v) = vals.get(j) {
                        let fd = h.borrow().field(fi);
                        let sv = to_store(fd, v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                        h.borrow_mut().row[fi] = sv;
                    }
                }
                let t = h.borrow().tdef;
                if let Some(pr) = t.triggers.on_rename {
                    self.invoke(pr, vec![], None, Some(h.clone()), Some(x.clone()))?;
                }
                if let Err(m) = self.rec_delete(&x, false)? {
                    return err(m);
                }
                if let Err(m) = self.rec_insert(h, false)? {
                    return err(m);
                }
                Ok(Val::Bool(true))
            }
            "init" => {
                let mut r = h.borrow_mut();
                let pkf = r.tdef.pk().fields.clone();
                let mut fresh = r.schema.new_row();
                self.init_row(r.tdef, &mut fresh);
                for (i, v) in fresh.into_iter().enumerate() {
                    if !pkf.contains(&i) {
                        r.row[i] = v;
                    }
                }
                Ok(Val::Void)
            }
            "reset" => {
                let mut r = h.borrow_mut();
                r.filters.clear();
                r.key = 0;
                r.custom_key = None;
                r.ascending = true;
                r.mark_only = false;
                r.marks.clear();
                r.filter_group = 0;
                Ok(Val::Void)
            }
            "setrange" => {
                let fi = self.field_arg(h, &args[0])?;
                let fd = h.borrow().field(fi);
                if args.len() == 1 {
                    h.borrow_mut().set_filter(fi, None, String::new());
                    return Ok(Val::Void);
                }
                let a = ev(self, f, 1)?;
                let b = if args.len() > 2 { Some(ev(self, f, 2)?) } else { None };
                let text = match &b {
                    Some(b) => format!("{}..{}", a.to_text(), b.to_text()),
                    None => a.to_text(),
                };
                let node = self.filter_node_from_vals(fd, a, b)?;
                h.borrow_mut().set_filter(fi, Some(node), text);
                Ok(Val::Void)
            }
            "setfilter" => {
                let fi = self.field_arg(h, &args[0])?;
                let fd = h.borrow().field(fi);
                let mut text = ev(self, f, 1)?.to_text();
                for i in 2..args.len() {
                    let v = ev(self, f, i)?;
                    let s = match &v {
                        Val::Text(t) if t.is_empty() => "''".to_string(),
                        Val::Text(t) if t.contains(['|', '&', '(', ')', '<', '>', '=', '*', '.', '@']) => format!("'{}'", t.replace('\'', "''")),
                        Val::Date(d) => format_date(*d),
                        // exact (BC passes %n values typed): RFC 3339 with milliseconds
                        Val::DateTime(t) if *t != bc_store::DATETIME_UNDEFINED => chrono::DateTime::from_timestamp_millis(*t)
                            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                            .unwrap_or_else(|| v.to_text()),
                        _ => v.to_text(),
                    };
                    // a blank value is written quoted: SetFilter(F, '>%1', 0D) is >'' [src:A07.1]
                    let s = if s.is_empty() { "''".to_string() } else { s };
                    text = text.replace(&format!("%{}", i - 1), &s);
                }
                if text.is_empty() {
                    h.borrow_mut().set_filter(fi, None, String::new());
                } else {
                    let node = self.parse_field_filter(fd, &text)?;
                    let text = filter_text_eq(&node, text);
                    h.borrow_mut().set_filter(fi, Some(node), text);
                }
                Ok(Val::Void)
            }
            "getfilter" => {
                // the field's filter in the current filter group (TryGetFilterOnFieldInCurrentGroup)
                let fi = self.field_arg(h, &args[0])?;
                let r = h.borrow();
                // Option/Enum values as captions, as BC formats filters
                Ok(Val::Text(
                    r.filters
                        .iter()
                        .filter(|x| x.field == fi && x.group == r.filter_group)
                        .map(|x| self.option_filter_text(&r.tdef.fields[fi], &x.text, false))
                        .collect::<Vec<_>>()
                        .join("&"),
                ))
            }
            "getfilters" => {
                let r = h.borrow();
                Ok(Val::Text(self.filters_text(&r)))
            }
            "getrangemin" | "getrangemax" => {
                let fi = self.field_arg(h, &args[0])?;
                let r = h.borrow();
                let fd = r.field(fi);
                let fl = r.filters.iter().find(|x| x.field == fi);
                let v = fl.and_then(|x| x.node.range()).and_then(|(a, b)| if lname == "getrangemin" { a.cloned() } else { b.cloned() });
                match v {
                    Some(v) => Ok(to_val(fd, &v)),
                    None => err(format!("The filter on {} is not a range", fd.caption)),
                }
            }
            // filters in the current filter group (HasFilterInCurrentGroup)
            "hasfilter" => {
                let r = h.borrow();
                Ok(Val::Bool(r.filters.iter().any(|x| x.group == r.filter_group)))
            }
            "copyfilters" => {
                let src = match ev(self, f, 0)? {
                    Val::Record(s) => s,
                    _ => return err("CopyFilters expects a record"),
                };
                let fl = src.borrow().filters.clone();
                let same = src.borrow().tdef.id == h.borrow().tdef.id;
                if same {
                    h.borrow_mut().filters = fl;
                }
                Ok(Val::Void)
            }
            "copyfilter" => {
                let fi = self.field_arg(h, &args[0])?;
                // the target field as any field reference resolves: Rec.Field, a field of the
                // implicit Rec / data item, of a `with` record, of an array element
                let (target, tfi) = match self.lvalue(f, &args[1])? {
                    LVal::Field(t, i) => (t, i),
                    _ => return err("CopyFilter target must be a record field"),
                };
                // like To.SetFilter(ToField, From.GetFilter(FromField)): the current filter group
                // of each record; no filter on the source field removes the target's
                let fl: Vec<_> = {
                    let r = h.borrow();
                    r.filters.iter().filter(|x| x.field == fi && x.group == r.filter_group).cloned().collect()
                };
                let mut t = target.borrow_mut();
                let g = t.filter_group;
                t.filters.retain(|x| !(x.field == tfi && x.group == g));
                for mut x in fl {
                    x.field = tfi;
                    x.group = g;
                    t.filters.push(x);
                }
                Ok(Val::Void)
            }
            "filtergroup" => {
                let old = h.borrow().filter_group;
                if !args.is_empty() {
                    let g = ev(self, f, 0)?.as_i64() as i32;
                    h.borrow_mut().filter_group = g;
                }
                Ok(Val::Int(old))
            }
            "setcurrentkey" => {
                let mut fields = vec![];
                for a in args {
                    fields.push(self.field_arg(h, a)?);
                }
                h.borrow_mut().set_sort_fields(&fields);
                Ok(Val::Bool(true))
            }
            "currentkey" => {
                let r = h.borrow();
                let fields = r.custom_key.clone().unwrap_or_else(|| r.schema.keys[r.key].clone());
                Ok(Val::Text(fields.iter().map(|&i| r.tdef.fields[i].name.clone()).collect::<Vec<_>>().join(",")))
            }
            "ascending" => {
                if !args.is_empty() {
                    let a = ev(self, f, 0)?.as_bool();
                    h.borrow_mut().ascending = a;
                }
                Ok(Val::Bool(h.borrow().ascending))
            }
            // SetAscending(Field, Ascending) / GetAscending(Field): the sort direction of a key field.
            // The whole current key shares one direction here.
            "setascending" => {
                if args.len() >= 2 {
                    let _ = self.field_arg(h, &args[0])?;
                    let a = ev(self, f, 1)?.as_bool();
                    h.borrow_mut().ascending = a;
                }
                Ok(Val::Void)
            }
            "getascending" => {
                if let Some(e) = args.first() {
                    let _ = self.field_arg(h, e)?;
                }
                Ok(Val::Bool(h.borrow().ascending))
            }
            "count" | "countapprox" => Ok(Val::Int(self.rec_count(h)? as i32)),
            "isempty" => Ok(Val::Bool(self.fetch(h, false, None, 1)?.is_empty())),
            "calcfields" | "setautocalcfields" => {
                let mut fis = vec![];
                for a in args {
                    fis.push(self.field_arg(h, a)?);
                }
                if lname == "setautocalcfields" {
                    h.borrow_mut().auto_calc = fis;
                } else {
                    for fi in fis {
                        self.calc_field(h, fi)?;
                    }
                }
                Ok(Val::Bool(true))
            }
            "calcsums" => {
                let mut fis = vec![];
                for a in args {
                    fis.push(self.field_arg(h, a)?);
                }
                let mut sums = vec![Decimal::ZERO; fis.len()];
                for (k, &fi) in fis.iter().enumerate() {
                    if let bc_store::AggOut::Sum(d) = self.rec_aggregate(h, fi, bc_store::Agg::Sum)? {
                        sums[k] = d;
                    }
                }
                for (k, &fi) in fis.iter().enumerate() {
                    let fd = h.borrow().field(fi);
                    let sv = to_store(fd, &Val::Dec(sums[k])).unwrap_or(Value::Dec(sums[k]));
                    h.borrow_mut().row[fi] = sv;
                }
                Ok(Val::Bool(true))
            }
            "validate" => {
                let fi = self.field_arg(h, &args[0])?;
                let v = if args.len() > 1 { Some(ev(self, f, 1)?) } else { None };
                // Validate from code: the events get the record's CurrFieldNo (0 unless the UI or
                // AL code set it), as in BC (crate::currfield)
                let cf = self.curr_field_no(h);
                // a Validate inside a Validate chain on the same record keeps the chain's xRec
                let outer_x = self.sys.validate_x.iter().rev().find(|(r, _)| Rc::ptr_eq(r, h)).map(|(_, x)| x.clone());
                self.rec_validate_x(h, fi, v, cf, outer_x)?;
                Ok(Val::Void)
            }
            "testfield" => {
                let fi = self.field_arg(h, &args[0])?;
                let cur = h.borrow().get_val(fi);
                let (fd, tcap, pk) = {
                    let r = h.borrow();
                    (r.field(fi), r.tdef.caption.clone(), r.pk_text())
                };
                let want = if args.len() > 1 { Some(ev(self, f, 1)?) } else { None };
                let want = match want {
                    Some(Val::ErrorInfo(_)) => None, // TestField(Field, ErrorInfo.Create())
                    w => w,
                };
                if let Some(want) = want {
                    let want_s = to_store(fd, &want).map(|v| to_val(fd, &v)).unwrap_or(want);
                    if !equals(&cur, &want_s) {
                        let show = |v: &Val| self.format_field_val(fd, v);
                        return err_code("TestField", format!(
                            "{} must be equal to '{}'  in {}: {}. Current value is '{}'.",
                            fd.caption,
                            show(&want_s),
                            tcap,
                            pk,
                            show(&cur)
                        ));
                    }
                } else {
                    let blank = match &cur {
                        Val::Text(s) => s.is_empty(),
                        Val::Bool(b) => !b,
                        Val::Date(d) | Val::Time(d) => *d == 0,
                        Val::Guid(g) => *g == [0; 16],
                        v if v.is_numeric() => v.as_dec().is_zero(),
                        Val::Opt { ord, .. } => *ord == 0,
                        _ => false,
                    };
                    if blank {
                        return err_code("TestField", format!("{} must have a value in {}: {}. It cannot be zero or empty.", fd.caption, tcap, pk));
                    }
                }
                Ok(Val::Void)
            }
            "fielderror" => {
                let fi = self.field_arg(h, &args[0])?;
                let txt = if args.len() > 1 { ev(self, f, 1)?.to_text() } else { String::new() };
                let msg = self.field_error_text(&h.borrow(), fi, &txt);
                // FieldError's error code [src:A11a]
                err_code("NCLCSRTS:TableErrorStr", msg)
            }
            "fieldcaption" => {
                let fi = self.field_arg(h, &args[0])?;
                Ok(Val::Text(h.borrow().field(fi).caption.clone()))
            }
            "fieldname" => {
                let fi = self.field_arg(h, &args[0])?;
                Ok(Val::Text(h.borrow().field(fi).name.clone()))
            }
            "fieldno" => {
                let fi = self.field_arg(h, &args[0])?;
                Ok(Val::Int(h.borrow().field(fi).id as i32))
            }
            "tablecaption" => Ok(Val::Text(h.borrow().tdef.caption.clone())),
            "tablename" => Ok(Val::Text(h.borrow().tdef.name.clone())),
            "transferfields" => {
                let src = match ev(self, f, 0)? {
                    Val::Record(s) => s,
                    _ => return err("TransferFields expects a record"),
                };
                let init_pk = if args.len() > 1 { ev(self, f, 1)?.as_bool() } else { true };
                let (srow, sdef) = {
                    let s = src.borrow();
                    (s.row.clone(), s.tdef)
                };
                let mut r = h.borrow_mut();
                let pkf = r.tdef.pk().fields.clone();
                for (di, fd) in r.tdef.fields.iter().enumerate() {
                    if fd.class != FieldClass::Normal || fd.is_system() || (!init_pk && pkf.contains(&di)) {
                        continue;
                    }
                    if let Some(si) = sdef.field_no(fd.id) {
                        let v = &srow[si];
                        if std::mem::discriminant(v) == std::mem::discriminant(&r.row[di]) {
                            let v = match (v, &fd.ty) {
                                (Value::Text(s), al_model::FieldType::Code(n)) | (Value::Text(s), al_model::FieldType::Text(n)) => {
                                    Value::Text(s.chars().take(*n as usize).collect())
                                }
                                _ => v.clone(),
                            };
                            r.row[di] = v;
                        }
                    }
                }
                Ok(Val::Void)
            }
            "copy" => {
                let src = match ev(self, f, 0)? {
                    Val::Record(s) => s,
                    _ => return err("Copy expects a record"),
                };
                let share = if args.len() > 1 { ev(self, f, 1)?.as_bool() } else { false };
                if Rc::ptr_eq(&src, h) {
                    return Ok(Val::Void);
                }
                let s = src.borrow().clone();
                let mut r = h.borrow_mut();
                r.row = s.row;
                r.xrow = s.xrow;
                r.filters = s.filters;
                r.key = s.key;
                r.custom_key = s.custom_key;
                r.ascending = s.ascending;
                r.marks = s.marks;
                r.mark_only = s.mark_only;
                // only Copy(From, true) makes two temporary records share one table; the default
                // copies the current record, filters and key and keeps this record's own table
                if share {
                    r.temp = s.temp;
                }
                Ok(Val::Void)
            }
            "istemporary" => Ok(Val::Bool(h.borrow().is_temp())),
            "locktable" => {
                let (schema, temp) = {
                    let r = h.borrow();
                    (r.schema.clone(), r.temp.is_some())
                };
                if !temp {
                    let c = self.company_of(h);
                    self.db.lock_table(&c, &schema).map_err(Self::store_err)?;
                }
                Ok(Val::Bool(true))
            }
            // ReadIsolation(UpdLock): reads of the table take row locks (FOR UPDATE) until the
            // transaction ends, as after LockTable (No. Series GetNextNo read the same Last No. Used
            // in two sessions; docs/job-queue.md). Only with bc_store::pg::table_locks() (bc serve's
            // background pool); the other levels are MVCC reads.
            "readisolation" if args.len() == 1 && bc_store::pg::table_locks() => {
                let v = self.eval(f, &args[0])?;
                let upd = v.as_i64() == 4 || v.to_text().eq_ignore_ascii_case("updlock");
                let (schema, temp) = {
                    let r = h.borrow();
                    (r.schema.clone(), r.temp.is_some())
                };
                if upd && !temp {
                    let c = self.company_of(h);
                    self.db.lock_table(&c, &schema).map_err(Self::store_err)?;
                }
                Ok(Val::Bool(true))
            }
            // load hints: every field is always loaded
            "readisolation" | "setloadfields" | "addloadfields" | "setbaseloadfields" | "consistent" | "securityfiltering" | "setpermissionfilter"
            | "loadfields" | "testfieldsset" => {
                for a in args {
                    if !matches!(a, Expr::Ident(_)) {
                        let _ = self.eval(f, a);
                    }
                }
                match lname.as_str() {
                    "consistent" => crate::assume::hit("Record.Consistent"),
                    "securityfiltering" | "setpermissionfilter" => crate::assume::hit("Record.SecurityFiltering"),
                    _ => {}
                }
                Ok(Val::Bool(true))
            }
            "readpermission" | "writepermission" => Ok(Val::Bool(self.record_permission(f.code, h, lname == "writepermission"))),
            "fieldactive" | "arefieldsloaded" => Ok(Val::Bool(true)),
            // AddLink / HasLinks / DeleteLinks / DeleteLink / CopyLinks: the Record Link table (crate::reclinks)
            l if crate::reclinks::is_link_method(l) => self.record_link_method(f, h, l, args),
            "isdirty" => {
                crate::assume::hit("Record.IsDirty");
                Ok(Val::Bool(false))
            }
            "mark" => {
                let pk = h.borrow().pk();
                if args.is_empty() {
                    return Ok(Val::Bool(h.borrow().marks.contains(&pk)));
                }
                let on = ev(self, f, 0)?.as_bool();
                let mut r = h.borrow_mut();
                if on {
                    r.marks.insert(pk);
                } else {
                    r.marks.remove(&pk);
                }
                Ok(Val::Bool(on))
            }
            "markedonly" => {
                if !args.is_empty() {
                    let on = ev(self, f, 0)?.as_bool();
                    h.borrow_mut().mark_only = on;
                }
                Ok(Val::Bool(h.borrow().mark_only))
            }
            "clearmarks" => {
                h.borrow_mut().marks.clear();
                Ok(Val::Void)
            }
            "recordid" => {
                let r = h.borrow();
                Ok(Val::RecordId(r.tdef.id, r.tdef.pk().fields.iter().map(|&i| r.row[i].clone()).collect()))
            }
            "getposition" => {
                let r = h.borrow();
                Ok(Val::Text(
                    r.tdef.pk().fields.iter().map(|&i| format!("{}=CONST({})", r.tdef.fields[i].name, r.get_val(i).to_text())).collect::<Vec<_>>().join(","),
                ))
            }
            "setposition" => {
                let s = ev(self, f, 0)?.to_text();
                let tdef = h.borrow().tdef;
                for part in s.split(",") {
                    if let Some((fname, rest)) = part.split_once("=CONST(") {
                        if let Some(fi) = tdef.field(fname.trim()) {
                            let v = rest.trim_end_matches(')');
                            let fd = &tdef.fields[fi];
                            let evf = |n: &str| self.enum_ordinal_for_field(fd, n);
                            if let Some(sv) = text_to_store(fd, v, &evf, self.work_date) {
                                h.borrow_mut().row[fi] = sv;
                            }
                        }
                    }
                }
                Ok(Val::Void)
            }
            "getbysystemid" => {
                let g = ev(self, f, 0)?;
                let Some(si) = h.borrow().tdef.field("SystemId") else { return Ok(Val::Bool(false)) };
                let probe = crate::record::copy_record(h);
                {
                    let mut p = probe.borrow_mut();
                    p.filters.clear();
                    let gv = match g {
                        Val::Guid(x) => Value::Guid(x),
                        o => Value::Guid(parse_guid(&o.to_text()).unwrap_or([0; 16])),
                    };
                    p.filters.push(FieldFilter { field: si, node: FNode::Eq(gv), text: String::new(), group: 0 });
                }
                let ok = self.find_first(&probe, false)?;
                if ok {
                    let row = probe.borrow().row.clone();
                    self.load_row(h, row)?;
                } else if discard {
                    return err(format!("The {} does not exist.", h.borrow().tdef.caption));
                }
                Ok(Val::Bool(ok))
            }
            "systemid" => {
                let r = h.borrow();
                Ok(r.tdef.field("SystemId").map(|i| r.get_val(i)).unwrap_or(Val::Guid([0; 16])))
            }
            "changecompany" => {
                let c = if args.is_empty() { None } else { Some(ev(self, f, 0)?.to_text()) };
                h.borrow_mut().company = c.filter(|s| !s.is_empty());
                Ok(Val::Bool(true))
            }
            "currentcompany" => Ok(Val::Text(self.company_of(h))),
            "setrecfilter" => {
                let mut r = h.borrow_mut();
                let pkf = r.tdef.pk().fields.clone();
                for fi in pkf {
                    let v = r.row[fi].clone();
                    let text = to_val(r.field(fi), &v).to_text();
                    r.set_filter(fi, Some(FNode::Eq(v)), text);
                }
                Ok(Val::Void)
            }
            "getview" => {
                let use_captions = if args.is_empty() { true } else { ev(self, f, 0)?.as_bool() };
                let r = h.borrow();
                Ok(Val::Text(self.view_text(&r, use_captions)))
            }
            "setview" => {
                let v = ev(self, f, 0)?.to_text();
                self.set_view(h, &v)?;
                Ok(Val::Void)
            }
            other => err(format!("Record method {other} is not implemented")),
        }
    }

    /// RecordRef-only methods (the RecordRef holds an ordinary record handle).
    fn recref_method(&mut self, f: &mut Frame, h: &RecHandle, lname: &str, args: &[Expr]) -> R<Val> {
        let tdef = h.borrow().tdef;
        let mut a = vec![];
        for x in args {
            a.push(self.eval(f, x)?);
        }
        if h.borrow().unopened && !matches!(lname, "open" | "gettable" | "number" | "close" | "isdirty" | "name" | "caption") {
            return err(format!("The RecordRef is not open (calling {lname})"));
        }
        Ok(match lname {
            "systemidno" => Val::Int(2000000000),
            "systemcreatedatno" => Val::Int(2000000001),
            "systemcreatedbyno" => Val::Int(2000000002),
            "systemmodifiedatno" => Val::Int(2000000003),
            "systemmodifiedbyno" => Val::Int(2000000004),
            "open" => {
                let tid = a.first().map(|v| v.as_i64()).unwrap_or(0);
                if !self.cat.tables.contains_key(&tid) {
                    return err(format!("Table {tid} does not exist"));
                }
                let temp = a.get(1).is_some_and(|v| v.as_bool());
                let fresh = self.new_record(tid, temp);
                let mut st = fresh.borrow().clone();
                st.is_ref = true;
                st.company = a.get(2).map(|c| c.to_text()).filter(|s| !s.is_empty());
                *h.borrow_mut() = st;
                Val::Void
            }
            "number" if h.borrow().unopened => Val::Int(0),
            "name" | "caption" if h.borrow().unopened => Val::Text(String::new()),
            "number" => Val::Int(tdef.id as i32),
            "name" => Val::Text(tdef.name.clone()),
            "caption" => Val::Text(tdef.caption.clone()),
            "close" => {
                if let Val::Record(fresh) = self.new_recordref() {
                    let st = fresh.borrow().clone();
                    *h.borrow_mut() = st;
                }
                Val::Void
            }
            "field" => {
                let no = a.first().map(|v| v.as_i64()).unwrap_or(0);
                match crate::record::field_by_no(tdef, no) {
                    Some(i) => Val::FieldRef(h.clone(), i),
                    None => return err(format!("Field {no} does not exist in table {}", tdef.name)),
                }
            }
            // FieldCount / FieldIndex count the table's fields without the system fields
            // (NavRecord.ALFieldCount = MetaTable.FieldCount - SystemFieldCount)
            "fieldindex" => {
                let i = a.first().map(|v| v.as_i64()).unwrap_or(1).max(1) as usize - 1;
                match tdef.fields.iter().enumerate().filter(|(_, f)| !f.is_system()).nth(i) {
                    Some((fi, _)) => Val::FieldRef(h.clone(), fi),
                    None => return err(format!("FieldIndex {} out of range", i + 1)),
                }
            }
            "fieldcount" => Val::Int(tdef.fields.iter().filter(|f| !f.is_system()).count() as i32),
            "fieldexist" => Val::Bool(a.first().is_some_and(|v| crate::record::field_by_no(tdef, v.as_i64()).is_some())),
            "keycount" => Val::Int(tdef.keys.len() as i32),
            "currentkeyindex" => {
                // CurrentKeyIndex(Index) selects a key by its number, from 1 [src:A06.8]. The CRM
                // coupling lookup selects the Name key this way before it completes a typed name.
                if let Some(v) = a.first() {
                    let i = v.as_i64();
                    let kf = if i >= 1 { tdef.keys.get(i as usize - 1).map(|k| k.fields.clone()) } else { None };
                    let mut r = h.borrow_mut();
                    match kf.and_then(|kf| r.schema.keys.iter().position(|sk| *sk == kf)) {
                        Some(k) => {
                            r.key = k;
                            r.custom_key = None;
                        }
                        None => return err(format!("The {} table has no key with index {i}.", tdef.name)),
                    }
                }
                let r = h.borrow();
                let kf = &r.schema.keys[r.key];
                Val::Int(tdef.keys.iter().position(|k| &k.fields == kf).map_or(1, |p| p as i32 + 1))
            }
            "keyindex" => {
                let i = a.first().map(|v| v.as_i64()).unwrap_or(1);
                if i < 1 || i as usize > tdef.keys.len() {
                    return err(format!("The key index {i} is out of range for table {}", tdef.name));
                }
                Val::KeyRef(h.clone(), i as usize - 1)
            }
            "gettable" => {
                if let Some(Val::Record(src)) = a.first() {
                    if !Rc::ptr_eq(src, h) {
                        let mut copy = src.borrow().clone();
                        copy.is_ref = true;
                        copy.unopened = false;
                        *h.borrow_mut() = copy;
                    }
                }
                Val::Void
            }
            "settable" => {
                if let Some(Val::Record(dst)) = a.first() {
                    let src = h.borrow();
                    let mut d = dst.borrow_mut();
                    if d.tdef.id == src.tdef.id {
                        d.row = src.row.clone();
                        d.xrow = src.xrow.clone();
                        d.filters = src.filters.clone();
                        d.key = src.key;
                        d.custom_key = src.custom_key.clone();
                    }
                }
                Val::Void
            }
            "duplicate" => Val::Record(crate::record::copy_record(h)),
            "isdirty" => Val::Bool(false),
            _ => return err(format!("Method '{lname}' does not exist on table '{}'", tdef.name)),
        })
    }

    pub fn fieldref_method(&mut self, f: &mut Frame, h: &RecHandle, fi: usize, base: &Expr, lname: &str, args: &[Expr]) -> R<Val> {
        if let Some(m) = crate::record::stale_fieldref(h, fi) {
            return err(m);
        }
        let fd = h.borrow().field(fi);
        let mut a = vec![];
        for x in args {
            a.push(self.eval(f, x)?);
        }
        Ok(match lname {
            "value" => {
                if let Some(v) = a.into_iter().next() {
                    let sv = to_store(fd, &v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                    h.borrow_mut().row[fi] = sv;
                    Val::Void
                } else {
                    h.borrow().get_val(fi)
                }
            }
            "number" => Val::Int(fd.id as i32),
            "name" => Val::Text(fd.name.clone()),
            "caption" => Val::Text(fd.caption.clone()),
            "length" => Val::Int(match fd.ty {
                FieldType::Code(n) | FieldType::Text(n) => n as i32,
                _ => 0,
            }),
            "type" => Val::Opt {
                ord: crate::interp::system_option_ord(
                    "fieldtype",
                    match &fd.ty {
                        FieldType::Integer => "integer",
                        FieldType::Decimal => "decimal",
                        FieldType::Text(_) => "text",
                        FieldType::Code(_) => "code",
                        FieldType::Boolean => "boolean",
                        FieldType::Date => "date",
                        FieldType::Time => "time",
                        FieldType::DateTime => "datetime",
                        FieldType::Option | FieldType::Enum(_) => "option",
                        FieldType::Guid => "guid",
                        FieldType::BigInteger => "biginteger",
                        FieldType::Blob => "blob",
                        FieldType::Media => "media",
                        FieldType::MediaSet => "mediaset",
                        FieldType::DateFormula => "dateformula",
                        FieldType::Duration => "duration",
                        FieldType::RecordId => "recordid",
                        FieldType::TableFilter => "tablefilter",
                        FieldType::Other(_) => "text",
                    },
                ),
                enum_id: crate::optmeta::for_system("fieldtype"),
            },
            "class" => Val::Opt {
                ord: match fd.class {
                    FieldClass::Normal => 0,
                    FieldClass::FlowField => 1,
                    FieldClass::FlowFilter => 2,
                },
                enum_id: None,
            },
            "active" => Val::Bool(true),
            "isenum" => Val::Bool(matches!(fd.ty, FieldType::Enum(_))),
            // Enum fields: the enum's values in ordinal order (names / captions) [src:A17.3]
            "optionmembers" | "optioncaption" | "optionstring" if matches!(fd.ty, FieldType::Enum(_)) => {
                let FieldType::Enum(en) = &fd.ty else { unreachable!() };
                let mut vals: Vec<&al_model::EnumValue> = self.cat.enum_by_name(en).map(|e| e.values.iter().collect()).unwrap_or_default();
                vals.sort_by_key(|v| v.ordinal);
                let caption = lname != "optionmembers";
                Val::Text(vals.iter().map(|v| if caption && !v.caption.is_empty() { v.caption.as_str() } else { v.name.as_str() }).collect::<Vec<_>>().join(","))
            }
            "optionmembers" => Val::Text(fd.options.join(",")),
            "optioncaption" | "optionstring" => Val::Text(fd.option_captions.join(",")),
            "relation" => Val::Int(fd.relation.first().and_then(|r| self.cat.table_by_name(&r.table)).map(|t| t.id as i32).unwrap_or(0)),
            "record" => Val::Record(h.clone()),
            "calcfield" => {
                self.calc_field(h, fi)?;
                Val::Bool(true)
            }
            "validate" => {
                let v = a.into_iter().next();
                let cf = self.curr_field_no(h);
                self.rec_validate(h, fi, v, cf)?;
                Val::Void
            }
            "setrange" => {
                if a.is_empty() {
                    h.borrow_mut().set_filter(fi, None, String::new());
                } else {
                    let text = if a.len() > 1 { format!("{}..{}", a[0].to_text(), a[1].to_text()) } else { a[0].to_text() };
                    let mut it = a.into_iter();
                    let from = it.next().unwrap();
                    let node = self.filter_node_from_vals(fd, from, it.next())?;
                    h.borrow_mut().set_filter(fi, Some(node), text);
                }
                Val::Void
            }
            "setfilter" => {
                let mut text = a.first().map(|v| v.to_text()).unwrap_or_default();
                for (k, v) in a.iter().enumerate().skip(1) {
                    text = text.replace(&format!("%{k}"), &v.to_text());
                }
                if text.is_empty() {
                    h.borrow_mut().set_filter(fi, None, String::new());
                } else {
                    let node = self.parse_field_filter(fd, &text)?;
                    let text = filter_text_eq(&node, text);
                    h.borrow_mut().set_filter(fi, Some(node), text);
                }
                Val::Void
            }
            "getfilter" => {
                let r = h.borrow();
                Val::Text(r.filters.iter().filter(|x| x.field == fi).map(|x| self.option_filter_text(&r.tdef.fields[fi], &x.text, false)).collect::<Vec<_>>().join("&"))
            }
            "testfield" => {
                let _ = base;
                let cur = h.borrow().get_val(fi);
                let blank = match &cur {
                    Val::Text(s) => s.is_empty(),
                    v if v.is_numeric() => v.as_dec().is_zero(),
                    Val::Bool(b) => !b,
                    _ => false,
                };
                if blank {
                    let r = h.borrow();
                    return err(format!("{} must have a value in {}: {}. It cannot be zero or empty.", fd.caption, r.tdef.caption, r.pk_text()));
                }
                Val::Void
            }
            "enumvaluecount" => Val::Int(match &fd.ty {
                FieldType::Enum(e) => self.cat.enum_by_name(e).map(|e| e.values.len() as i32).unwrap_or(0),
                _ => fd.options.len() as i32,
            }),
            "getenumvalueordinal" | "getenumvaluename" | "getenumvaluecaption" | "getenumvaluenamefromordinalvalue" | "getenumvaluecaptionfromordinalvalue" => {
                // (ordinal, name, caption) of the field's enum values (option members for Option fields)
                let values: Vec<(i64, String, String)> = match &fd.ty {
                    FieldType::Enum(e) => self.cat.enum_by_name(e).map(|e| e.values.iter().map(|v| (v.ordinal, v.name.clone(), v.caption.clone())).collect()).unwrap_or_default(),
                    _ => fd.options.iter().enumerate().map(|(i, o)| (i as i64, o.trim().to_string(), fd.option_captions.get(i).cloned().unwrap_or_else(|| o.clone()))).collect(),
                };
                let n = a.first().map(|v| v.as_i64()).unwrap_or(0);
                let by_ordinal = lname.ends_with("fromordinalvalue");
                let found = if by_ordinal { values.iter().find(|v| v.0 == n) } else { values.get((n - 1).max(0) as usize).filter(|_| n >= 1) };
                let Some((ord, name, caption)) = found else {
                    return err(format!("The value {n} is out of range for the enum of field {}", fd.caption));
                };
                if lname == "getenumvalueordinal" {
                    Val::Int(*ord as i32)
                } else if lname.contains("caption") {
                    Val::Text(caption.clone())
                } else {
                    Val::Text(name.clone())
                }
            }
            "fielderror" => {
                // as Record.FieldError, with FieldError's error code [src:A11a]
                let txt = a.first().map(|v| v.to_text()).unwrap_or_default();
                let msg = self.field_error_text(&h.borrow(), fi, &txt);
                return err_code("NCLCSRTS:TableErrorStr", msg);
            }
            _ => return self.miss_fieldref(lname),
        })
    }

    fn miss_fieldref(&mut self, n: &str) -> R<Val> {
        *self.missing.entry(format!("FieldRef.{n}")).or_default() += 1;
        err(format!("Not implemented in bc-rust: FieldRef.{n}"))
    }

    /// Clear(Rec), and ClearAll for each Record variable (NavRecord.Clear): back to the current
    /// company, a fresh initialized row, no filters, marks or key; a temporary table keeps its rows
    /// (Purch.-Post relies on that: ClearAll keeps TempWhseRcptHeader).
    pub fn clear_record(&mut self, h: &RecHandle) {
        let mut rr = h.borrow_mut();
        let fresh = rr.schema.new_row();
        rr.row = (fresh).into();
        self.init_row(rr.tdef, &mut rr.row);
        rr.filters.clear();
        rr.filter_group = 0;
        rr.key = 0;
        rr.custom_key = None;
        rr.ascending = true;
        rr.marks.clear();
        rr.mark_only = false;
        rr.company = None;
        // BC NavRecord.Clear -> ClearAll: the table's global variables start over (a global
        // codeunit gets a new instance); they are created again on first use
        rr.globals = None;
        rr.ext_globals.clear();
    }

    /// The text of FieldError(Field[, Text]) [src:A10.2].
    fn field_error_text(&self, r: &RecState, fi: usize, txt: &str) -> String {
        let fd = r.field(fi);
        if !txt.is_empty() {
            return format!("{} {} in {} {}.", fd.caption, txt, r.tdef.caption, self.key_text(r));
        }
        let shown = self.format_field_val(fd, &r.get_val(fi));
        // text, code, date and time types with no value: "You must specify" [src:A10.2]; Time
        // and DateFormula are an open question [src:A10.5]
        let empty_kind = matches!(
            fd.ty,
            FieldType::Code(_) | FieldType::Text(_) | FieldType::Date | FieldType::Time | FieldType::DateTime | FieldType::DateFormula
        );
        if empty_kind && shown.is_empty() {
            return format!("You must specify {} in {} {}.", fd.caption, r.tdef.caption, self.key_text(r));
        }
        format!("{} must not be {} in {} {}.", fd.caption, shown, r.tdef.caption, self.key_text(r))
    }

    /// GetFilters: the filters of the current filter group [src:A06.4].
    pub fn filters_text(&self, r: &RecState) -> String {
        r.filters
            .iter()
            .filter(|x| x.group == r.filter_group && !x.text.is_empty())
            .map(|x| format!("{}: {}", r.tdef.fields[x.field].caption, self.option_filter_text(&r.tdef.fields[x.field], &x.text, false)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Option values of an Option or Enum field: (ordinal, name, caption).
    fn option_values(&self, fd: &al_model::FieldDef) -> Vec<(i64, String, String)> {
        match &fd.ty {
            FieldType::Enum(en) => self
                .cat
                .enum_by_name(en)
                .map(|e| e.values.iter().map(|v| (v.ordinal, v.name.clone(), if v.caption.is_empty() { v.name.clone() } else { v.caption.clone() })).collect())
                .unwrap_or_default(),
            FieldType::Option => fd
                .options
                .iter()
                .enumerate()
                .map(|(i, n)| (i as i64, n.clone(), fd.option_captions.get(i).filter(|c| !c.is_empty()).cloned().unwrap_or_else(|| n.clone())))
                .collect(),
            _ => vec![],
        }
    }

    /// A filter expression of an Option/Enum field with its values as ordinals (`ordinals`) or as
    /// captions; other fields' expressions are returned unchanged.
    fn option_filter_text(&self, fd: &al_model::FieldDef, text: &str, ordinals: bool) -> String {
        let vals = self.option_values(fd);
        if vals.is_empty() {
            return text.to_string();
        }
        let map = |tok: &str| -> Option<String> {
            let t = tok.trim().trim_matches('\'');
            if t.is_empty() {
                return None;
            }
            let v = match t.parse::<i64>() {
                Ok(n) => vals.iter().find(|v| v.0 == n),
                Err(_) => vals.iter().find(|v| crate::filter::text_eq(&v.1, t) || crate::filter::text_eq(&v.2, t)),
            }?;
            // a blank option is written '' (BC quotes a blank value)
            Some(if ordinals {
                v.0.to_string()
            } else if v.2.trim().is_empty() {
                "''".to_string()
            } else {
                quote_filter_value(&v.2)
            })
        };
        // map the operands between the filter operators | & .. < > = ( )
        let mut out = String::new();
        let mut tok = String::new();
        let flush = |tok: &mut String, out: &mut String| {
            if !tok.is_empty() {
                let lead = tok.len() - tok.trim_start().len();
                let trail = tok.len() - tok.trim_end().len();
                match map(tok) {
                    Some(m) => {
                        out.push_str(&tok[..lead]);
                        out.push_str(&m);
                        out.push_str(&tok[tok.len() - trail..]);
                    }
                    None => out.push_str(tok),
                }
                tok.clear();
            }
        };
        let cs: Vec<char> = text.chars().collect();
        let mut i = 0;
        // a quoted value ('Mat. & Requisition') is one operand
        let mut in_quote = false;
        while i < cs.len() {
            let c = cs[i];
            if c == '\'' {
                in_quote = !in_quote;
                tok.push(c);
            } else if !in_quote && (matches!(c, '|' | '&' | '<' | '>' | '=' | '(' | ')') || (c == '.' && cs.get(i + 1) == Some(&'.'))) {
                flush(&mut tok, &mut out);
                out.push(c);
                if c == '.' {
                    out.push('.');
                    i += 1;
                }
            } else {
                tok.push(c);
            }
            i += 1;
        }
        flush(&mut tok, &mut out);
        out
    }

    /// GetView text: `VERSION(1) SORTING(..)[ ORDER(..)][ WHERE(..)]`. With captions (GetView() /
    /// GetView(true)) fields are written by caption and the filter type as FILTER; without
    /// (GetView(false)) fields as `Field<no>`, the filter type as 1 and option values as ordinals
    /// [src:A06.1]. Which filter groups it writes and the quoting of a caption that holds the
    /// separator are open questions [src:A06.10].
    pub fn view_text(&self, r: &RecState, use_captions: bool) -> String {
        let name = |fi: usize, sep: char| -> String {
            let fd = &r.tdef.fields[fi];
            if !use_captions {
                format!("Field{}", fd.id)
            } else if fd.caption.contains(sep) {
                format!("\"{}\"", fd.caption.replace('"', "\"\""))
            } else {
                fd.caption.clone()
            }
        };
        let sorting = r.custom_key.as_ref().unwrap_or(&r.schema.keys[r.key]).iter().map(|&i| name(i, ',')).collect::<Vec<_>>().join(",");
        let mut v = format!("VERSION(1) SORTING({sorting})");
        if !r.ascending {
            v.push_str(if use_captions { " ORDER(Descending)" } else { " ORDER(1)" });
        }
        let wh: Vec<String> = r
            .filters
            .iter()
            .filter(|x| x.group == r.filter_group && !x.text.is_empty())
            .map(|x| {
                let fd = &r.tdef.fields[x.field];
                // GetView writes values culture-invariant [src:A06.2]; GetFilters keeps the
                // session's format
                let text = invariant_filter_text(fd, &x.node, use_captions).unwrap_or_else(|| self.option_filter_text(fd, &x.text, !use_captions));
                format!("{}={}({})", name(x.field, '='), if use_captions { "FILTER" } else { "1" }, text)
            })
            .collect();
        if !wh.is_empty() {
            v.push_str(&format!(" WHERE({})", wh.join(",")));
        }
        v
    }

    /// SetView: the key from SORTING (else the primary key), the order from ORDER, and the
    /// filters of WHERE replacing those of the current filter group [src:A06.3] (FILTERGROUP(n: ...)
    /// sections replace group n's: open question [src:A06.10]). Reads both GetView forms
    /// (captions/names, quoted or not, or `Field<no>`; FILTER/CONST or 1/0).
    pub fn set_view(&mut self, h: &RecHandle, view: &str) -> R<()> {
        let tdef = h.borrow().tdef;
        let field = |n: &str| -> Option<usize> {
            let n = n.trim();
            if let Some(q) = n.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
                let q = q.replace("\"\"", "\"");
                return tdef.field(&q).or_else(|| tdef.fields.iter().position(|f| crate::filter::text_eq(&f.caption, &q)));
            }
            if let Some(no) = n.strip_prefix("Field").and_then(|x| x.parse::<i64>().ok()) {
                if let Some(i) = tdef.fields.iter().position(|f| f.id == no) {
                    return Some(i);
                }
            }
            tdef.field(n).or_else(|| tdef.fields.iter().position(|f| crate::filter::text_eq(&f.caption, n)))
        };
        let sections = view_sections(view);
        let get = |k: &str| sections.iter().find(|(name, _)| name.eq_ignore_ascii_case(k)).map(|(_, b)| b.as_str());
        // key
        let key_fields: Vec<usize> = get("SORTING").map(|b| split_view_conds(b).iter().filter_map(|n| field(n)).collect()).unwrap_or_default();
        {
            let mut r = h.borrow_mut();
            r.key = 0;
            r.custom_key = None;
            r.set_sort_fields(&key_fields);
            r.ascending = !get("ORDER").and_then(|b| split_view_conds(b).into_iter().next()).is_some_and(|o| {
                let o = o.trim();
                o.eq_ignore_ascii_case("descending") || o == "1"
            });
        }
        // filters: WHERE for the current group, FILTERGROUP(n: ...) for group n
        let current = h.borrow().filter_group;
        let mut groups: Vec<(i32, String)> = vec![(current, get("WHERE").unwrap_or("").to_string())];
        for (name, body) in &sections {
            if name.eq_ignore_ascii_case("FILTERGROUP") {
                if let Some((g, conds)) = body.split_once(':') {
                    if let Ok(g) = g.trim().parse::<i32>() {
                        groups.push((g, conds.to_string()));
                    }
                }
            }
        }
        for (g, body) in groups {
            h.borrow_mut().filters.retain(|x| x.group != g);
            for part in split_view_conds(&body) {
                // Name=FILTER(value) / Name=CONST(value) / Field<no>=1(value)
                let Some(eq) = top_level_eq(&part) else { continue };
                let (fname, rest) = (&part[..eq], part[eq + 1..].trim());
                let val = match (rest.find('('), rest.ends_with(')')) {
                    (Some(i), true) => &rest[i + 1..rest.len() - 1],
                    _ => rest,
                };
                let Some(fi) = field(fname) else { continue };
                let fd = h.borrow().field(fi);
                let node = self.parse_field_filter(fd, val)?;
                let mut r = h.borrow_mut();
                let saved = r.filter_group;
                r.filter_group = g;
                r.set_filter(fi, Some(node), val.to_string());
                r.filter_group = saved;
            }
        }
        Ok(())
    }

    /// Whether an expression has the static type Code: a Code variable/parameter, a Code field of
    /// a record variable or of the implicit Rec. Code values that consist of digits only compare
    /// right-aligned (as numbers): '9' < '10', '109001' < '1010999'.
    pub fn expr_is_code(&self, f: &Frame, e: &Expr) -> bool {
        fn is_code_ty(t: &al_syntax::TypeRef) -> bool {
            matches!(t, al_syntax::TypeRef::Simple { name, .. } if name.eq_ignore_ascii_case("code"))
        }
        let var = |n: &str| f.locals.iter().rev().find(|v| v.name.eq_ignore_ascii_case(n)).or_else(|| f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n)));
        let field_of = |tdef: &al_model::TableDef, n: &str| tdef.field(n).is_some_and(|i| matches!(tdef.fields[i].ty, FieldType::Code(_)));
        match e {
            Expr::Ident(n) => match var(n) {
                Some(v) => v.ty.is_some_and(is_code_ty),
                None => f.rec.as_ref().is_some_and(|r| field_of(r.borrow().tdef, n)),
            },
            Expr::Member(b, n) => match &**b {
                Expr::Ident(rv) => match var(rv).and_then(|v| v.ty) {
                    Some(al_syntax::TypeRef::Object { kind, target, .. }) if kind.eq_ignore_ascii_case("record") => self.cat.table_by_name(target).is_some_and(|t| field_of(t, n)),
                    _ => false,
                },
                _ => false,
            },
            _ => false,
        }
    }

    /// The Option/Enum field named by `Rec."Field"::Value` or `"Field"::Value` (implicit Rec), so
    /// Format shows the option's caption as it does for the field itself.
    pub fn option_field_of_scope(&mut self, f: &mut Frame, e: &Expr) -> R<Option<&'static al_model::FieldDef>> {
        let Expr::Scope(base, _) = e else { return Ok(None) };
        let fd = match &**base {
            Expr::Member(rb, fname) => {
                if !matches!(&**rb, Expr::Ident(b) if !crate::builtins::is_static_class(&b.to_ascii_lowercase())) {
                    return Ok(None);
                }
                match self.eval(f, rb)? {
                    Val::Record(r) => {
                        let fi = r.borrow().tdef.field(fname);
                        fi.map(|i| r.borrow().field(i))
                    }
                    _ => None,
                }
            }
            Expr::Ident(n) => {
                if f.locals.iter().chain(f.globals.vars.iter()).any(|v| v.name.eq_ignore_ascii_case(n)) {
                    return Ok(None);
                }
                f.rec.as_ref().and_then(|r| {
                    let fi = r.borrow().tdef.field(n);
                    fi.map(|i| r.borrow().field(i))
                })
            }
            _ => None,
        };
        Ok(fd.filter(|fd| matches!(fd.ty, FieldType::Option | FieldType::Enum(_))))
    }

    pub fn format_field_val(&self, fd: &al_model::FieldDef, v: &Val) -> String {
        match (&fd.ty, v) {
            (FieldType::Enum(en), Val::Opt { ord, .. }) => {
                self.cat.enum_by_name(en).and_then(|e| e.by_ordinal(*ord as i64)).map(|x| x.caption.clone()).unwrap_or_else(|| ord.to_string())
            }
            (FieldType::Option, Val::Opt { ord, .. }) => fd.option_captions.get(*ord as usize).cloned().unwrap_or_else(|| ord.to_string()),
            _ => v.to_text(),
        }
    }
}

/// Split `A=FILTER(x),B=CONST(y)` at top-level commas.
/// The sections of a table view: `VERSION(1) SORTING(..) ORDER(..) WHERE(..) FILTERGROUP(n: ..)`
/// as (keyword, body) pairs. Parentheses nest; double-quoted names may contain any character.
fn view_sections(view: &str) -> Vec<(String, String)> {
    let cs: Vec<char> = view.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < cs.len() && cs[i].is_ascii_alphabetic() {
            i += 1;
        }
        let name: String = cs[start..i].iter().collect();
        while i < cs.len() && cs[i] == ' ' {
            i += 1;
        }
        if cs.get(i) != Some(&'(') {
            continue;
        }
        i += 1;
        let (mut depth, mut quoted, body_start) = (1, false, i);
        while i < cs.len() {
            match cs[i] {
                '"' => quoted = !quoted,
                '(' if !quoted => depth += 1,
                ')' if !quoted => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        out.push((name, cs[body_start..i.min(cs.len())].iter().collect()));
        i += 1;
    }
    out
}

/// Position of the `=` between a view condition's field and its filter (outside double quotes).
fn top_level_eq(part: &str) -> Option<usize> {
    let mut quoted = false;
    for (i, c) in part.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '=' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

fn split_view_conds(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut depth = 0;
    let mut quoted = false;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            _ if quoted => {}
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// BC's optimistic concurrency on Modify / Delete / Rename (`Session::rec_stored_for_write`): on
/// by default; BC_STALE_CHECK=0 turns it off (I14 S1, 2026-11-09).
pub fn stale_check() -> bool {
    match STALE_CHECK.load(std::sync::atomic::Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *ENV.get_or_init(|| !std::env::var("BC_STALE_CHECK").is_ok_and(|v| v.trim() == "0"))
        }
    }
}

/// (table, primary key) of a row in a session's own-writes log.
type OwnKey = (i64, Box<[u8]>);

/// BC's per-session timestamp log: a fingerprint of the row the session last wrote, by (table,
/// primary key), with the oldest writes forgotten first past `OWN_WRITES_MAX`. Never emptied at
/// once while the session lives: its own earlier write would then look like another user's change
/// ("Another user has modified the record ..."). It ends with the session (`forget_own_writes`).
#[derive(Default)]
struct OwnLog {
    /// key -> (write number, the last `OWN_HISTORY` writes oldest first: (fingerprint of the row
    /// as stored, fingerprint of the version it replaced; None for an Insert))
    rows: std::collections::HashMap<OwnKey, (u64, Vec<(u64, Option<u64>)>)>,
    /// write order, oldest first; entries a later write of the same key replaced are skipped
    order: std::collections::VecDeque<(OwnKey, u64)>,
    n: u64,
}

/// The own-writes logs of the sessions living on this thread (a Session never changes thread).
#[derive(Default)]
struct OwnWrites {
    sessions: std::collections::HashMap<u64, OwnLog>,
}

/// A session ends (Session's Drop): its own-writes log goes with it. Before, the logs of ended
/// sessions stayed until a per-thread cap, and a server's RSS grew by hundreds of MB per hour.
pub(crate) fn forget_own_writes(sid: u64) {
    let _ = OWN_WRITES.try_with(|m| {
        if let Ok(mut m) = m.try_borrow_mut() {
            m.sessions.remove(&sid);
        }
    });
}

thread_local! {
    static OWN_WRITES: std::cell::RefCell<OwnWrites> = std::cell::RefCell::new(Default::default());
}
const OWN_WRITES_MAX: usize = 32_768;
/// Own writes remembered per row (a record modified again and again keeps its recent chain).
const OWN_HISTORY: usize = 16;

/// A fingerprint of a row as the database keeps it: DateTimes rounded (`round_datetimes`),
/// Decimals by value, everything else exactly (what `Value`'s equality compares).
fn row_print(schema: &bc_store::TableSchema, row: &[Value]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (i, v) in row.iter().enumerate() {
        match v {
            Value::Int(x) => (0u8, x).hash(&mut h),
            Value::BigInt(x) => (1u8, x).hash(&mut h),
            Value::Dec(d) => (2u8, d.normalize().serialize()).hash(&mut h),
            Value::Bool(x) => (3u8, x).hash(&mut h),
            Value::Text(x) => (4u8, x).hash(&mut h),
            Value::Date(x) => (5u8, x).hash(&mut h),
            Value::Time(x) => (6u8, x).hash(&mut h),
            Value::DateTime(ms) => {
                let ms = if matches!(schema.columns.get(i), Some(bc_store::ColType::DateTime)) { bc_store::value::sql_datetime_ms(*ms) } else { *ms };
                (7u8, ms).hash(&mut h)
            }
            Value::Duration(x) => (8u8, x).hash(&mut h),
            Value::Guid(x) => (9u8, x).hash(&mut h),
            Value::Option(x) => (10u8, x).hash(&mut h),
            Value::Blob(x) => (11u8, x).hash(&mut h),
        }
    }
    h.finish()
}

/// Remember that session `sid` wrote `row` (Insert, Modify) over the version `pre` (a fingerprint;
/// None for an Insert).
pub(crate) fn note_own_write(sid: u64, schema: &bc_store::TableSchema, row: &crate::record::CowRow, pre: Option<u64>) {
    if !stale_check() {
        return;
    }
    let key = (schema.id, schema.pk(row));
    let print = row_print(schema, row);
    OWN_WRITES.with(|all| {
        let mut all = all.borrow_mut();
        let m = all.sessions.entry(sid).or_default();
        m.n += 1;
        let n = m.n;
        let e = m.rows.entry(key.clone()).or_insert_with(|| (n, Vec::new()));
        e.0 = n;
        if e.1.len() >= OWN_HISTORY {
            e.1.remove(0);
        }
        e.1.push((print, pre));
        m.order.push_back((key, n));
        if m.order.len() > 2 * OWN_WRITES_MAX {
            // drop the order entries of keys written again since
            let OwnLog { rows, order, .. } = &mut *m;
            order.retain(|(k, n)| rows.get(k).is_some_and(|e| e.0 == *n));
        }
        while m.rows.len() > OWN_WRITES_MAX {
            let Some((k, n)) = m.order.pop_front() else { break };
            if m.rows.get(&k).is_some_and(|e| e.0 == n) {
                m.rows.remove(&k);
            }
        }
    });
}

/// Whether the stored row `stored` is a version session `sid` itself wrote (its last write, or an
/// earlier one that is current again because the later ones were rolled back).
fn own_write(sid: u64, schema: &bc_store::TableSchema, stored: &Row) -> bool {
    OWN_WRITES.with(|m| {
        let print = row_print(schema, stored);
        m.borrow().sessions.get(&sid).and_then(|l| l.rows.get(&(schema.id, schema.pk(stored)))).is_some_and(|w| w.1.iter().any(|l| l.0 == print))
    })
}

/// Whether the stored row `stored` is OLDER than a record's version `version`: session `sid` wrote
/// `version` itself, directly or through a chain of its own writes, over `stored`, and those writes
/// were rolled back since (an error, asserterror, a rollback to a savepoint). BC raises the
/// stale-record error only for a newer stored version (Ncl HandleStaleRecordAsync); another
/// session's change after the rollback is a version none of these writes replaced, so it still is.
fn rolled_back_past(sid: u64, schema: &bc_store::TableSchema, version: &Row, stored: &Row) -> bool {
    OWN_WRITES.with(|m| {
        let m = m.borrow();
        let Some((_, hist)) = m.sessions.get(&sid).and_then(|l| l.rows.get(&(schema.id, schema.pk(version)))) else { return false };
        let target = row_print(schema, stored);
        let mut v = row_print(schema, version);
        for &(written, pre) in hist.iter().rev() {
            if written == v {
                match pre {
                    Some(p) if p == target => return true,
                    Some(p) => v = p,
                    None => return false,
                }
            }
        }
        false
    })
}

#[cfg(test)]
mod own_writes_tests {
    use super::*;
    use bc_store::{ColType, TableSchema};

    #[test]
    fn oldest_own_writes_go_first() {
        set_stale_check(true);
        let schema = TableSchema { id: 990_014, name: "OW".into(), columns: vec![ColType::Int, ColType::Int, ColType::DateTime], field_ids: vec![1, 2, 3], keys: vec![vec![0]], per_company: true };
        let row = |k: i32, v: i32| -> crate::record::CowRow { vec![Value::Int(k), Value::Int(v), Value::DateTime(1_000_001)].into() };
        // the first write, then more than the log keeps: only the oldest are forgotten
        note_own_write(77, &schema, &row(0, 1), None);
        for k in 1..=(OWN_WRITES_MAX as i32 + 10) {
            note_own_write(77, &schema, &row(k, 1), None);
        }
        let mut stored = row(OWN_WRITES_MAX as i32, 1).to_vec();
        schema.round_datetimes(&mut stored);
        assert!(own_write(77, &schema, &stored), "a recent own write is remembered (DateTimes as stored)");
        assert!(!own_write(77, &schema, &row(0, 1).to_vec()), "the oldest write is forgotten");
        assert!(!own_write(78, &schema, &stored), "another session's write is not ours");
        assert!(!own_write(77, &schema, &row(OWN_WRITES_MAX as i32, 2).to_vec()), "a different row is not ours");
        // a key written again and again stays remembered while others come and go
        for i in 0..(3 * OWN_WRITES_MAX as i32) {
            note_own_write(77, &schema, &row(-1, i), None);
            note_own_write(77, &schema, &row(i % 100 + 1_000_000, 0), None);
        }
        assert!(own_write(77, &schema, &row(-1, 3 * OWN_WRITES_MAX as i32 - 1).to_vec()));
        OWN_WRITES.with(|m| {
            let m = m.borrow();
            let l = &m.sessions[&77];
            assert!(l.rows.len() <= OWN_WRITES_MAX && l.order.len() <= 2 * OWN_WRITES_MAX + 1);
        });
        // the log ends with its session
        assert!(own_write(77, &schema, &row(-1, 3 * OWN_WRITES_MAX as i32 - 1).to_vec()));
        forget_own_writes(77);
        assert!(!own_write(77, &schema, &row(-1, 3 * OWN_WRITES_MAX as i32 - 1).to_vec()));
        OWN_WRITES.with(|m| assert!(!m.borrow().sessions.contains_key(&77)));
    }

    #[test]
    fn rolled_back_own_writes_make_the_stored_row_older() {
        set_stale_check(true);
        let schema = TableSchema { id: 990_017, name: "OW2".into(), columns: vec![ColType::Int, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true };
        let row = |v: i32| -> crate::record::CowRow { vec![Value::Int(5), Value::Int(v)].into() };
        let p = |v: i32| row_print(&schema, &row(v));
        // v1 (committed elsewhere) -> v2 -> v3, both written by session 91
        note_own_write(91, &schema, &row(2), Some(p(1)));
        note_own_write(91, &schema, &row(3), Some(p(2)));
        let (v1, v2, v3, v9) = (row(1).to_vec(), row(2).to_vec(), row(3).to_vec(), row(9).to_vec());
        assert!(rolled_back_past(91, &schema, &v3, &v1), "both writes rolled back: v1 is older than v3");
        assert!(rolled_back_past(91, &schema, &v3, &v2), "the second write rolled back");
        assert!(rolled_back_past(91, &schema, &v2, &v1));
        assert!(!rolled_back_past(91, &schema, &v3, &v9), "another session's version is newer");
        assert!(!rolled_back_past(92, &schema, &v3, &v1), "only the session's own writes count");
        assert!(!rolled_back_past(91, &schema, &v1, &v3), "a version the session didn't write");
        assert!(own_write(91, &schema, &v2), "an earlier own write, current again after a rollback");
        assert!(own_write(91, &schema, &v3) && !own_write(91, &schema, &v9) && !own_write(92, &schema, &v2));
        // an Insert has no older version
        let ins = vec![Value::Int(6), Value::Int(1)].into();
        note_own_write(91, &schema, &ins, None);
        assert!(!rolled_back_past(91, &schema, &ins.to_vec(), &vec![Value::Int(6), Value::Int(0)]));
    }
}

/// 0: from BC_STALE_CHECK, 1: off, 2: on (`set_stale_check`).
static STALE_CHECK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Turn BC's optimistic concurrency check on or off in this process (tests, A/B runs).
pub fn set_stale_check(on: bool) {
    STALE_CHECK.store(if on { 2 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
}

/// AutoIncrement as SQL Server IDENTITY (see `Session::identity_next`); al-test turns it on.
pub static IDENTITY_AUTOINCREMENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_identity_autoincrement(on: bool) {
    IDENTITY_AUTOINCREMENT.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// A Date, Decimal, Integer or BigInteger filter written culture-invariant, as GetView writes it:
/// dates yyyy-MM-dd (closing dates with a C), numbers without grouping and with a '.', Booleans as
/// 0/1 when the view has no captions [src:A06.2]. None: keep the stored text (text, code, option
/// and the other types).
fn invariant_filter_text(fd: &al_model::FieldDef, node: &bc_store::filter::FNode, use_captions: bool) -> Option<String> {
    use bc_store::filter::FNode as N;
    match fd.ty {
        FieldType::Date | FieldType::Decimal | FieldType::Integer | FieldType::BigInteger => {}
        FieldType::Boolean if !use_captions => {}
        // text and code: the value printed from the parsed expression, not the text the filter
        // was set with
        FieldType::Text(_) | FieldType::Code(_) => {}
        _ => return None,
    }
    fn val(v: &Value) -> Option<String> {
        Some(match v {
            Value::Date(0) => "''".to_string(),
            Value::Date(d) => {
                let n = crate::value::date_to_naive(*d)?;
                format!("{}{}", if crate::value::date_is_closing(*d) { "C" } else { "" }, n.format("%Y-%m-%d"))
            }
            Value::Dec(x) => x.normalize().to_string(),
            Value::Int(i) => i.to_string(),
            Value::BigInt(i) => i.to_string(),
            Value::Bool(b) => (*b as i32).to_string(),
            Value::Text(s) => quote_text_filter_value(s),
            _ => return None,
        })
    }
    // '=' only before a value that reads as a pattern: it starts with '@' or holds * or ?
    // [src:A07.4]
    fn eq_text(s: String) -> String {
        if s.starts_with('@') || s.contains(['*', '?']) {
            format!("={s}")
        } else {
            s
        }
    }
    fn render(n: &N, inner: bool) -> Option<String> {
        let side = |v: &Option<Value>| -> Option<String> {
            match v {
                Some(v) => val(v),
                None => Some(String::new()),
            }
        };
        Some(match n {
            N::Eq(v @ Value::Text(_)) => eq_text(val(v)?),
            N::Eq(v) => val(v)?,
            N::Ne(v) => format!("<>{}", val(v)?),
            N::Lt(v) => format!("<{}", val(v)?),
            N::Le(v) => format!("<={}", val(v)?),
            N::Gt(v) => format!(">{}", val(v)?),
            N::Ge(v) => format!(">={}", val(v)?),
            N::Range(a, b) => format!("{}..{}", side(a)?, side(b)?),
            N::Or(l) => {
                let s = l.iter().map(|x| render(x, false)).collect::<Option<Vec<_>>>()?.join("|");
                if inner {
                    format!("({s})")
                } else {
                    s
                }
            }
            N::And(l) => l.iter().map(|x| render(x, true)).collect::<Option<Vec<_>>>()?.join("&"),
            _ => return None,
        })
    }
    render(node, false)
}

/// The text kept for a filter: the parsed filter printed again, so a plain equality has no '='
/// and the operator stays before a pattern value, one with '*' or '?' or starting with '@'
/// [src:A07.4].
fn filter_text_eq(node: &FNode, text: String) -> String {
    if let (FNode::Eq(_), Some(rest)) = (node, text.strip_prefix('=')) {
        let rest = rest.trim_start();
        let quoted = rest.starts_with('\'');
        let wildcard = rest.starts_with('@') || (!quoted && rest.contains(['*', '?']));
        if !rest.is_empty() && !rest.starts_with(['=', '<', '>']) && !wildcard {
            return rest.to_string();
        }
    }
    text
}
