//! The `Dialog` data type (progress windows) and the `Dialog` system class.
//!
//! A Dialog variable (`Window: Dialog`) is a progress window: Open / Update / Close /
//! HideSubsequentDialogs. Without a client UI (background sessions, tests) BC shows nothing and
//! the calls only evaluate their arguments; test codeunits need no handler for them.
//! `Dialog.Confirm` / `Dialog.Message` / `Dialog.Error` / `Dialog.StrMenu` are the global
//! functions under their class name and go through the same UI handlers (crate::testing).

use crate::interp::*;
use crate::value::*;
use al_syntax::Expr;

impl<'r> Session<'r> {
    /// `Dialog.<name>(...)` where `Dialog` is the system class, not a variable.
    pub fn dialog_static(&mut self, f: &mut Frame, name: &str, args: &[Expr]) -> R<Val> {
        match name.to_ascii_lowercase().as_str() {
            "confirm" | "message" | "error" | "strmenu" => self.builtin(f, name, args),
            _ => self.dialog_method(f, name, args),
        }
    }

    /// A method of a Dialog variable.
    pub fn dialog_method(&mut self, f: &mut Frame, name: &str, args: &[Expr]) -> R<Val> {
        let m = name.to_ascii_lowercase();
        if matches!(m.as_str(), "confirm" | "message" | "error" | "strmenu") {
            return self.builtin(f, name, args);
        }
        for a in args {
            self.eval(f, a)?;
        }
        match m.as_str() {
            // progress window: nothing to show without a client
            "open" | "update" | "close" | "hidesubsequentdialogs" | "loginternalerror" => Ok(Val::Void),
            _ => {
                *self.missing.entry(format!("Dialog.{name}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: Dialog.{name}"))
            }
        }
    }
}
