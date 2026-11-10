//! Record links: `Rec.AddLink(Url [, Description])`, `HasLinks`, `DeleteLinks`, `DeleteLink(Id)`
//! and `CopyLinks(FromRecord)` (also on RecordRefs). As in BC, the links are rows of the system
//! table "Record Link" (2000000068) keyed by "Link ID" (AutoIncrement) and pointing to the
//! record through "Record ID"; AddLink returns the new link's id.

use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_syntax::Expr;
use bc_store::Row;

pub const RECORD_LINK: i64 = 2000000068;

pub fn is_link_method(l: &str) -> bool {
    matches!(l, "addlink" | "haslinks" | "deletelinks" | "deletelink" | "copylinks")
}

fn record_id_of(h: &RecHandle) -> Val {
    let r = h.borrow();
    Val::RecordId(r.tdef.id, r.tdef.pk().fields.iter().map(|&i| r.row[i].clone()).collect())
}

impl<'r> Session<'r> {
    /// A "Record Link" record filtered to the links of record `rid`.
    fn links_of(&mut self, rid: &Val) -> R<Option<RecHandle>> {
        if !self.cat.tables.contains_key(&RECORD_LINK) {
            return Ok(None);
        }
        let rl = self.new_record(RECORD_LINK, false);
        {
            let mut b = rl.borrow_mut();
            if let Some(fi) = b.tdef.field("Record ID") {
                let fd = b.field(fi);
                let v = to_store(fd, rid).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                b.set_filter(fi, Some(crate::filter::FNode::Eq(v)), rid.to_text());
            }
        }
        Ok(Some(rl))
    }

    fn set_link_field(rl: &RecHandle, name: &str, v: Val) -> R<()> {
        let mut b = rl.borrow_mut();
        if let Some(fi) = b.tdef.field(name) {
            let sv = to_store(b.field(fi), &v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
            b.row[fi] = sv;
        }
        Ok(())
    }

    /// Insert a link row for `rid`: copies `from` (a Record Link row) when given, else URL/Description.
    fn insert_link(&mut self, rid: &Val, url: &str, desc: &str, from: Option<Row>) -> R<Val> {
        let Some(rl) = self.links_of(rid)? else { return Ok(Val::Int(0)) };
        let now = crate::clock::now_ms(crate::clock::at(self, self.steps));
        if let Some(row) = from {
            rl.borrow_mut().row = (row).into();
        } else {
            Self::set_link_field(&rl, "URL1", Val::Text(url.to_string()))?;
            Self::set_link_field(&rl, "Description", Val::Text(desc.to_string()))?;
            Self::set_link_field(&rl, "User ID", Val::Text(self.user_id.clone()))?;
        }
        Self::set_link_field(&rl, "Link ID", Val::Int(0))?;
        Self::set_link_field(&rl, "Record ID", rid.clone())?;
        Self::set_link_field(&rl, "Created", Val::DateTime(now))?;
        Self::set_link_field(&rl, "Company", Val::Text(self.company.clone()))?;
        self.rec_insert(&rl, false)?.map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
        let b = rl.borrow();
        Ok(b.tdef.field("Link ID").map(|i| b.get_val(i)).unwrap_or(Val::Int(0)))
    }

    /// AddLink / HasLinks / DeleteLinks / DeleteLink / CopyLinks on record `h` (`l` lower-cased).
    pub fn record_link_method(&mut self, f: &mut Frame, h: &RecHandle, l: &str, args: &[Expr]) -> R<Val> {
        let mut a = Vec::with_capacity(args.len());
        for e in args {
            a.push(self.eval(f, e)?);
        }
        let rid = record_id_of(h);
        match l {
            "addlink" => {
                let url = a.first().map(|v| v.to_text()).unwrap_or_default();
                let desc = a.get(1).map(|v| v.to_text()).unwrap_or_default();
                self.insert_link(&rid, &url, &desc, None)
            }
            "haslinks" => {
                let Some(rl) = self.links_of(&rid)? else { return Ok(Val::Bool(false)) };
                Ok(Val::Bool(!self.fetch_all(&rl)?.is_empty()))
            }
            "deletelinks" | "deletelink" => {
                let Some(rl) = self.links_of(&rid)? else { return Ok(Val::Void) };
                let id = a.first().map(|v| v.as_i64());
                let link_fi = rl.borrow().tdef.field("Link ID");
                if l == "deletelink" {
                    // DeleteLink(Id): the link with that id, whichever record it belongs to
                    rl.borrow_mut().filters.clear();
                }
                for row in self.fetch_all(&rl)? {
                    if let (Some(id), Some(fi)) = (id, link_fi) {
                        if l == "deletelink" && store_to_val(&row[fi]).as_i64() != id {
                            continue;
                        }
                    }
                    rl.borrow_mut().row = (row).into();
                    self.rec_delete(&rl, false)?.map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                }
                Ok(Val::Void)
            }
            _ => {
                // CopyLinks(FromRecord / FromRecordRef): the source's links, added to this record
                let Some(Val::Record(src)) = a.first() else { return Ok(Val::Void) };
                let src_id = record_id_of(src);
                let Some(rl) = self.links_of(&src_id)? else { return Ok(Val::Void) };
                for row in self.fetch_all(&rl)? {
                    self.insert_link(&rid, "", "", Some(row))?;
                }
                Ok(Val::Void)
            }
        }
    }
}
