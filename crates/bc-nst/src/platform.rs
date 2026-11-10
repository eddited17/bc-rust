//! Platform trigger events: the System.app trigger codeunits in platform/ publish events
//! that only the service tier raises (docs/platform-tables.md, "Platform events"). bc-nst
//! raises them where the real tier does: Company Triggers when a session opens and closes
//! its company, System Action Triggers for the web client calls that ask for settings,
//! UI Helper Triggers for the system indicator and caption classes, Filter Helper Triggers
//! before a client filter is parsed.
//!
//! `var` parameters go in as slots and are read back. With no subscriber, `raise_event`
//! returns at once and the defaults stay.

use al_model::{FieldDef, FieldType};
use al_runtime::interp::{Arg, LVal};
use al_runtime::{Flow, Session, Val};
use al_syntax::ObjectKind;

pub const COMPANY_TRIGGERS: i64 = 2000000003;
pub const UI_HELPER_TRIGGERS: i64 = 2000000004;
pub const SYSTEM_ACTION_TRIGGERS: i64 = 2000000006;
pub const FILTER_HELPER_TRIGGERS: i64 = 2000000007;

type R<T> = Result<T, Flow>;

fn raise(s: &mut Session<'_>, codeunit: i64, event: &str, args: Vec<(&str, Arg)>) -> R<()> {
    s.raise_event(ObjectKind::Codeunit, codeunit, event, "", args, None, None)
}

fn var(v: Val) -> (al_runtime::value::Slot, Arg) {
    let sl = al_runtime::value::slot(v);
    (sl.clone(), Arg::Ref(LVal::Slot(sl, None)))
}

/// The message of an AL error, for logs.
pub fn flow_message(e: &Flow) -> String {
    match e {
        Flow::Err(e) => e.msg.clone(),
        _ => "unexpected control flow".into(),
    }
}

/// Company Triggers.OnCompanyOpen: while the session opens its company. A subscriber error
/// fails the open, as on the real tier.
pub fn company_open(s: &mut Session<'_>) -> R<()> {
    raise(s, COMPANY_TRIGGERS, "OnCompanyOpen", vec![])
}

/// Company Triggers.OnCompanyOpenCompleted: after the open. The event is isolated
/// (`BusinessEvent(false, true)`): a failing subscriber's changes roll back and the open
/// goes on. Returns the error text, if any.
pub fn company_open_completed(s: &mut Session<'_>) -> Option<String> {
    let sp = s.db.savepoint().ok();
    match raise(s, COMPANY_TRIGGERS, "OnCompanyOpenCompleted", vec![]) {
        Ok(()) => None,
        Err(e) => {
            if let Some(sp) = sp {
                let _ = s.db.rollback_to(sp);
            }
            Some(flow_message(&e))
        }
    }
}

/// Company Triggers.OnCompanyClose: while the session closes its company.
pub fn company_close(s: &mut Session<'_>) -> R<()> {
    raise(s, COMPANY_TRIGGERS, "OnCompanyClose", vec![])
}

/// A System Action Triggers event with one `var IsEnabled: Boolean` (GetAutoStartTours,
/// GetIsLegacyActionBarEnabled).
pub fn is_enabled(s: &mut Session<'_>, event: &str, default: bool) -> R<bool> {
    let (v, a) = var(Val::Bool(default));
    raise(s, SYSTEM_ACTION_TRIGGERS, event, vec![("IsEnabled", a)])?;
    let b = v.borrow().as_bool();
    Ok(b)
}

/// System Action Triggers.GetRoleCenterBannerPartID: the page shown as the role center's
/// banner (Checklist Banner when the user's checklist is due), 0 for none.
pub fn role_center_banner_part(s: &mut Session<'_>) -> R<i64> {
    let (v, a) = var(Val::Int(0));
    raise(s, SYSTEM_ACTION_TRIGGERS, "GetRoleCenterBannerPartID", vec![("PartID", a)])?;
    let id = v.borrow().as_i64();
    Ok(id)
}

