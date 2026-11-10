//! Platform value types with state: `Notification` and `SessionSettings` (`Val::Plat`).
//!
//! Both are value types in BC (NavNotification / NavSessionSettings clone on assignment and on
//! by-value parameters); `var` parameters share the value.
//!
//! Notification (Ncl NavNotification): Id / Message / Scope (get or set), SetData / GetData /
//! HasData, AddAction (at most 3; the codeunit must have the method, otherwise false), Send and
//! Recall. Send needs a message, gives the notification a new Id when it has none, and goes to
//! the test's [SendNotificationHandler] if it has one (no handler is not an error under the test
//! runner); outside tests the session keeps it in `sys.notifications` until it is recalled.
//! Recall needs an Id and goes to the [RecallNotificationHandler]. With a used return value
//! (`if N.Send() then`) errors become false.
//!
//! FilterPageBuilder (Ncl NavFilterPageBuilder): named RecordRefs in AddTable order. AddTable
//! opens a new RecordRef, AddRecord takes a copy of the record (GetTable), AddRecordRef shares the
//! given one; AddField / AddFieldNo set an optional default filter on the field; GetView returns
//! the RecordRef's GetView(UseCaptions, default true), SetView sets its view (an empty view is an
//! error). Count, Name(Index), PageCaption ("Filter Page" until set). RunModal needs the test
//! runner's filter page handler and is not modelled.
//!
//! WebServiceActionContext (Ncl NavWebServiceActionContext): the `var ActionContext` of an API
//! page's bound action. SetObjectType / SetObjectId / AddEntityKey / SetResultCode and their
//! getters, Clear. The OData layer turns it into the response (bc-server odata.rs).
//!
//! SessionSettings (Ncl NavSessionSettings): Init loads the user's User Personalization row
//! (table 2000000073) or else the session's values; RequestSessionUpdate(Save) saves it when
//! asked and goes to the test's [SessionSettingsHandler] (required in tests, like other UI).

use crate::interp::*;
use crate::value::*;
use al_syntax::{Expr, ObjectKind};
use std::cell::RefCell;
use std::rc::Rc;

pub type PlatRef = Rc<RefCell<Plat>>;

#[derive(Clone, Debug)]
pub enum Plat {
    Notification(Notif),
    SessionSettings(SessSettings),
    FilterPageBuilder(FilterPage),
    ActionContext(ActionCtx),
}

/// WebServiceActionContext state. `result_code` is the WebServiceActionResultCode ordinal
/// (interp::system_option_ord: None, Get, Created, Updated, Deleted).
#[derive(Clone, Debug, Default)]
pub struct ActionCtx {
    pub object_type: i32,
    pub object_id: i64,
    /// (field number, value) in AddEntityKey order.
    pub keys: Vec<(i64, Val)>,
    pub result_code: i32,
}

/// HTTP status of a WebServiceActionResultCode ordinal (Ncl WebServiceActionResultCode:
/// None = 0, Get = Updated = 200, Created = 201, Deleted = 204).
pub fn action_result_status(ord: i32) -> u16 {
    [0, 200, 201, 200, 204].get(ord as usize).copied().unwrap_or(0)
}

/// FilterPageBuilder state: caption and (name, RecordRef, filter field numbers) in AddTable order.
#[derive(Clone, Default)]
pub struct FilterPage {
    pub caption: String,
    pub items: Vec<(String, crate::record::RecHandle, Vec<i64>)>,
}

