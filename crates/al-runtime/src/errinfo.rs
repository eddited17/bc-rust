//! The ErrorInfo type and collectible errors [src:A11b].
//!
//! An ErrorInfo carries a message plus Title, DetailedMessage, Collectible, ErrorType, Verbosity,
//! DataClassification, RecordId / SystemId / TableId / FieldNo / PageNo / ControlName,
//! CustomDimensions (a Dictionary of [Text, Text] owned by the ErrorInfo) and actions.
//! `ErrorInfo.Create(Message, Collectible, Record, FieldNo, PageNo, ControlName, Verbosity,
//! DataClassification, CustomDimensions, Title)` fills them.
//!
//! Collectible errors: a procedure with `[ErrorBehavior(ErrorBehavior::Collect)]` opens a
//! collection scope. Inside it (including callees), `Error(ErrorInfo)` with Collectible = true
//! records a copy and returns normally. When the outermost scope ends without an uncaught error
//! and errors were collected, one error is raised: the single message, or (for several) a summary
//! with the first message. HasCollectedErrors / GetCollectedErrors([Clear]) /
//! ClearCollectedErrors work on the current scope; IsCollectingErrors tells whether a scope is open.

use crate::interp::*;
use crate::value::*;
use std::cell::RefCell;
use std::rc::Rc;

pub type ErrRef = Rc<RefCell<ErrInfo>>;

#[derive(Debug, Default)]
pub struct ErrInfo {
    pub message: String,
    pub detailed_message: String,
    pub title: String,
    pub collectible: bool,
    /// ErrorType: Client = 0, Internal = 1.
    pub error_type: i32,
    /// Verbosity (Critical 0, Error 1, Warning 2, Normal 3, Verbose 4); Error by default.
    pub verbosity: i32,
    pub data_classification: i32,
    pub record_id: Option<Val>,
    pub system_id: [u8; 16],
    pub table_id: i64,
    pub field_no: i64,
    pub page_no: i64,
    pub control_name: String,
    pub custom_dimensions: Option<Rc<RefCell<Vec<(Val, Val)>>>>,
    pub callstack: String,
    /// (caption, codeunit id, method, description)
    pub actions: Vec<(String, i64, String, String)>,
    pub navigation_actions: Vec<(String, String)>,
}

impl Clone for ErrInfo {
    fn clone(&self) -> Self {
        ErrInfo {
            message: self.message.clone(),
            detailed_message: self.detailed_message.clone(),
            title: self.title.clone(),
            collectible: self.collectible,
            error_type: self.error_type,
            verbosity: self.verbosity,
            data_classification: self.data_classification,
            record_id: self.record_id.clone(),
            system_id: self.system_id,
            table_id: self.table_id,
            field_no: self.field_no,
            page_no: self.page_no,
            control_name: self.control_name.clone(),
            custom_dimensions: self.custom_dimensions.as_ref().map(|d| Rc::new(RefCell::new(d.borrow().clone()))),
            callstack: self.callstack.clone(),
            actions: self.actions.clone(),
            navigation_actions: self.navigation_actions.clone(),
        }
    }
}

pub fn new_val(message: String) -> Val {
    Val::ErrorInfo(Rc::new(RefCell::new(ErrInfo { message, verbosity: 1, ..Default::default() })))
}

/// A copy (ErrorInfo is a value type).
pub fn copy(e: &ErrRef) -> Val {
    Val::ErrorInfo(Rc::new(RefCell::new(e.borrow().clone())))
}

/// Session state of the error collection (ErrorCollection).
#[derive(Default)]
pub struct Collection {
    errors: Option<Vec<ErrInfo>>,
    /// Index of the current scope's first error; None when no scope is open.
    scope_start: Option<usize>,
}

/// `[ErrorBehavior(ErrorBehavior::Collect)]` on a procedure.
pub fn collects(p: &al_syntax::Procedure) -> bool {
    p.attributes.iter().any(|a| {
        a.name.eq_ignore_ascii_case("errorbehavior")
            && a.args.first().is_some_and(|x| matches!(x, al_syntax::Expr::Scope(_, n) if n.eq_ignore_ascii_case("collect")))
    })
}

impl<'r> Session<'r> {
    /// Open a collection scope; returns the previous scope start for [`Session::stop_collecting`].
    pub fn start_collecting(&mut self) -> Option<usize> {
        let c = &mut self.sys.errors;
        let n = c.errors.get_or_insert_with(Vec::new).len();
        std::mem::replace(&mut c.scope_start, Some(n))
    }

