//! Media and MediaSet fields. A Media field holds the id (Guid) of a "Tenant Media" row whose
//! Content blob has the bytes; a MediaSet field holds the id of a set, whose members are
//! "Tenant Media Set" rows (set id, media id). Same layout as BC's platform tables.

use crate::interp::*;
use crate::record::*;
use crate::streams::OutTarget;
use crate::value::*;
use al_model::FieldType;
use al_syntax::Expr;
use bc_store::filter::{FNode, FieldFilter};
use bc_store::Value;

pub const TENANT_MEDIA: i64 = 2000000184;
pub const TENANT_MEDIA_SET: i64 = 2000000183;

fn guid_of(v: &Value) -> [u8; 16] {
    match v {
        Value::Guid(g) => *g,
        _ => [0; 16],
    }
}

fn al_err(e: String) -> Flow {
    Flow::Err(Box::new(AlError::new(e)))
}

impl<'r> Session<'r> {
    /// Methods on a Media or MediaSet field (`Rec.Picture.ImportStream(InStr, 'x')`). Returns
    /// None when `base` is not a Media/MediaSet field of a record.
    pub fn media_method(&mut self, f: &mut Frame, base: &Expr, m: &str, args: &[Expr]) -> R<Option<Val>> {
        // only field expressions (`Picture`, `Rec.Picture`, `Rec."Picture"`) can be Media fields
        if !matches!(base, Expr::Ident(_) | Expr::Member(..)) {
            return Ok(None);
        }
        let LVal::Field(r, fi) = self.lvalue(f, base)? else { return Ok(None) };
        let set = match r.borrow().field(fi).ty {
            FieldType::Media => false,
            FieldType::MediaSet => true,
            _ => return Ok(None),
        };
        if !self.cat.tables.contains_key(&TENANT_MEDIA) || !self.cat.tables.contains_key(&TENANT_MEDIA_SET) {
            return Ok(None);
        }
        let id = guid_of(&r.borrow().row[fi]);
        let mut a = Vec::with_capacity(args.len());
        for x in args {
            a.push(self.eval(f, x)?);
        }
        let v = match (set, m) {
            (_, "importstream") => {
                let bytes = match a.first() {
                    Some(Val::InStream(s)) => {
                        let mut st = s.borrow_mut();
                        let data = st.data.clone();
                        let rest = data[st.pos.min(data.len())..].to_vec();
                        st.pos = data.len();
                        rest
                    }
                    _ => vec![],
                };
                let desc = a.get(1).map(|v| v.to_text()).unwrap_or_default();
                let mime = a.get(2).map(|v| v.to_text()).filter(|s| !s.is_empty()).unwrap_or_else(|| sniff_mime(&bytes).to_string());
                let media = self.media_create(bytes, &desc, &mime)?;
                if set {
                    let set_id = if id == [0; 16] { crate::clock::new_guid() } else { id };
                    self.media_set_add(set_id, media)?;
                    r.borrow_mut().row[fi] = Value::Guid(set_id);
                } else {
                    r.borrow_mut().row[fi] = Value::Guid(media);
                }
                Val::Guid(media)
            }
            (true, "insert") => {
                let media = match a.first() {
                    Some(Val::Guid(g)) => *g,
                    _ => return err("MediaSet.Insert requires a media id"),
                };
                let set_id = if id == [0; 16] { crate::clock::new_guid() } else { id };
                let added = self.media_set_add(set_id, media)?;
                r.borrow_mut().row[fi] = Value::Guid(set_id);
                Val::Bool(added)
            }
            (true, "remove") => {
                let media = match a.first() {
                    Some(Val::Guid(g)) => *g,
                    _ => return err("MediaSet.Remove requires a media id"),
                };
                let h = self.new_record(TENANT_MEDIA_SET, false);
                set_field(&h, "ID", Value::Guid(id));
                set_field(&h, "Media ID", Value::Guid(media));
                let found = match self.rec_get_pk_r(&h)? {
                    Some(row) => {
                        h.borrow_mut().row = (row).into();
                        true
                    }
                    None => false,
                };
                if found {
                    self.rec_delete(&h, false)?.map_err(al_err)?;
                }
                Val::Bool(found)
            }
            (true, "count") => Val::Int(self.media_set_items(id)?.len() as i32),
            (true, "item") => {
                let i = a.first().map(|v| v.as_i64()).unwrap_or(1);
                let items = self.media_set_items(id)?;
                match items.get((i - 1).max(0) as usize) {
                    Some(g) if i >= 1 => Val::Guid(*g),
                    _ => return err(format!("The index {i} is out of range for the MediaSet.")),
                }
            }
            (false, "exportstream") => {
                let content = self.media_content(id)?;
                match (a.first(), content) {
                    (Some(Val::OutStream(t)), Some(bytes)) => {
                        write_out(t, &bytes);
                        Val::Bool(true)
                    }
                    _ => Val::Bool(false),
                }
            }
            (false, "hasvalue") => Val::Bool(id != [0; 16]),
            (true, "hasvalue") => Val::Bool(!self.media_set_items(id)?.is_empty()),
            (_, "mediaid") => Val::Guid(id),
            (_, "exportfile") => Val::Int(0),
            // ImportFile(FileName, Description [, MimeType]): a server file (crate::files::Vfs)
            (_, "importfile") => {
                let path = a.first().map(|v| v.to_text()).unwrap_or_default();
                let Some(bytes) = self.files.read(&path) else {
                    return err(format!("The file '{}' does not exist.", crate::files::full_path(&path)));
                };
                let desc = a.get(1).map(|v| v.to_text()).unwrap_or_default();
                let mime = a.get(2).map(|v| v.to_text()).filter(|s| !s.is_empty()).unwrap_or_else(|| sniff_mime(&bytes).to_string());
                let media = self.media_create(bytes, &desc, &mime)?;
                if set {
                    let set_id = if id == [0; 16] { crate::clock::new_guid() } else { id };
                    self.media_set_add(set_id, media)?;
                    r.borrow_mut().row[fi] = Value::Guid(set_id);
                } else {
                    r.borrow_mut().row[fi] = Value::Guid(media);
                }
                Val::Guid(media)
            }
            (_, "findorphans") => Val::List(std::rc::Rc::new(std::cell::RefCell::new(vec![]))),
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    /// Insert a "Tenant Media" row and return its id.
    pub fn media_create(&mut self, content: Vec<u8>, description: &str, mime: &str) -> R<[u8; 16]> {
        let id = crate::clock::new_guid();
        let h = self.new_record(TENANT_MEDIA, false);
        set_field(&h, "ID", Value::Guid(id));
        set_field(&h, "Description", Value::Text(description.chars().take(250).collect()));
        set_field(&h, "Content", Value::Blob(content));
        set_field(&h, "Mime Type", Value::Text(mime.chars().take(100).collect()));
        set_field(&h, "Company Name", Value::Text(self.company.chars().take(30).collect()));
        self.rec_insert(&h, false)?.map_err(al_err)?;
        Ok(id)
    }

    /// ImportStreamWithUrlAccess(Stream, FileName [, MinutesToExpire]): a Tenant Media row the
    /// OpenDynamicFileHandler serves by its security token until the expiration date (BC
    /// NavMedia.ALImportWithUrlAccess, 30 minutes by default).
    pub fn media_create_url_access(&mut self, a: &[Val]) -> R<[u8; 16]> {
        let content = crate::dotnet::text::arg_bytes(a.first());
        let name = a.get(1).map(|v| v.to_text()).unwrap_or_default();
        let minutes = a.get(2).map(|v| v.as_i64()).unwrap_or(30);
        let id = crate::clock::new_guid();
        let token: String = crate::clock::new_guid().iter().map(|b| format!("{b:02x}")).collect();
        let h = self.new_record(TENANT_MEDIA, false);
        set_field(&h, "ID", Value::Guid(id));
        set_field(&h, "Description", Value::Text(name.chars().take(250).collect()));
        set_field(&h, "Mime Type", Value::Text(sniff_mime(&content).into()));
        set_field(&h, "Content", Value::Blob(content));
        set_field(&h, "Company Name", Value::Text(self.company.chars().take(30).collect()));
        set_field(&h, "Security Token", Value::Text(token));
        set_field(&h, "Expiration Date", Value::DateTime(crate::clock::now_ms(crate::clock::at(self, self.steps)) + minutes * 60_000));
        self.rec_insert(&h, false)?.map_err(al_err)?;
        Ok(id)
    }

    /// GetDocumentUrl(Id): the OpenDynamicFileHandler URL of a media imported with URL access
    /// (BC ALSystemOperatingSystem.GetOpenDynamicFileHandlerUrl); '' for any other id.
    pub fn media_document_url(&mut self, id: [u8; 16]) -> String {
        if id == [0; 16] || !self.cat.tables.contains_key(&TENANT_MEDIA) {
            return String::new();
        }
        let h = self.new_record(TENANT_MEDIA, false);
        set_field(&h, "ID", Value::Guid(id));
        let Some(row) = self.rec_get_pk(&h) else { return String::new() };
        let token = match h.borrow().tdef.field("Security Token").map(|fi| &row[fi]) {
            Some(Value::Text(t)) if !t.is_empty() => t.clone(),
            _ => return String::new(),
        };
        format!("{}/OpenDynamicFileHandler.axd?token={token}", crate::platform::public_base_url())
    }

    fn media_content(&mut self, id: [u8; 16]) -> R<Option<Vec<u8>>> {
        if id == [0; 16] {
            return Ok(None);
        }
        let h = self.new_record(TENANT_MEDIA, false);
        set_field(&h, "ID", Value::Guid(id));
        let Some(row) = self.rec_get_pk_r(&h)? else { return Ok(None) };
        let fi = h.borrow().tdef.field("Content").unwrap_or(0);
        Ok(match &row[fi] {
            Value::Blob(b) => Some(b.clone()),
            _ => None,
        })
    }

    fn media_set_add(&mut self, set_id: [u8; 16], media: [u8; 16]) -> R<bool> {
        let h = self.new_record(TENANT_MEDIA_SET, false);
        set_field(&h, "ID", Value::Guid(set_id));
        set_field(&h, "Media ID", Value::Guid(media));
        if self.rec_get_pk_r(&h)?.is_some() {
            return Ok(false);
        }
        let n = self.media_set_items(set_id)?.len() as i64;
        set_field(&h, "Media Index", Value::BigInt(n + 1));
        set_field(&h, "Company Name", Value::Text(self.company.chars().take(30).collect()));
        self.rec_insert(&h, false)?.map_err(al_err)?;
        Ok(true)
    }

    /// Media ids of a set, in the order they were added.
    fn media_set_items(&mut self, set_id: [u8; 16]) -> R<Vec<[u8; 16]>> {
        if set_id == [0; 16] {
            return Ok(vec![]);
        }
        let h = self.new_record(TENANT_MEDIA_SET, false);
        let (idf, mf, xf) = {
            let b = h.borrow();
            (b.tdef.field("ID").unwrap_or(0), b.tdef.field("Media ID").unwrap_or(1), b.tdef.field("Media Index").unwrap_or(3))
        };
        h.borrow_mut().filters.push(FieldFilter { field: idf, node: FNode::Eq(Value::Guid(set_id)), text: String::new(), group: 0 });
        let mut rows = self.fetch_all(&h)?;
        rows.sort_by_key(|r| match &r[xf] {
            Value::BigInt(i) => *i,
            Value::Int(i) => *i as i64,
            _ => 0,
        });
        Ok(rows.iter().map(|r| guid_of(&r[mf])).collect())
    }
}

fn set_field(h: &RecHandle, name: &str, v: Value) {
    let mut b = h.borrow_mut();
    if let Some(i) = b.tdef.field(name) {
        b.row[i] = v;
    }
}

/// Append bytes to an OutStream's target.
pub(crate) fn write_out(t: &OutTarget, bytes: &[u8]) {
    match t {
        OutTarget::Field(r, fi) => {
            let mut rb = r.borrow_mut();
            match &mut rb.row[*fi] {
                Value::Blob(b) => b.extend_from_slice(bytes),
                other => *other = Value::Blob(bytes.to_vec()),
            }
        }
        OutTarget::Buffer(b) => b.borrow_mut().extend_from_slice(bytes),
    }
}

/// MIME type from the first bytes (what BC stores when ImportStream gets no MIME type).
fn sniff_mime(b: &[u8]) -> &'static str {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if b.starts_with(b"GIF8") {
        "image/gif"
    } else if b.starts_with(b"BM") {
        "image/bmp"
    } else if b.starts_with(b"%PDF") {
        "application/pdf"
    } else {
        "application/octet-stream"
    }
}