impl std::fmt::Debug for FilterPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterPage").field("caption", &self.caption).field("items", &self.items.iter().map(|i| &i.0).collect::<Vec<_>>()).finish()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Notif {
    pub id: [u8; 16],
    pub message: String,
    /// NotificationScope: 0 LocalScope, 1 GlobalScope.
    pub scope: i32,
    pub data: Vec<(String, String)>,
    /// (caption, codeunit id, method, tooltip)
    pub actions: Vec<(String, i64, String, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct SessSettings {
    pub company: String,
    pub profile_id: String,
    pub profile_app_id: [u8; 16],
    pub profile_system_scope: bool,
    pub language_id: i32,
    pub locale_id: i32,
    pub time_zone: String,
    pub saved: bool,
}

/// User Personalization (system table 2000000073) and its fields.
const USER_PERSONALIZATION: i64 = 2000000073;
const UP_USER_SID: i64 = 3;
const UP_PROFILE_ID: i64 = 9;
const UP_APP_ID: i64 = 10;
const UP_SCOPE: i64 = 11;
const UP_LANGUAGE_ID: i64 = 12;
const UP_COMPANY: i64 = 15;
const UP_LOCALE_ID: i64 = 27;
const UP_TIME_ZONE: i64 = 30;

pub fn new_notification() -> Val {
    Val::Plat(Rc::new(RefCell::new(Plat::Notification(Notif::default()))))
}

pub fn new_session_settings() -> Val {
    Val::Plat(Rc::new(RefCell::new(Plat::SessionSettings(SessSettings::default()))))
}

pub fn new_action_context() -> Val {
    Val::Plat(Rc::new(RefCell::new(Plat::ActionContext(ActionCtx::default()))))
}

pub fn new_filter_page_builder() -> Val {
    Val::Plat(Rc::new(RefCell::new(Plat::FilterPageBuilder(FilterPage::default()))))
}

pub fn type_name(p: &Plat) -> &'static str {
    match p {
        Plat::Notification(_) => "Notification",
        Plat::SessionSettings(_) => "SessionSettings",
        Plat::FilterPageBuilder(_) => "FilterPageBuilder",
        Plat::ActionContext(_) => "WebServiceActionContext",
    }
}

/// A copy for assignment / by-value passing (a FilterPageBuilder clones its RecordRefs, as
/// NavFilterPageBuilder.Clone does).
pub fn copy(p: &PlatRef) -> Val {
    let c = match &*p.borrow() {
        Plat::FilterPageBuilder(fp) => Plat::FilterPageBuilder(FilterPage {
            caption: fp.caption.clone(),
            items: fp.items.iter().map(|(n, h, fs)| (n.clone(), crate::record::copy_record(h), fs.clone())).collect(),
        }),
        other => other.clone(),
    };
    Val::Plat(Rc::new(RefCell::new(c)))
}

pub fn is_notification(v: &Val) -> bool {
    matches!(v, Val::Plat(p) if matches!(&*p.borrow(), Plat::Notification(_)))
}

fn guid_of(v: &Val) -> [u8; 16] {
    match v {
        Val::Guid(g) => *g,
        o => parse_guid(&o.to_text()).unwrap_or([0; 16]),
    }
}

/// WebServiceActionContext methods (Ncl NavWebServiceActionContext).
fn action_context_method(p: &PlatRef, m: &str, a: &[Val]) -> R<Val> {
    let mut b = p.borrow_mut();
    let Plat::ActionContext(c) = &mut *b else { unreachable!() };
    Ok(match m {
        "setobjecttype" => {
            c.object_type = a.first().map(|v| v.as_i64() as i32).unwrap_or(0);
            Val::Void
        }
        "getobjecttype" => Val::Opt { ord: c.object_type, enum_id: None },
        "setobjectid" => {
            c.object_id = a.first().map(|v| v.as_i64()).unwrap_or(0);
            Val::Void
        }
        "getobjectid" => Val::Int(c.object_id as i32),
        "addentitykey" => {
            let field = a.first().map(|v| v.as_i64()).unwrap_or(0);
            if c.keys.iter().any(|(f, _)| *f == field) {
                // Dictionary.Add of an existing key: NavNCLArgumentException
                return err("The argument is not valid for WebServiceActionContext.");
            }
            c.keys.push((field, a.get(1).cloned().unwrap_or(Val::Void)));
            Val::Bool(true)
        }
        "setresultcode" => {
            c.result_code = a.first().map(|v| v.as_i64() as i32).unwrap_or(0);
            Val::Void
        }
        "getresultcode" => Val::Opt { ord: c.result_code, enum_id: None },
        "clear" => {
            *c = ActionCtx::default();
            Val::Void
        }
        _ => return err(format!("WebServiceActionContext.{m} is not supported")),
    })
}

