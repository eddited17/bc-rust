//! AL UI calls (Confirm, StrMenu, Page.Run) shown in Microsoft's web client.
//!
//! [`NstUi`] is the session's [`al_runtime::interp::UiHost`]. Confirm and StrMenu are live
//! calls into the web server while the web server's own request (ActionField,
//! ValidateField, ...) is still waiting, the pattern the real service tier uses for its
//! `CreateDotNetHandle` / `InvokeAutomationMethod` callbacks (data/capture/nst-real/notes.md,
//! "Callbacks from the NST"):
//!
//! 1. bc-nst sends a JSON-RPC request with its own id counter (from 2):
//!    `Confirm([ConfirmRequest])` or `SelectionMenu([SelectionRequest])`, both
//!    `ServerResponse`s (`requests`, `VersionNumber`) plus their members
//!    (Microsoft.Dynamics.Nav.Types, `IClientCallbackApi`).
//! 2. The web server answers that request with `null` at once, shows the dialog, and sends
//!    the user's answer as a new request `EndClientCall([{"Result": {typeHint, value}}])`
//!    (`ServiceClientCallback.OnConfirm` / `OnSelectionMenu`). bc-nst answers that with
//!    `null` and continues the AL code.
//!
//! Frames that arrive meanwhile and are not part of the round trip are queued and handled
//! after the current call.
//!
//! Page.Run is not a live call: the page opens on the server (OnInit, OnOpenPage, the
//! caller's record and view) and the response of the current call carries a
//! `FormRunRequest` (`SendOnlyOnServerResponse`) with the new form's handle and state. The
//! web client builds the page from metadata and reads it through that handle
//! (`PageRunCallbackHandler.RunPage`).

use crate::form::PageForm;
use crate::rpc::Job;
use crate::wire::NIL_GUID;
use al_runtime::interp::{Instance, Session, UiHost};
use al_runtime::{RecHandle, Val};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc;

/// What the session worker and its UI host share.
#[derive(Clone)]
pub struct UiLink {
    pub sid: i32,
    pub token: i64,
    pub out: tokio::sync::mpsc::UnboundedSender<String>,
    pub jobs: Rc<mpsc::Receiver<Job>>,
    /// frames that arrived during a round trip and belong to the worker
    pub pending: Rc<RefCell<VecDeque<String>>>,
    /// bc-nst's own JSON-RPC request ids (the real NST starts at 2)
    pub next_id: Rc<Cell<i64>>,
    /// forms Page.Run opened during the current call: (handle, form)
    pub opened: Rc<RefCell<Vec<(String, PageForm)>>>,
    /// callback requests for the current call's response (`FormRunRequest`)
    pub requests: Rc<RefCell<Vec<Value>>>,
    /// the service state and the session's write log, for the nested worker of a RunModal
    pub st: std::sync::Arc<crate::rpc::NstState>,
    pub writes: crate::track::SharedLog,
    /// the session's page background tasks (crate::pbt)
    pub pbt: Rc<RefCell<crate::pbt::Host>>,
    /// wakes the session's worker when a page background task ends
    pub wake: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

/// `FormResult` (EndClientCall of a FormRunModal: `"Microsoft.Dynamics.Nav.Types.FormResult.LookupOK"`,
/// or a number) as AL's `Action`.
pub fn form_result_action(v: &Value, lookup: bool) -> Val {
    let n = match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::String(s) => match s.rsplit('.').next().unwrap_or("") {
            "OK" | "CloseOK" => 1,
            "Cancel" => 2,
            "LookupOK" => 3,
            "LookupCancel" => 4,
            "Yes" => 5,
            "No" => 6,
            "RunObject" => 9,
            "RunSystem" => 10,
            _ => 0,
        },
        _ => 0,
    };
    let name = match n {
        1 | 7 => "ok",
        3 => "lookupok",
        4 => "lookupcancel",
        5 => "yes",
        6 => "no",
        9 => "runobject",
        10 => "runsystem",
        _ if lookup => "lookupcancel",
        _ => "cancel",
    };
    Val::Opt { ord: al_runtime::interp::system_option_ord("action", name), enum_id: None }
}