    /// Close a scope. The outermost scope raises the collected errors if the procedure succeeded.
    pub fn stop_collecting(&mut self, success: bool, prev: Option<usize>) -> R<()> {
        let c = &mut self.sys.errors;
        c.scope_start = prev;
        if prev.is_some() {
            return Ok(());
        }
        let errs = c.errors.take().unwrap_or_default();
        if !success || errs.is_empty() {
            return Ok(());
        }
        if errs.len() == 1 {
            return err_code("Dialog", errs[0].message.clone());
        }
        let detail = errs.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("\n");
        let mut e = AlError::new(format!("Multiple errors occurred during the operation. The first error is: {}", errs[0].message));
        e.code = "Dialog";
        e.stack = vec![];
        let _ = detail;
        Err(Flow::Err(Box::new(e)))
    }

    /// Error(ErrorInfo): a collectible error inside a collection scope is recorded (true).
    pub fn collect_error(&mut self, e: &ErrRef) -> bool {
        let c = &mut self.sys.errors;
        let (Some(errs), true) = (c.errors.as_mut(), e.borrow().collectible) else { return false };
        let mut x = e.borrow().clone();
        x.callstack = self.call_stack.iter().rev().map(|&ci| self.cat.code[ci as usize].name.clone()).collect::<Vec<_>>().join("\\");
        if x.error_type == 1 {
            x.message = "An internal error has occurred.".into();
        }
        errs.push(x);
        true
    }

    /// HasCollectedErrors / GetCollectedErrors / ClearCollectedErrors / IsCollectingErrors.
    pub fn collected_errors_fn(&mut self, l: &str, a: &[Val]) -> Val {
        let c = &mut self.sys.errors;
        let start = c.scope_start.unwrap_or(0);
        match l {
            "iscollectingerrors" => Val::Bool(c.errors.is_some()),
            "hascollectederrors" => Val::Bool(c.errors.as_ref().is_some_and(|e| e.len() > start)),
            "clearcollectederrors" => {
                if let Some(e) = c.errors.as_mut() {
                    e.truncate(start);
                }
                Val::Void
            }
            _ => {
                let list: Vec<Val> = c.errors.as_ref().map(|e| e[start.min(e.len())..].iter().map(|x| Val::ErrorInfo(Rc::new(RefCell::new(x.clone())))).collect()).unwrap_or_default();
                if a.first().is_some_and(|v| v.as_bool()) {
                    if let Some(e) = c.errors.as_mut() {
                        e.truncate(start);
                    }
                }
                Val::List(Rc::new(RefCell::new(list)))
            }
        }
    }

    /// ErrorInfo.Create(Message, Collectible, Record, FieldNo, PageNo, ControlName, Verbosity,
    /// DataClassification, CustomDimensions, Title).
    pub fn errorinfo_create(&mut self, a: &[Val]) -> Val {
        let mut e = ErrInfo { verbosity: 1, ..Default::default() };
        e.message = a.first().map(|v| v.to_text()).unwrap_or_default();
        e.collectible = a.get(1).is_some_and(|v| v.as_bool());
        if let Some(Val::Record(r)) = a.get(2) {
            let b = r.borrow();
            e.table_id = b.tdef.id;
            e.record_id = Some(Val::RecordId(b.tdef.id, b.tdef.pk().fields.iter().map(|&i| b.row[i].clone()).collect()));
            drop(b);
            if let Some(i) = r.borrow().tdef.field("SystemId") {
                if let Val::Guid(g) = r.borrow().get_val(i) {
                    e.system_id = g;
                }
            }
        }
        e.field_no = a.get(3).map(|v| v.as_i64()).unwrap_or(0);
        e.page_no = a.get(4).map(|v| v.as_i64()).unwrap_or(0);
        e.control_name = a.get(5).map(|v| v.to_text()).unwrap_or_default();
        if let Some(v) = a.get(6) {
            e.verbosity = v.as_i64() as i32;
        }
        if let Some(v) = a.get(7) {
            e.data_classification = v.as_i64() as i32;
        }
        if let Some(Val::Dict(d)) = a.get(8) {
            e.custom_dimensions = Some(Rc::new(RefCell::new(d.borrow().clone())));
        }
        e.title = a.get(9).map(|v| v.to_text()).unwrap_or_default();
        Val::ErrorInfo(Rc::new(RefCell::new(e)))
    }

