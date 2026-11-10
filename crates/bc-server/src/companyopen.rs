//! Company open for bc serve's own web client.
//!
//! BC's service tier raises Company Triggers.OnCompanyOpen and OnCompanyOpenCompleted when a session
//! opens its company. For interactive client types the System Application then records the sign-in:
//! System Initialization.OnAfterLogin -> User Login Time Tracker.CreateOrUpdateLoginInfo -> table 9008
//! "User Login" (at most one write a minute per user). Base App code reads those rows: Cash Flow
//! Forecast Update (codeunit 842) deletes its own job queue entry when nobody signed in for two weeks
//! (`UserLoginTimeTracker.AnyUserLoggedInSinceDate`). bc-nst does the same for Microsoft's web client
//! (bc-nst rpc.rs OpenCompany).
//!
//! bc serve's web client has no sessions: every request is one. Loading the web client (GET /api/home)
//! counts as opening the company, at most once a minute per user. API and OData requests don't count:
//! BC records no logins for ClientType Api/ODataV4/Background either.

use crate::api::Ctx;
use al_runtime::{Flow, Session};
use al_syntax::ObjectKind;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const COMPANY_TRIGGERS: i64 = 2000000003;
const EVERY: Duration = Duration::from_secs(60);
static LAST: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn msg(e: &Flow) -> String {
    match e {
        Flow::Err(e) => e.msg.clone(),
        _ => "unexpected control flow".into(),
    }
}

/// Whether `user` opened the company within the last minute; if not, remember that it does now.
fn due(user: &str) -> bool {
    let mut g = LAST.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    let key = user.to_uppercase();
    match m.get(&key) {
        Some(t) if t.elapsed() < EVERY => false,
        _ => {
            m.insert(key, Instant::now());
            true
        }
    }
}

/// Open the company for the request's user (crate::auth::bind) as the service tier does for an
/// interactive session: OnCompanyOpen (an error rolls back and is logged; the page still loads) and the
/// isolated OnCompanyOpenCompleted. Returns the error text, if any.
pub fn web_client_open(ctx: &Ctx) -> Option<String> {
    let mut s = match Session::try_new(ctx.rt, &ctx.company) {
        Ok(s) => s,
        Err(e) => return Some(e.to_string()),
    };
    crate::auth::bind(&mut s);
    if !due(&s.user_id) {
        return None;
    }
    open(&mut s)
}

/// The company-open triggers in session `s`, committed.
pub fn open(s: &mut Session) -> Option<String> {
    if let Err(e) = s.raise_event(ObjectKind::Codeunit, COMPANY_TRIGGERS, "OnCompanyOpen", "", vec![], None, None) {
        s.rollback();
        let m = format!("OnCompanyOpen failed: {}", msg(&e));
        eprintln!("bc: web client company open: {m}");
        return Some(m);
    }
    if let Err(e) = s.commit() {
        return Some(format!("commit after OnCompanyOpen: {e}"));
    }
    // OnCompanyOpenCompleted is isolated (BusinessEvent(false, true)): a failing subscriber's changes roll back
    let sp = s.db.savepoint().ok();
    let mut out = None;
    if let Err(e) = s.raise_event(ObjectKind::Codeunit, COMPANY_TRIGGERS, "OnCompanyOpenCompleted", "", vec![], None, None) {
        if let Some(sp) = sp {
            let _ = s.db.rollback_to(sp);
        }
        let m = format!("OnCompanyOpenCompleted subscriber failed (isolated, rolled back): {}", msg(&e));
        eprintln!("bc: web client company open: {m}");
        out = Some(m);
    }
    if let Err(e) = s.commit() {
        return Some(format!("commit after OnCompanyOpenCompleted: {e}"));
    }
    out
}