impl UiLink {
    /// One round trip: send `method(param)`, wait for `EndClientCall` and return its
    /// `Result` value (`None` when the socket closed or the web server refused the call).
    fn round_trip(&self, method: &str, param: Value) -> Option<Value> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let msg = crate::wire::rpc_request(&json!(id), method, json!([param]), self.token);
        eprintln!("bc-nst[{}]: -> web server {method} (id {id})", self.sid);
        self.out.send(msg.to_string()).ok()?;
        loop {
            let text = match self.jobs.recv() {
                Ok(Job::Frame(text)) => text,
                // page background tasks are reported after the call (crate::pbt)
                Ok(Job::Wake) => continue,
                Err(_) => {
                    eprintln!("bc-nst[{}]: socket closed during {method}", self.sid);
                    return None;
                }
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            match v.get("method").and_then(Value::as_str) {
                None => {
                    // the web server's answer to our request: null, or an error
                    if v.get("id") == Some(&json!(id)) {
                        if let Some(e) = v.get("error") {
                            eprintln!("bc-nst[{}]: web server refused {method}: {}", self.sid, e);
                            return None;
                        }
                    }
                }
                Some(m @ ("EndClientCall" | "EndClientFormRunModalCall")) => {
                    if let Some(rid) = v.get("id") {
                        let _ = self.out.send(crate::wire::rpc_result(rid, Value::Null, Some(self.token)).to_string());
                    }
                    let p = v.pointer("/params/0").cloned().unwrap_or(Value::Null);
                    eprintln!("bc-nst[{}]: <- web server {m} {}", self.sid, crate::rpc::clip(&p.to_string(), 200));
                    // EndClientCall: {"Result": {"typeHint", "value"}} (no Result for null);
                    // EndClientFormRunModalCall: the FormResult itself
                    let r = match p {
                        Value::Object(ref o) if m == "EndClientCall" => o.get("Result").map(|r| r.get("value").cloned().unwrap_or_else(|| r.clone())).unwrap_or(Value::Null),
                        other => other,
                    };
                    return Some(r);
                }
                Some(_) => self.pending.borrow_mut().push_back(text),
            }
        }
    }
}

pub struct NstUi {
    pub link: UiLink,
}

/// AL dialog text as the client shows it: a backslash is a line break (Message, Confirm,
/// StrMenu instructions).
pub fn dialog_text(s: &str) -> String {
    s.replace('\\', "\n")
}

/// A `ServerResponse` callback argument. Its `requests` carry the session's pending Message()
/// texts, which the web server shows before the dialog (`InternalProcessServerCallbackRequests`).
fn server_response(s: &mut Session<'_>, extra: Map<String, Value>) -> Value {
    let mut m = Map::new();
    m.insert("requests".into(), crate::rpc::dialog_messages(std::mem::take(&mut s.messages)));
    m.insert("VersionNumber".into(), json!(0));
    m.extend(extra);
    Value::Object(m)
}

impl UiHost for NstUi {
    fn confirm(&mut self, s: &mut Session<'_>, question: &str, default: bool) -> Option<bool> {
        let mut m = Map::new();
        m.insert("AutomationId".into(), json!(NIL_GUID));
        m.insert("Message".into(), json!(dialog_text(question)));
        m.insert("DefaultValue".into(), json!(default));
        // no answer (socket closed, call refused) is No: never go on with a default the
        // user did not see
        let r = self.link.round_trip("Confirm", server_response(s, m)).unwrap_or(Value::Bool(false));
        Some(r.as_bool().unwrap_or(false))
    }