impl<'r> Session<'r> {
    /// A method of a Notification or SessionSettings value (`m` lower-cased). `discard`: the
    /// call is a statement (errors are raised instead of returning false).
    pub fn plat_method(&mut self, f: &mut Frame, p: &PlatRef, m: &str, args: &[Expr], discard: bool) -> R<Val> {
        if matches!(&*p.borrow(), Plat::FilterPageBuilder(_)) {
            return self.filter_page_method(f, p, m, args, discard);
        }
        let mut a = Vec::with_capacity(args.len());
        for e in args {
            a.push(crate::dotnet::unboxed(self.eval(f, e)?));
        }
        let is_notif = matches!(&*p.borrow(), Plat::Notification(_));
        if is_notif {
            self.notification_method(p, m, &a, discard)
        } else if matches!(&*p.borrow(), Plat::ActionContext(_)) {
            action_context_method(p, m, &a)
        } else {
            self.session_settings_method(p, m, &a)
        }
    }

    /// `N.Message := X` style assignment (crate::propcall): the setter with one value.
    pub fn plat_set(&mut self, p: &PlatRef, m: &str, v: &Val) -> R<bool> {
        let m = m.to_ascii_lowercase();
        let ok = match &mut *p.borrow_mut() {
            Plat::FilterPageBuilder(fp) => {
                if m != "pagecaption" {
                    return Ok(false);
                }
                fp.caption = v.to_text();
                return Ok(true);
            }
            Plat::Notification(_) => matches!(m.as_str(), "id" | "message" | "scope"),
            Plat::SessionSettings(_) => {
                matches!(m.as_str(), "company" | "profileid" | "profileappid" | "profilesystemscope" | "languageid" | "localeid" | "timezone")
            }
            Plat::ActionContext(_) => return Ok(false),
        };
        if !ok {
            return Ok(false);
        }
        let is_notif = matches!(&*p.borrow(), Plat::Notification(_));
        if is_notif {
            self.notification_method(p, &m, std::slice::from_ref(v), true)?;
        } else {
            self.session_settings_method(p, &m, std::slice::from_ref(v))?;
        }
        Ok(true)
    }

    fn notification_method(&mut self, p: &PlatRef, m: &str, a: &[Val], discard: bool) -> R<Val> {
        macro_rules! n {
            () => {
                match &mut *p.borrow_mut() {
                    Plat::Notification(n) => n,
                    _ => unreachable!(),
                }
            };
        }
        Ok(match m {
            "id" => {
                if let Some(v) = a.first() {
                    n!().id = guid_of(v);
                }
                Val::Guid(n!().id)
            }
            "message" => {
                if let Some(v) = a.first() {
                    n!().message = v.to_text();
                }
                Val::Text(n!().message.clone())
            }
            "scope" => {
                if let Some(v) = a.first() {
                    n!().scope = v.as_i64() as i32;
                }
                Val::Opt { ord: n!().scope, enum_id: None }
            }
            "setdata" => {
                let k = a.first().map(|v| v.to_text()).unwrap_or_default();
                let v = a.get(1).map(|v| v.to_text()).unwrap_or_default();
                let mut b = p.borrow_mut();
                let Plat::Notification(n) = &mut *b else { unreachable!() };
                match n.data.iter_mut().find(|(x, _)| *x == k) {
                    Some(e) => e.1 = v,
                    None => n.data.push((k, v)),
                }
                Val::Void
            }
            "getdata" => {
                let k = a.first().map(|v| v.to_text()).unwrap_or_default();
                Val::Text(n!().data.iter().find(|(x, _)| *x == k).map(|(_, v)| v.clone()).unwrap_or_default())
            }
            "hasdata" => {
                let k = a.first().map(|v| v.to_text()).unwrap_or_default();
                Val::Bool(n!().data.iter().any(|(x, _)| *x == k))
            }
            "addaction" => {
                let caption = a.first().map(|v| v.to_text()).unwrap_or_default();
                let cu = a.get(1).map(|v| v.as_i64()).unwrap_or(0);
                let method = a.get(2).map(|v| v.to_text()).unwrap_or_default();
                let tip = a.get(3).map(|v| v.to_text()).unwrap_or_default();
                let has_method = self.cat.code_by_key.get(&(ObjectKind::Codeunit, cu)).is_some_and(|&ci| !self.code(ci).find(&method).is_empty());
                let mut b = p.borrow_mut();
                let Plat::Notification(n) = &mut *b else { unreachable!() };
                if n.actions.len() >= 3 || !has_method {
                    Val::Bool(false)
                } else {
                    n.actions.push((caption, cu, method, tip));
                    Val::Bool(true)
                }
            }
            "send" => {
                if n!().message.is_empty() {
                    return if discard { err("The notification message cannot be empty.") } else { Ok(Val::Bool(false)) };
                }
                if n!().id == [0; 16] {
                    n!().id = crate::clock::new_guid();
                }
                if self.in_test() {
                    self.ui_send_notification(Val::Plat(p.clone()))?;
                } else {
                    let copy = match &*p.borrow() {
                        Plat::Notification(n) => n.clone(),
                        _ => unreachable!(),
                    };
                    self.sys.notifications.retain(|x| x.id != copy.id);
                    self.sys.notifications.push(copy);
                }
                Val::Bool(true)
            }
            "recall" => {
                let id = n!().id;
                if id == [0; 16] {
                    return if discard { err("The notification cannot be recalled because its ID is not known.") } else { Ok(Val::Bool(false)) };
                }
                if self.in_test() {
                    self.ui_recall_notification(Val::Plat(p.clone()))?;
                } else {
                    self.sys.notifications.retain(|x| x.id != id);
                }
                Val::Bool(true)
            }
            _ => {
                *self.missing.entry(format!("Notification.{m}")).or_default() += 1;
                return err(format!("Not implemented in bc-rust: Notification.{m}"));
            }
        })
    }