    /// A method or property of an ErrorInfo value (`m` lower-cased); with an argument a property
    /// is set.
    pub fn errorinfo_method(&mut self, e: &ErrRef, m: &str, a: &[Val]) -> R<Val> {
        match m {
            "addaction" => {
                let t = |i: usize| a.get(i).map(|v| v.to_text()).unwrap_or_default();
                let cu = a.get(1).map(|v| v.as_i64()).unwrap_or(0);
                let ok = self.cat.code_by_key.get(&(al_syntax::ObjectKind::Codeunit, cu)).is_some_and(|&ci| !self.code(ci).find(&t(2)).is_empty());
                let mut x = e.borrow_mut();
                if ok && x.actions.len() < 3 {
                    x.actions.push((t(0), cu, t(2), t(3)));
                }
                Ok(Val::Bool(ok))
            }
            "addnavigationaction" => {
                let t = |i: usize| a.get(i).map(|v| v.to_text()).unwrap_or_default();
                e.borrow_mut().navigation_actions.push((t(0), t(1)));
                Ok(Val::Bool(true))
            }
            _ => {
                if let Some(v) = a.first() {
                    set_prop(e, m, v);
                }
                match get_prop(e, m) {
                    Some(v) => Ok(v),
                    None => {
                        *self.missing.entry(format!("ErrorInfo.{m}")).or_default() += 1;
                        err(format!("Not implemented in bc-rust: ErrorInfo.{m}"))
                    }
                }
            }
        }
    }
}

/// An ErrorInfo property (`p` lower-cased); None for an unknown name.
pub fn get_prop(e: &ErrRef, p: &str) -> Option<Val> {
    let mut x = e.borrow_mut();
    Some(match p {
        "message" => Val::Text(x.message.clone()),
        "detailedmessage" => Val::Text(x.detailed_message.clone()),
        "title" => Val::Text(x.title.clone()),
        "collectible" => Val::Bool(x.collectible),
        "errortype" => Val::Opt { ord: x.error_type, enum_id: None },
        "verbosity" => Val::Opt { ord: x.verbosity, enum_id: None },
        "dataclassification" => Val::Opt { ord: x.data_classification, enum_id: None },
        "recordid" => x.record_id.clone().unwrap_or(Val::RecordId(0, vec![])),
        "systemid" => Val::Guid(x.system_id),
        "tableid" => Val::Int(x.table_id as i32),
        "fieldno" => Val::Int(x.field_no as i32),
        "pageno" => Val::Int(x.page_no as i32),
        "controlname" => Val::Text(x.control_name.clone()),
        // the ErrorInfo's own dictionary: CustomDimensions.Add(...) changes it
        "customdimensions" => Val::Dict(x.custom_dimensions.get_or_insert_with(|| Rc::new(RefCell::new(vec![]))).clone()),
        "callstack" => Val::Text(x.callstack.clone()),
        _ => return None,
    })
}

/// Set an ErrorInfo property; false for an unknown or read-only name.
pub fn set_prop(e: &ErrRef, p: &str, v: &Val) -> bool {
    let mut x = e.borrow_mut();
    match p {
        "message" => x.message = v.to_text(),
        "detailedmessage" => x.detailed_message = v.to_text(),
        "title" => x.title = v.to_text(),
        "collectible" => x.collectible = v.as_bool(),
        "errortype" => x.error_type = v.as_i64() as i32,
        "verbosity" => x.verbosity = v.as_i64() as i32,
        "dataclassification" => x.data_classification = v.as_i64() as i32,
        "recordid" => {
            if let Val::RecordId(t, _) = v {
                x.table_id = *t;
            }
            x.record_id = Some(v.clone());
        }
        "systemid" => {
            x.system_id = match v {
                Val::Guid(g) => *g,
                o => parse_guid(&o.to_text()).unwrap_or([0; 16]),
            }
        }
        "tableid" => x.table_id = v.as_i64(),
        "fieldno" => x.field_no = v.as_i64(),
        "pageno" => x.page_no = v.as_i64(),
        "controlname" => x.control_name = v.to_text(),
        "customdimensions" => {
            if let Val::Dict(d) = v {
                x.custom_dimensions = Some(Rc::new(RefCell::new(d.borrow().clone())));
            }
        }
        _ => return false,
    }
    true
}