    fn strmenu(&mut self, s: &mut Session<'_>, options: &str, default: i32, instruction: &str) -> Option<i32> {
        let opts: Vec<&str> = options.split(',').collect();
        let mut m = Map::new();
        m.insert("AutomationId".into(), json!(NIL_GUID));
        m.insert("Options".into(), json!(opts));
        // the dialog's options are 0-based, AL's 1-based (0 = cancel)
        m.insert("DefaultSelection".into(), json!(default - 1));
        m.insert("Instruction".into(), json!(dialog_text(instruction)));
        // no answer is Cancel, as for Confirm
        let r = self.link.round_trip("SelectionMenu", server_response(s, m)).unwrap_or(Value::Null);
        let sel = r.as_i64().unwrap_or(-1);
        Some(if sel < 0 { 0 } else { sel as i32 + 1 })
    }

    fn run_page(&mut self, s: &mut Session<'_>, page_id: i64, inst: Option<Rc<Instance>>, rec: Option<RecHandle>, modal: bool) -> Option<al_runtime::interp::R<Val>> {
        let page = s.cat.pages.get(&page_id)?;
        // LookupMode, or Page.RunModal(Id, Rec) on a list: the page opens to pick a record
        // (OK / Cancel, LookupOK), as on the real service tier
        let list = matches!(page.page_type.to_ascii_lowercase().as_str(), "list" | "worksheet" | "listplus");
        let lookup = inst.as_ref().is_some_and(|i| i.lookup.get()) || (modal && inst.is_none() && rec.is_some() && list);
        let form = PageForm::new(s, page, inst, None);
        if let (Some(src), Some(dst)) = (&rec, &form.rec) {
            // the page shows the caller's view, positioned on the caller's record
            let r = src.borrow().clone();
            *dst.borrow_mut() = r;
        }
        if let Err(e) = form.open(s) {
            return Some(Err(al_runtime::interp::Flow::Err(Box::new(e))));
        }
        let mut in_db = false;
        if let Some(h) = &form.rec {
            if let Some(row) = s.rec_get_pk(h) {
                form.load_row(&row);
                in_db = true;
            } else {
                match s.find_first(h, false) {
                    Ok(found) => in_db = found,
                    Err(e) => return Some(Err(e)),
                }
            }
        }
        let handle = uuid::Uuid::new_v4().to_string();
        let mut st = form_state(&form, page_id, &handle, in_db);
        form.finish_state(s, &mut st);
        let editable = page.props.get("editable").is_none_or(|v| !v.eq_ignore_ascii_case("false"));
        st.insert("NavFormEditable".into(), json!(editable));
        let data = json!({
            "StoreFilterPersonalization": false,
            "FormHandle": handle,
            "LookupMode": lookup,
            "PageType": crate::metadata::page_type_code(&page.page_type),
            "MasterPageId": page_id,
            "State": Value::Object(st),
        });
        if modal {
            // FormRunModal: a live call; the web server works with the form through a nested
            // worker until it ends the call with the page's FormResult
            eprintln!("bc-nst[{}]: Page.RunModal {page_id} {} lookup={lookup} -> FormRunModal (form {handle})", self.link.sid, page.name);
            // SAFETY: Session<'r> borrows nothing but its runtime (`rt: &'r Runtime`). The check
            // makes sure that runtime is bc-nst's own, which is leaked ('static), so every borrow
            // in the session is 'static and only the lifetime parameter of the reference changes.
            // A session of another runtime (NstUi installed elsewhere) panics instead of being
            // extended to 'static.
            assert!(std::ptr::eq(s.rt, self.link.st.rt), "bc-nst: Page.RunModal from a session that does not use bc-nst's runtime");
            let s: &mut Session<'static> = unsafe { std::mem::transmute::<&mut Session<'_>, &mut Session<'static>>(s) };
            let (result, form) = crate::rpc::run_modal(&self.link, s, handle, form, data);
            let action = form_result_action(&result, lookup);
            // Page.RunModal(Id, Rec): the selected record goes back to the caller on LookupOK
            // (builtins copies it from `rec`); a page variable shares its Rec with the form
            if let (Some(dst), Some(f)) = (&rec, &form) {
                if let Some(src) = &f.rec {
                    let row = src.borrow().row.clone();
                    dst.borrow_mut().row = row;
                }
            }
            return Some(Ok(action));
        }
        eprintln!("bc-nst[{}]: Page.Run {page_id} {} -> FormRunRequest (form {handle})", self.link.sid, page.name);
        self.link.requests.borrow_mut().push(json!({"$polyType": "Microsoft.Dynamics.Nav.Types.FormRunRequest", "obj": {"Data": data, "AutomationId": NIL_GUID}}));
        self.link.opened.borrow_mut().push((handle, form));
        Some(Ok(Val::Opt { ord: al_runtime::interp::system_option_ord("action", "ok"), enum_id: None }))
    }
}