    fn session_settings_method(&mut self, p: &PlatRef, m: &str, a: &[Val]) -> R<Val> {
        macro_rules! s {
            () => {
                match &mut *p.borrow_mut() {
                    Plat::SessionSettings(s) => s,
                    _ => unreachable!(),
                }
            };
        }
        Ok(match m {
            "init" => {
                let loaded = self.load_user_personalization();
                let company = self.company.clone();
                let lang = self.global_language;
                *s!() = loaded.unwrap_or_else(|| SessSettings { company, language_id: lang, locale_id: lang, time_zone: "UTC".into(), ..Default::default() });
                Val::Void
            }
            "requestsessionupdate" => {
                let save = a.first().is_some_and(|v| v.as_bool());
                if save {
                    let s = s!().clone();
                    self.save_user_personalization(&s)?;
                }
                s!().saved = save;
                if self.in_test() {
                    match self.take_handler(crate::testing::UiKind::SessionSettings, None) {
                        Some((pr, inst)) => {
                            self.invoke(pr, vec![Arg::Ref(LVal::Slot(slot(Val::Plat(p.clone())), None))], Some(inst), None, None)?;
                        }
                        None => return err_code("MissingUIHandler", "Unhandled UI: SessionSettings"),
                    }
                }
                Val::Void
            }
            "company" => {
                if let Some(v) = a.first() {
                    s!().company = v.to_text();
                }
                Val::Text(s!().company.clone())
            }
            "profileid" => {
                if let Some(v) = a.first() {
                    s!().profile_id = v.to_text();
                }
                Val::Text(s!().profile_id.clone())
            }
            "profileappid" => {
                if let Some(v) = a.first() {
                    s!().profile_app_id = guid_of(v);
                }
                Val::Guid(s!().profile_app_id)
            }
            "profilesystemscope" => {
                if let Some(v) = a.first() {
                    s!().profile_system_scope = v.as_bool();
                }
                Val::Bool(s!().profile_system_scope)
            }
            "languageid" => {
                if let Some(v) = a.first() {
                    s!().language_id = v.as_i64() as i32;
                }
                Val::Int(s!().language_id)
            }
            "localeid" => {
                if let Some(v) = a.first() {
                    s!().locale_id = v.as_i64() as i32;
                }
                Val::Int(s!().locale_id)
            }
            "timezone" => {
                if let Some(v) = a.first() {
                    s!().time_zone = v.to_text();
                }
                Val::Text(s!().time_zone.clone())
            }
            _ => {
                *self.missing.entry(format!("SessionSettings.{m}")).or_default() += 1;
                return err(format!("Not implemented in bc-rust: SessionSettings.{m}"));
            }
        })
    }

