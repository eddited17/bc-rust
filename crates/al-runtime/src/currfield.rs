//! `CurrFieldNo`: a property of the record instance, as in BC (NavRecord.CurrFieldNo).
//!
//! BC's service tier writes it in one place only: when the client validates a field
//! (Microsoft.Dynamics.Nav.Service NSField.ValidateFieldAsync / LookupFieldAsync), it sets
//! `Rec.CurrFieldNo := FieldNo` on the page's record, runs Validate, and resets it to 0
//! afterwards, also on error. `Rec.Validate(...)` from AL code leaves it unchanged, so it is 0
//! unless the change came from the UI or AL code assigned it (`CurrFieldNo := FieldNo("No.")`).
//! Copies of a record (assignment, by-value parameters) start at 0.
//!
//! bc-rust keeps the value per record handle in the session, keyed by the handle's identity
//! (a `Weak` reference, so a dropped handle's address is never reused while its entry exists).

use crate::interp::{Session, R};
use crate::record::{RecCell, RecHandle};
use crate::value::Val;
use std::rc::{Rc, Weak};

/// Session state: the records whose CurrFieldNo is not 0.
#[derive(Default)]
pub struct CurrFields(Vec<(Weak<RecCell>, i32)>);

impl CurrFields {
    pub fn get(&self, h: &RecHandle) -> i32 {
        let p = Rc::as_ptr(h);
        self.0.iter().find(|(w, _)| std::ptr::eq(w.as_ptr(), p)).map(|(_, n)| *n).unwrap_or(0)
    }

    pub fn set(&mut self, h: &RecHandle, n: i32) {
        let p = Rc::as_ptr(h);
        self.0.retain(|(w, _)| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), p));
        if n != 0 {
            self.0.push((Rc::downgrade(h), n));
        }
    }
}

impl<'r> Session<'r> {
    /// `CurrFieldNo` of a record instance (0 unless set by a UI validation or AL code).
    pub fn curr_field_no(&self, h: &RecHandle) -> i32 {
        self.curr_fields.get(h)
    }

    pub fn set_curr_field_no(&mut self, h: &RecHandle, n: i32) {
        self.curr_fields.set(h, n);
    }

    /// A field changed through the UI (page control, TestPage field): the record's CurrFieldNo
    /// is the field's number while it validates, and 0 afterwards (NSField.ValidateFieldAsync).
    pub fn ui_validate(&mut self, h: &RecHandle, fi: usize, value: Option<Val>) -> R<()> {
        // an entered value outside the field's MinValue..MaxValue is rejected (crate::fieldrange)
        if let Some(v) = &value {
            let fd = h.borrow().field(fi);
            crate::fieldrange::check(fd, v).map_err(|m| crate::interp::Flow::Err(Box::new(crate::interp::AlError::new(m))))?;
        }
        let fid = h.borrow().field(fi).id as i32;
        self.set_curr_field_no(h, fid);
        let r = self.rec_validate(h, fi, value, fid);
        self.set_curr_field_no(h, 0);
        r
    }
}
