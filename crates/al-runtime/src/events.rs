//! Isolated events: `[IntegrationEvent(IncludeSender, GlobalVarAccess, Isolated)]`,
//! `[BusinessEvent(IncludeSender, Isolated)]` and `[InternalEvent(IncludeSender, Isolated)]` with
//! Isolated = true. Each subscriber then runs in its own transaction: committed when it
//! succeeds; when it fails, its database changes are rolled back, the error is swallowed (it
//! becomes GetLastErrorText) and the next subscriber runs. Changes to variables (VAR parameters,
//! single-instance state) are kept. As in BC, a write transaction that is open when the event is
//! raised is committed first (Microsoft's tests rely on it: with CommitBehavior::Ignore the commit
//! does not happen and the event runs like a normal one); while apps install, isolated events run
//! like normal events.

use crate::interp::*;
use al_model::ProcRef;
use al_syntax::{Expr, Procedure};
use std::rc::Rc;

/// The event procedure is declared isolated.
pub fn is_isolated(p: &Procedure) -> bool {
    p.attributes.iter().any(|a| {
        let at = match a.name.to_ascii_lowercase().as_str() {
            "integrationevent" => 2,
            "businessevent" | "internalevent" => 1,
            _ => return false,
        };
        matches!(a.args.get(at), Some(Expr::Bool(true)))
    })
}

impl<'r> Session<'r> {
    /// Prepare to raise an isolated event: commit the open write transaction (under test
    /// isolation Commit() is a savepoint, so "open" means written since the test's last Commit()).
    /// False when the event runs like a normal event (apps installing).
    pub(crate) fn isolation_applies(&mut self) -> R<bool> {
        if self.installing.is_some() {
            return Ok(false);
        }
        let open = match self.test.as_ref().map(|t| t.commit_mark) {
            Some(mark) => self.db.savepoint().map_err(Self::store_err)? > mark,
            None => self.db.in_write(),
        };
        if open {
            self.al_commit()?;
        }
        Ok(true)
    }

    /// Run one subscriber of an isolated event in its own transaction.
    pub(crate) fn invoke_isolated(&mut self, pr: ProcRef, args: Vec<Arg>, inst: Rc<Instance>) -> R<()> {
        let mark = self.db.savepoint().map_err(Self::store_err)?;
        match self.trapped(|s| s.invoke(pr, args, Some(inst), None, None)) {
            Err(Flow::Err(e)) => {
                self.note_error(&e);
                self.rollback_trapped(mark)
            }
            Err(o) => Err(o),
            Ok(_) => self.al_commit(),
        }
    }
}