    /// UserSecurityId() of the session (crate::builtins "usersecurityid").
    fn user_sid(&self) -> [u8; 16] {
        self.user_sid
    }

    fn user_personalization_rec(&mut self) -> Option<crate::record::RecHandle> {
        if !self.cat.tables.contains_key(&USER_PERSONALIZATION) {
            return None;
        }
        let h = self.new_record(USER_PERSONALIZATION, false);
        let fi = h.borrow().tdef.field_no(UP_USER_SID)?;
        h.borrow_mut().row[fi] = bc_store::Value::Guid(self.user_sid());
        Some(h)
    }

    fn load_user_personalization(&mut self) -> Option<SessSettings> {
        let h = self.user_personalization_rec()?;
        let row = self.rec_get_pk(&h)?;
        let t = h.borrow().tdef;
        let get = |id: i64| t.field_no(id).map(|i| crate::record::to_val(&t.fields[i], &row[i]));
        Some(SessSettings {
            company: get(UP_COMPANY).map(|v| v.to_text()).unwrap_or_default(),
            profile_id: get(UP_PROFILE_ID).map(|v| v.to_text()).unwrap_or_default(),
            profile_app_id: get(UP_APP_ID).map(|v| guid_of(&v)).unwrap_or([0; 16]),
            profile_system_scope: false,
            language_id: get(UP_LANGUAGE_ID).map(|v| v.as_i64() as i32).unwrap_or(0),
            locale_id: get(UP_LOCALE_ID).map(|v| v.as_i64() as i32).unwrap_or(0),
            time_zone: get(UP_TIME_ZONE).map(|v| v.to_text()).unwrap_or_default(),
            saved: false,
        })
    }

    fn save_user_personalization(&mut self, s: &SessSettings) -> R<()> {
        let Some(h) = self.user_personalization_rec() else { return Ok(()) };
        let existing = self.rec_get_pk_r(&h)?;
        if let Some(row) = &existing {
            h.borrow_mut().row = (row.clone()).into();
        }
        {
            let mut r = h.borrow_mut();
            let t = r.tdef;
            let mut set = |id: i64, v: Val| {
                if let Some(i) = t.field_no(id) {
                    if let Ok(sv) = crate::record::to_store(&t.fields[i], &v) {
                        r.row[i] = sv;
                    }
                }
            };
            set(UP_LANGUAGE_ID, Val::Int(s.language_id));
            set(UP_LOCALE_ID, Val::Int(s.locale_id));
            set(UP_PROFILE_ID, Val::Text(s.profile_id.clone()));
            set(UP_APP_ID, Val::Guid(s.profile_app_id));
            set(UP_COMPANY, Val::Text(s.company.clone()));
            set(UP_TIME_ZONE, Val::Text(s.time_zone.clone()));
            set(UP_SCOPE, Val::Int(1));
        }
        let r = if existing.is_some() { self.rec_modify(&h, false)? } else { self.rec_insert(&h, false)? };
        r.map_err(|m| Flow::Err(Box::new(AlError::new(m))))
    }
}