/// `NavRecordState` of a form opened on the server for the client: the record's view
/// (filters by group, key) and position.
fn form_state(form: &PageForm, page_id: i64, handle: &str, in_db: bool) -> Map<String, Value> {
    let mut st = Map::new();
    let mut tv = json!({"TableId": 0, "CurrentSortingFieldIds": [], "Ascending": true, "CurrentFilters": [], "SearchFilter": {"FilterGroupNo": -1, "Filters": []}});
    let mut current = Value::Null;
    if let Some(h) = &form.rec {
        let r = h.borrow();
        let t = r.tdef;
        tv["TableId"] = json!(t.id);
        let key: Vec<i64> = t.keys.get(r.key).map(|k| k.fields.iter().map(|&fi| t.fields[fi].id).collect()).unwrap_or_default();
        tv["CurrentSortingFieldIds"] = json!(key);
        tv["Ascending"] = json!(r.ascending);
        let mut groups: Vec<(i32, Vec<Value>)> = vec![];
        for f in &r.filters {
            if f.group < 0 {
                continue;
            }
            let item = json!({"FilterField": t.fields[f.field].id, "FilterValue": f.text, "UserTypedFilterValue": f.text, "RuntimeFilter": 1, "IsExactValue": false});
            match groups.iter_mut().find(|(g, _)| *g == f.group) {
                Some((_, v)) => v.push(item),
                None => groups.push((f.group, vec![item])),
            }
        }
        tv["CurrentFilters"] = Value::Array(groups.into_iter().map(|(g, fs)| json!({"FilterGroupNo": g, "Filters": fs})).collect());
        if in_db {
            current = json!(base64::engine::general_purpose::STANDARD.encode(crate::dataset::encode_bookmark(&r.schema, &r.row)));
        }
    }
    st.insert("RunFormOnRec".into(), json!(false));
    st.insert("TableView".into(), tv);
    st.insert("FlushDataCache".into(), json!(false));
    if !current.is_null() {
        st.insert("CurrentRecord".into(), current);
    }
    st.insert("PromptMode".into(), json!(0));
    st.insert("ValidateFieldsInOnNewRecord".into(), json!(true));
    st.insert("AllSelected".into(), json!(false));
    st.insert("SelectedRecords".into(), json!([]));
    st.insert("ServerFormHandle".into(), json!(handle));
    st.insert("FormId".into(), json!(page_id));
    st.insert("ParentFormId".into(), json!(0));
    st.insert("PersonalizationId".into(), json!(""));
    st.insert("IsResourceDefinedForm".into(), json!(false));
    st.insert("Timeout".into(), json!(0));
    st.insert("FormUpdateRequest".into(), json!(0));
    st.insert("SubFormUpdateRequests".into(), json!([]));
    st.insert("RecordState".into(), json!(if in_db { 0x40 | 0x20 } else { 0x100 }));
    st.insert("ValidateRequired".into(), json!(true));
    st.insert("ClientRecordDraft".into(), json!(false));
    st.insert("RenamingMode".into(), json!(0));
    st.insert("CurrentFilterGroup".into(), json!(0));
    st.insert("IsSubFormUpdateRequest".into(), json!(false));
    st.insert("MoreDataInReadDirection".into(), json!(false));
    st.insert("MoreDataInOppositeDirection".into(), json!(false));
    st.insert("UpdatePropagation".into(), json!(false));
    st.insert("RecordTemporary".into(), json!(false));
    st.insert("DataSourceType".into(), json!(0));
    st
}
