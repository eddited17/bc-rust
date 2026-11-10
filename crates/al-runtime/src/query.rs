//! Query objects: SetFilter/SetRange on columns and filters, Open (data items joined through
//! DataItemLink with inner/left outer joins, DataItemTableFilter, ColumnFilter, Method aggregates,
//! OrderBy, TopNumberOfRows), Read and column access.

use crate::filter::*;
use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_model::{CondKind, QueryDef};
use al_syntax::Expr;
use bc_store::{Row, Value};
use rust_decimal::prelude::*;
use std::rc::Rc;

#[derive(Default)]
pub struct QueryState {
    /// Filters set with SetFilter/SetRange: (column or filter name, filter text, node).
    pub filters: Vec<(String, String, Option<FNode>)>,
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Val>>,
    /// 0 = before the first row; i = row i-1 is current.
    pub pos: usize,
    pub top: Option<usize>,
}

/// (data item index, field index) of a column or filter element by name.
fn element_field(cat: &al_model::Catalog, q: &QueryDef, name: &str) -> Option<(usize, usize, Option<String>)> {
    for (di, d) in q.dataitems.iter().enumerate() {
        let t = cat.table_by_name(&d.table)?;
        for c in &d.columns {
            if c.name.eq_ignore_ascii_case(name) {
                return Some((di, t.field(&c.field)?, c.method.clone()));
            }
        }
        for fl in &d.filters {
            if fl.name.eq_ignore_ascii_case(name) {
                return Some((di, t.field(&fl.field)?, None));
            }
        }
    }
    None
}