impl<'r> Session<'r> {
    /// FilterPageBuilder methods (NavFilterPageBuilder). Errors are raised for a statement call
    /// (`discard`) and give the method's "false" value when the return value is used.
    fn filter_page_method(&mut self, f: &mut Frame, p: &PlatRef, m: &str, args: &[Expr], discard: bool) -> R<Val> {
        let find = |p: &PlatRef, name: &str| -> Option<(crate::record::RecHandle, i64)> {
            match &*p.borrow() {
                Plat::FilterPageBuilder(fp) => fp.items.iter().find(|i| i.0 == name).map(|i| (i.1.clone(), i.1.borrow().tdef.id)),
                _ => None,
            }
        };
        let fail = |msg: String, falsy: Val| -> R<Val> { if discard { err(msg) } else { Ok(falsy) } };
        match m {
            "count" => Ok(Val::Int(match &*p.borrow() {
                Plat::FilterPageBuilder(fp) => fp.items.len() as i32,
                _ => 0,
            })),
            "pagecaption" => {
                if let Some(e) = args.first() {
                    let v = self.eval(f, e)?.to_text();
                    if let Plat::FilterPageBuilder(fp) = &mut *p.borrow_mut() {
                        fp.caption = v;
                    }
                    return Ok(Val::Void);
                }
                Ok(Val::Text(match &*p.borrow() {
                    Plat::FilterPageBuilder(fp) if !fp.caption.is_empty() => fp.caption.clone(),
                    _ => "Filter Page".to_string(),
                }))
            }
            "name" => {
                let i = match args.first() {
                    Some(e) => self.eval(f, e)?.as_i64(),
                    None => 0,
                };
                let names: Vec<String> = match &*p.borrow() {
                    Plat::FilterPageBuilder(fp) => fp.items.iter().map(|x| x.0.clone()).collect(),
                    _ => vec![],
                };
                if i < 1 || i as usize > names.len() {
                    return err(format!("The filter control index value {i} is out of range. The value must be greater than or equal to 1 and less than or equal to {}.", names.len()));
                }
                Ok(Val::Text(names[i as usize - 1].clone()))
            }
            "addtable" | "addrecord" | "addrecordref" => {
                let name = match args.first() {
                    Some(e) => self.eval(f, e)?.to_text(),
                    None => String::new(),
                };
                if name.is_empty() {
                    // own wording [src:X10]
                    return fail("A table name is required.".into(), Val::Text(String::new()));
                }
                let src = match args.get(1) {
                    Some(e) => self.eval(f, e)?,
                    None => Val::Void,
                };
                let h = match (m, &src) {
                    ("addtable", v) => {
                        let id = v.as_i64();
                        if id < 1 {
                            // own wording [src:X10]
                            return fail("The table number of a filter page must be greater than zero.".into(), Val::Text(String::new()));
                        }
                        if let Some((_, existing)) = find(p, &name) {
                            if existing != id {
                                // own wording [src:X10]
                                return fail(format!("The name {name} is already used for another table."), Val::Text(String::new()));
                            }
                            return Ok(Val::Text(name));
                        }
                        if !self.cat.tables.contains_key(&id) {
                            return fail(format!("The table {id} does not exist."), Val::Text(String::new()));
                        }
                        let h = self.new_record(id, false);
                        h.borrow_mut().is_ref = true;
                        h
                    }
                    (_, Val::Record(r)) => {
                        if let Some((_, existing)) = find(p, &name) {
                            if existing != r.borrow().tdef.id {
                                // own wording [src:X10]
                                return fail(format!("The name {name} is already used for another table."), Val::Text(String::new()));
                            }
                            return Ok(Val::Text(name));
                        }
                        if m == "addrecordref" {
                            r.clone()
                        } else {
                            // RecordRef.GetTable(Record): a copy of the record with its filters
                            let c = crate::record::copy_record(r);
                            {
                                let mut b = c.borrow_mut();
                                b.is_ref = true;
                                b.unopened = false;
                            }
                            c
                        }
                    }
                    _ => return fail(format!("FilterPageBuilder.{m} needs a record."), Val::Text(String::new())),
                };
                if let Plat::FilterPageBuilder(fp) = &mut *p.borrow_mut() {
                    fp.items.push((name.clone(), h, vec![]));
                }
                Ok(Val::Text(name))
            }
            "addfield" | "addfieldno" => {
                let name = match args.first() {
                    Some(e) => self.eval(f, e)?.to_text(),
                    None => String::new(),
                };
                // AddField(Name, Rec.Field [, Filter]): the field's number; AddFieldNo(Name, No [, Filter])
                let field_no = match args.get(1) {
                    Some(Expr::Member(b, fname)) if m == "addfield" => match self.eval(f, b)? {
                        Val::Record(r) => {
                            let rb = r.borrow();
                            match rb.tdef.field(fname) {
                                Some(fi) => rb.tdef.fields[fi].id,
                                None => return err(format!("Field {fname} does not exist in table {}", rb.tdef.name)),
                            }
                        }
                        _ => self.eval(f, &args[1])?.as_i64(),
                    },
                    Some(e) => match self.eval(f, e)? {
                        Val::FieldRef(h, fi) => h.borrow().tdef.fields[fi].id,
                        v => v.as_i64(),
                    },
                    None => 0,
                };
                let filter = match args.get(2) {
                    Some(e) => Some(self.eval(f, e)?.to_text()),
                    None => None,
                };
                let Some((h, _)) = find(p, &name) else {
                    // own wording [src:X10]
                    return fail(format!("The FilterPageBuilder has no control named {name}."), Val::Bool(false));
                };
                let fi = h.borrow().tdef.fields.iter().position(|x| x.id == field_no);
                let Some(fi) = fi else {
                    return fail(format!("The field with ID {field_no} is not found in table {}.", h.borrow().tdef.name), Val::Bool(false));
                };
                if let Some(flt) = filter {
                    self.fieldref_method(f, &h, fi, &Expr::Int(field_no), "setfilter", &[Expr::Str(flt.into())])?;
                }
                if let Plat::FilterPageBuilder(fp) = &mut *p.borrow_mut() {
                    if let Some(item) = fp.items.iter_mut().find(|i| i.0 == name) {
                        if !item.2.contains(&field_no) {
                            item.2.push(field_no);
                        }
                    }
                }
                Ok(Val::Bool(true))
            }
            "getview" => {
                let name = match args.first() {
                    Some(e) => self.eval(f, e)?.to_text(),
                    None => String::new(),
                };
                let captions = match args.get(1) {
                    Some(e) => self.eval(f, e)?.as_bool(),
                    None => true,
                };
                if name.is_empty() {
                    // own wording [src:X10]
                    return err("A table name is required.");
                }
                match find(p, &name) {
                    Some((h, _)) => self.record_method(f, &h, "getview", &[Expr::Bool(captions)]),
                    None => Ok(Val::Text(String::new())),
                }
            }
            "setview" => {
                let name = match args.first() {
                    Some(e) => self.eval(f, e)?.to_text(),
                    None => String::new(),
                };
                let view = match args.get(1) {
                    Some(e) => self.eval(f, e)?.to_text(),
                    None => String::new(),
                };
                if view.is_empty() {
                    // own wording [src:X10]
                    return fail("A view is required.".into(), Val::Bool(false));
                }
                let Some((h, _)) = find(p, &name) else {
                    // own wording [src:X10]
                    return fail(format!("The FilterPageBuilder has no control named {name}."), Val::Bool(false));
                };
                self.record_method(f, &h, "setview", &[Expr::Str(view.into())])?;
                Ok(Val::Bool(true))
            }
            // NavTestExecution.TestHandleModalForm: the [FilterPageHandler] gets one var RecordRef
            // per table in AddTable order; its Boolean result is OK (true) or Cancel (false)
            "runmodal" => {
                let recs: Vec<crate::record::RecHandle> = match &*p.borrow() {
                    Plat::FilterPageBuilder(fp) => fp.items.iter().map(|i| i.1.clone()).collect(),
                    _ => vec![],
                };
                let res = if !self.in_test() {
                    // own wording [src:X11]
                    err("FilterPageBuilder.RunModal needs a client, and this session has none.")
                } else {
                    match self.take_handler(crate::testing::UiKind::FilterPage, None) {
                        Some((pr, inst)) => {
                            let args = recs.into_iter().map(|h| Arg::Ref(LVal::Slot(slot(Val::Record(h)), None))).collect();
                            self.trapped(|s| s.invoke(pr, args, Some(inst), None, None)).map(|r| Val::Bool(r.as_bool()))
                        }
                        None => err_code("MissingUIHandler", "Unhandled UI: FilterPage"),
                    }
                };
                // ALRunModalAsync: with a used return value (TrapError) any error, also the missing
                // handler, gives false
                match res {
                    Err(Flow::Err(e)) if !discard => {
                        self.note_error(&e);
                        Ok(Val::Bool(false))
                    }
                    other => other,
                }
            }
            _ => {
                *self.missing.entry(format!("FilterPageBuilder.{m}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: FilterPageBuilder.{m}"))
            }
        }
    }
}