/// System Action Triggers.GetDefaultRoleCenterID: the default role center page, 0 if no
/// subscriber names one.
pub fn default_role_center(s: &mut Session<'_>) -> R<i64> {
    let (v, a) = var(Val::Int(0));
    raise(s, SYSTEM_ACTION_TRIGGERS, "GetDefaultRoleCenterID", vec![("ID", a)])?;
    let id = v.borrow().as_i64();
    Ok(id)
}

fn guid_val(id: &str) -> Val {
    Val::Guid(al_runtime::value::parse_guid(id).unwrap_or([0; 16]))
}

/// System Action Triggers.GetNotificationStatus: whether the user still wants notification
/// `id` (My Notifications). Enabled unless a subscriber says otherwise.
pub fn notification_status(s: &mut Session<'_>, id: &str) -> R<bool> {
    let (v, a) = var(Val::Bool(true));
    raise(s, SYSTEM_ACTION_TRIGGERS, "GetNotificationStatus", vec![("NotificationId", Arg::Val(guid_val(id))), ("IsEnabled", a)])?;
    let b = v.borrow().as_bool();
    Ok(b)
}

/// System Action Triggers.SetNotificationStatus: the user turned notification `id` on or off.
pub fn set_notification_status(s: &mut Session<'_>, id: &str, enable: bool) -> R<()> {
    raise(s, SYSTEM_ACTION_TRIGGERS, "SetNotificationStatus", vec![("NotificationId", Arg::Val(guid_val(id))), ("Enable", Arg::Val(Val::Bool(enable)))])
}

/// UI Helper Triggers.GetSystemIndicator: the company badge text and style (Company
/// Information's System Indicator settings).
pub fn system_indicator(s: &mut Session<'_>) -> R<(String, i64)> {
    let (text, ta) = var(Val::Text(String::new()));
    let (style, sa) = var(Val::Opt { ord: 0, enum_id: None });
    raise(s, UI_HELPER_TRIGGERS, "GetSystemIndicator", vec![("Text", ta), ("Style", sa)])?;
    let t = text.borrow().to_text();
    let st = style.borrow().as_i64();
    Ok((t, st))
}

/// UI Helper Triggers.CaptionClassTranslate: a `CaptionClass` (`"<area>,<expr>"`) as the
/// caption the subscribers resolve it to (Caption Class Impl. handles area 3 and raises
/// "Caption Class".OnResolveCaptionClass for the others). `None` when nobody answers.
pub fn caption_class_translate(s: &mut Session<'_>, language: i32, caption_expr: &str) -> R<Option<String>> {
    let (tr, a) = var(Val::Text(String::new()));
    raise(
        s,
        UI_HELPER_TRIGGERS,
        "CaptionClassTranslate",
        vec![("Language", Arg::Val(Val::Int(language))), ("CaptionExpr", Arg::Val(Val::text(caption_expr))), ("Translation", a)],
    )?;
    let t = tr.borrow().to_text();
    Ok(if t.is_empty() { None } else { Some(t) })
}

/// Filter Helper Triggers.Make<Type>Filter for a filter the user typed on field `fd`, before
/// it is parsed: Filter Tokens Impl. replaces TODAY, WORKDATE, USER, COMPANY and the like.
/// Returns the filter text to parse.
pub fn make_filter(s: &mut Session<'_>, fd: &FieldDef, text: &str) -> R<String> {
    let (event, param) = match fd.ty {
        FieldType::Date => ("MakeDateFilter", "DateFilterText"),
        FieldType::DateTime => ("MakeDateTimeFilter", "DateTimeFilterText"),
        FieldType::Time => ("MakeTimeFilter", "TimeFilterText"),
        FieldType::Text(_) => ("MakeTextFilter", "TextFilterText"),
        FieldType::Code(_) => ("MakeCodeFilter", "TextFilterText"),
        FieldType::Integer | FieldType::BigInteger => ("MakeIntFilter", "IntFilterText"),
        _ => return Ok(text.to_string()),
    };
    let (v, a) = var(Val::text(text));
    raise(s, FILTER_HELPER_TRIGGERS, event, vec![(param, a)])?;
    let t = v.borrow().to_text();
    Ok(t)
}