impl<'r> Session<'r> {
    fn query_def(&self, inst: &Rc<Instance>) -> Option<&'static QueryDef> {
        self.cat.queries.get(&self.code(inst.code).id)
    }

    /// The table field behind column (or filter) `col` of the query instance `inst`.
    pub(crate) fn query_column_field(&self, inst: &Rc<Instance>, col: &str) -> Option<&'static al_model::FieldDef> {
        if self.code(inst.code).kind != al_syntax::ObjectKind::Query {
            return None;
        }
        let q = self.query_def(inst)?;
        let (di, fi, _) = element_field(self.cat, q, col)?;
        Some(&self.cat.table_by_name(&q.dataitems[di].table)?.fields[fi])
    }

    pub fn query_method(&mut self, f: &mut Frame, inst: &Rc<Instance>, name: &str, args: &[Expr]) -> R<Val> {
        let Some(q) = self.query_def(inst) else { return err(format!("Query {} is not defined", self.code(inst.code).name)) };
        let m = name.to_ascii_lowercase();
        if inst.query.borrow().is_none() {
            *inst.query.borrow_mut() = Some(Box::default());
        }
        match m.as_str() {
            // ColumnCaption(Column): the column's Caption, else its field's caption
            "columncaption" => {
                let col = match args.first() {
                    Some(Expr::Ident(n)) | Some(Expr::Member(_, n)) => n.to_string(),
                    _ => return err("ColumnCaption requires a query column"),
                };
                let own = q.dataitems.iter().flat_map(|d| d.columns.iter()).find(|c| c.name.eq_ignore_ascii_case(&col)).and_then(|c| c.caption.clone());
                if let Some(cap) = own {
                    return Ok(Val::Text(cap));
                }
                let Some((di, fi, _)) = element_field(self.cat, q, &col) else { return err(format!("Column {col} does not exist in query {}", q.name)) };
                let tdef = self.cat.table_by_name(&q.dataitems[di].table).unwrap();
                Ok(Val::Text(tdef.fields[fi].caption.clone()))
            }
            "setfilter" | "setrange" => {
                let col = match args.first() {
                    Some(Expr::Ident(n)) | Some(Expr::Member(_, n)) => n.to_string(),
                    _ => return err(format!("{name} requires a query column")),
                };
                let Some((di, fi, _)) = element_field(self.cat, q, &col) else { return err(format!("Column {col} does not exist in query {}", q.name)) };
                let tdef = self.cat.table_by_name(&q.dataitems[di].table).unwrap();
                let fd = &tdef.fields[fi];
                let mut vals = vec![];
                for a in &args[1..] {
                    vals.push(self.eval(f, a)?);
                }
                let (text, node) = if m == "setrange" {
                    match vals.len() {
                        0 => (String::new(), None),
                        1 => {
                            let v = to_store(fd, &vals[0]).map_err(|e| Flow::Err(Box::new(AlError::new(e))))?;
                            (vals[0].to_text(), Some(FNode::Eq(v)))
                        }
                        _ => {
                            let a = to_store(fd, &vals[0]).map_err(|e| Flow::Err(Box::new(AlError::new(e))))?;
                            let b = to_store(fd, &vals[1]).map_err(|e| Flow::Err(Box::new(AlError::new(e))))?;
                            (format!("{}..{}", vals[0].to_text(), vals[1].to_text()), Some(FNode::Range(Some(a), Some(b))))
                        }
                    }
                } else {
                    let mut text = vals.first().map(|v| v.to_text()).unwrap_or_default();
                    for (i, v) in vals.iter().enumerate().skip(1) {
                        text = text.replace(&format!("%{i}"), &v.to_text());
                    }
                    let node = if text.is_empty() { None } else { Some(self.parse_field_filter(fd, &text)?) };
                    (text, node)
                };
                let mut st = inst.query.borrow_mut();
                let st = st.as_mut().unwrap();
                st.filters.retain(|(c, _, _)| !c.eq_ignore_ascii_case(&col));
                if node.is_some() {
                    st.filters.push((col, text, node));
                }
                Ok(Val::Void)
            }
            "getfilter" => {
                let col = match args.first() {
                    Some(Expr::Ident(n)) | Some(Expr::Member(_, n)) => n.to_string(),
                    _ => return Ok(Val::Text(String::new())),
                };
                let st = inst.query.borrow();
                Ok(Val::Text(st.as_ref().and_then(|s| s.filters.iter().find(|(c, _, _)| c.eq_ignore_ascii_case(&col)).map(|x| x.1.clone())).unwrap_or_default()))
            }
            "getfilters" => {
                let st = inst.query.borrow();
                Ok(Val::Text(st.as_ref().map(|s| s.filters.iter().map(|(c, t, _)| format!("{c}: {t}")).collect::<Vec<_>>().join(", ")).unwrap_or_default()))
            }
            "topnumberofrows" => {
                if let Some(a) = args.first() {
                    let n = self.eval(f, a)?.as_i64().max(0) as usize;
                    inst.query.borrow_mut().as_mut().unwrap().top = Some(n);
                }
                Ok(Val::Int(inst.query.borrow().as_ref().and_then(|s| s.top).unwrap_or(0) as i32))
            }
            "open" => {
                self.query_open(inst, q)?;
                Ok(Val::Bool(true))
            }
            "read" => {
                let mut st = inst.query.borrow_mut();
                let st = st.as_mut().unwrap();
                if st.pos < st.rows.len() {
                    st.pos += 1;
                    Ok(Val::Bool(true))
                } else {
                    Ok(Val::Bool(false))
                }
            }
            "close" => {
                let mut st = inst.query.borrow_mut();
                let st = st.as_mut().unwrap();
                st.rows.clear();
                st.pos = 0;
                Ok(Val::Void)
            }
            "clear" => {
                *inst.query.borrow_mut() = Some(Box::default());
                Ok(Val::Void)
            }
            _ => {
                // column value of the current row
                {
                    let st = inst.query.borrow();
                    let st = st.as_ref().unwrap();
                    if let Some(ci) = st.cols.iter().position(|c| c.eq_ignore_ascii_case(name)) {
                        if st.pos == 0 {
                            return Ok(Val::Void);
                        }
                        return Ok(st.rows[st.pos - 1][ci].clone());
                    }
                }
                if element_field(self.cat, q, name).is_some() {
                    return Ok(Val::Void);
                }
                *self.missing.entry(format!("Query.{name}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: Query.{name}"))
            }
        }
    }

    fn query_open(&mut self, inst: &Rc<Instance>, q: &'static QueryDef) -> R<()> {
        let user_filters = inst.query.borrow().as_ref().map(|s| s.filters.clone()).unwrap_or_default();
        let top_override = inst.query.borrow().as_ref().and_then(|s| s.top);
        // per data item: table, filters on stored fields (DataItemTableFilter, ColumnFilter, SetFilter)
        let mut tables = vec![];
        let mut item_filters: Vec<Vec<FieldFilter>> = vec![];
        // ColumnFilter/SetFilter on a left-outer-joined data item filter the joined result (WHERE):
        // rows without a match don't pass them; DataItemTableFilter still restricts the join
        let mut post_filters: Vec<Vec<FieldFilter>> = vec![];
        // filters on aggregated columns apply to the result (HAVING)
        let mut having: Vec<(String, FNode)> = vec![];
        for d in &q.dataitems {
            let Some(t) = self.cat.table_by_name(&d.table) else { return err(format!("Table {} of query {} does not exist", d.table, q.name)) };
            tables.push(t);
            let post_join = tables.len() > 1 && d.join != "innerjoin";
            let mut pfl = vec![];
            let mut fl = vec![];
            for c in &d.table_filter {
                let Some(fi) = t.field(&c.field) else { continue };
                let fd = &t.fields[fi];
                let node = match c.kind {
                    CondKind::Const => {
                        let ev = |n: &str| self.enum_ordinal_for_field(fd, n);
                        text_to_store(fd, &c.value, &ev, self.work_date).map(FNode::Eq)
                    }
                    CondKind::Filter => Some(self.parse_field_filter(fd, &c.value)?),
                    CondKind::Field => None,
                };
                if let Some(n) = node {
                    fl.push(FieldFilter { field: fi, node: n, text: c.value.clone(), group: 0 });
                }
            }
            for (name, field, filter, method) in d
                .columns
                .iter()
                .map(|c| (&c.name, &c.field, &c.filter, c.method.as_deref()))
                .chain(d.filters.iter().map(|x| (&x.name, &x.field, &x.filter, None)))
            {
                // `column(Count_) { Method = Count; }` has no source field: its filters compare counts
                let count_fd;
                let (fi, fd) = match t.field(field) {
                    Some(fi) => (Some(fi), &t.fields[fi]),
                    None if method.is_some() => {
                        count_fd = al_model::FieldDef::system(0, name, al_model::FieldType::Integer);
                        (None, &count_fd)
                    }
                    None => continue,
                };
                let mut nodes = vec![];
                if let Some(text) = filter {
                    nodes.push(self.parse_field_filter(fd, text)?);
                }
                for (c, _, n) in &user_filters {
                    if c.eq_ignore_ascii_case(name) {
                        if let Some(n) = n {
                            nodes.push(n.clone());
                        }
                    }
                }
                for n in nodes {
                    match (method, fi) {
                        (Some(_), _) => having.push((name.to_ascii_lowercase(), n)),
                        (None, Some(fi)) if post_join && t.fields[fi].class == al_model::FieldClass::Normal => pfl.push(FieldFilter { field: fi, node: n, text: String::new(), group: 0 }),
                        (None, Some(fi)) => fl.push(FieldFilter { field: fi, node: n, text: String::new(), group: 0 }),
                        (None, None) => {}
                    }
                }
            }
            item_filters.push(fl);
            post_filters.push(pfl);
        }
        // join the data items
        let mut tuples: Vec<Vec<Option<Row>>> = vec![];
        for (di, d) in q.dataitems.iter().enumerate() {
            let t = tables[di];
            if di == 0 {
                let r = self.new_record(t.id, false);
                r.borrow_mut().filters = item_filters[0].clone();
                for row in self.fetch_all(&r)? {
                    let mut tu = vec![None; q.dataitems.len()];
                    tu[0] = Some(row);
                    tuples.push(tu);
                }
                continue;
            }
            let mut next = vec![];
            for tu in tuples {
                let r = self.new_record(t.id, false);
                let mut fl = item_filters[di].clone();
                let mut linkable = true;
                for (field, other, ofield) in &d.links {
                    let (Some(fi), Some(oi)) = (t.field(field), q.dataitems.iter().position(|x| x.name.eq_ignore_ascii_case(other))) else { continue };
                    let Some(of) = tables[oi].field(ofield) else { continue };
                    match &tu[oi] {
                        Some(orow) => fl.push(FieldFilter { field: fi, node: FNode::Eq(orow[of].clone()), text: String::new(), group: 0 }),
                        None => linkable = false,
                    }
                }
                r.borrow_mut().filters = fl;
                let rows = if linkable { self.fetch_all(&r)? } else { vec![] };
                if rows.is_empty() {
                    if d.join != "innerjoin" {
                        next.push(tu);
                    }
                    continue;
                }
                for row in rows {
                    let mut t2 = tu.clone();
                    t2[di] = Some(row);
                    next.push(t2);
                }
            }
            tuples = next;
            if !post_filters[di].is_empty() {
                let probe = self.new_record(t.id, false);
                probe.borrow_mut().filters = post_filters[di].clone();
                let pb = probe.borrow();
                // an unmatched row's columns read as their type's default value (BC: the WHERE
                // clause wraps them in ISNULL(column, default)), so a filter that the default
                // satisfies keeps the row
                let blank = pb.schema.new_row();
                tuples.retain(|tu| pb.row_matches(tu[di].as_ref().unwrap_or(&blank)));
            }
        }
        // columns
        let mut cols = vec![];
        let mut col_src = vec![];
        for (di, d) in q.dataitems.iter().enumerate() {
            for c in &d.columns {
                cols.push(c.name.clone());
                col_src.push((di, tables[di].field(&c.field), c.method.clone(), c.reverse_sign));
            }
        }
        let value_of = |tu: &Vec<Option<Row>>, di: usize, fi: Option<usize>| -> Val {
            let Some(fi) = fi else { return Val::Void };
            let fd = &tables[di].fields[fi];
            match &tu[di] {
                Some(row) => to_val(fd, &row[fi]),
                None => to_val(fd, &Value::default_for(col_type(fd))),
            }
        };
        let aggregated = col_src.iter().any(|(_, _, m, _)| m.is_some());
        let mut rows: Vec<Vec<Val>> = vec![];
        if aggregated {
            // group by the plain columns
            let mut groups: Vec<(Vec<Val>, Vec<&Vec<Option<Row>>>)> = vec![];
            for tu in &tuples {
                let key: Vec<Val> = col_src.iter().filter(|(_, _, m, _)| m.is_none()).map(|(di, fi, _, _)| value_of(tu, *di, *fi)).collect();
                match groups.iter_mut().find(|(k, _)| k.len() == key.len() && k.iter().zip(&key).all(|(a, b)| equals(a, b))) {
                    Some(g) => g.1.push(tu),
                    None => groups.push((key, vec![tu])),
                }
            }
            if groups.is_empty() && col_src.iter().all(|(_, _, m, _)| m.is_some()) {
                groups.push((vec![], vec![]));
            }
            for (key, members) in groups {
                let mut k = key.into_iter();
                let mut row = vec![];
                for (di, fi, m, rev) in &col_src {
                    let v = match m.as_deref() {
                        None => k.next().unwrap_or(Val::Void),
                        Some("count") => Val::Int(members.iter().filter(|tu| tu[*di].is_some()).count() as i32),
                        Some(agg) => {
                            let vals: Vec<Val> = members.iter().filter(|tu| tu[*di].is_some()).map(|tu| value_of(tu, *di, *fi)).collect();
                            let v = match agg {
                                "sum" => Val::Dec(vals.iter().map(|v| v.as_dec()).sum()),
                                "average" => {
                                    let n = vals.len().max(1);
                                    Val::Dec(vals.iter().map(|v| v.as_dec()).sum::<Decimal>() / Decimal::from(n))
                                }
                                "min" => vals.iter().cloned().min_by(|a, b| compare(a, b)).unwrap_or(Val::Dec(Decimal::ZERO)),
                                _ => vals.iter().cloned().max_by(|a, b| compare(a, b)).unwrap_or(Val::Dec(Decimal::ZERO)),
                            };
                            match (v, *rev) {
                                (Val::Dec(d), true) => Val::Dec(-d),
                                (v, _) => v,
                            }
                        }
                    };
                    row.push(v);
                }
                rows.push(row);
            }
            // HAVING on aggregated columns
            rows.retain(|row| {
                having.iter().all(|(c, n)| {
                    let Some(ci) = cols.iter().position(|x| x.eq_ignore_ascii_case(c)) else { return true };
                    let sv = match &row[ci] {
                        Val::Int(i) => Value::Int(*i),
                        Val::Dec(d) => Value::Dec(*d),
                        o => Value::Dec(o.as_dec()),
                    };
                    n.matches(&sv)
                })
            });
        } else {
            for tu in &tuples {
                rows.push(col_src.iter().map(|(di, fi, _, _)| value_of(tu, *di, *fi)).collect());
            }
        }
        // OrderBy
        if !q.order_by.is_empty() {
            let keys: Vec<(usize, bool)> = q.order_by.iter().filter_map(|(c, d)| cols.iter().position(|x| x.eq_ignore_ascii_case(c)).map(|i| (i, *d))).collect();
            rows.sort_by(|a, b| {
                for (i, desc) in &keys {
                    let o = compare(&a[*i], &b[*i]);
                    if o != std::cmp::Ordering::Equal {
                        return if *desc { o.reverse() } else { o };
                    }
                }
                std::cmp::Ordering::Equal
            });
        }
        if let Some(n) = top_override.or(q.top.map(|t| t as usize)) {
            if n > 0 {
                rows.truncate(n);
            }
        }
        let mut st = inst.query.borrow_mut();
        let st = st.as_mut().unwrap();
        st.cols = cols;
        st.rows = rows;
        st.pos = 0;
        Ok(())
    }
}
