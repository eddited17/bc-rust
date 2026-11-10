//! Procedure calls written as property assignments: AL lets a procedure with one parameter be
//! called as `Obj.Proc := Value;` (and `Proc := Value;` inside its own object), e.g.
//! `EnvironmentInfoTestLibrary.SetTestabilitySoftwareAsAService := true;`. This works for
//! codeunit, page, report, query, XMLport and table procedures, whenever the name is not a
//! variable, field or property of the target.

use crate::interp::*;
use crate::record::RecHandle;
use crate::value::*;
use al_model::ProcRef;
use al_syntax::{Expr, ObjectKind, ProcKind};
use std::rc::Rc;

impl<'r> Session<'r> {
    /// The one-parameter procedure `name` of `code`, if any.
    fn one_param_proc(&self, code: u32, name: &str) -> Option<ProcRef> {
        let co = self.code(code);
        co.find(name).iter().copied().find(|&i| {
            let p = &co.procs[i as usize];
            p.kind != ProcKind::Trigger && p.params.len() == 1
        })
        .map(|proc| ProcRef { code, proc })
    }

    fn call_with_value(&mut self, pr: ProcRef, v: &Val, inst: Option<Rc<Instance>>, rec: Option<RecHandle>) -> R<()> {
        let by_ref = self.code(pr.code).procs[pr.proc as usize].params[0].by_ref;
        let arg = if by_ref { Arg::Ref(LVal::Slot(slot(v.clone()), None)) } else { Arg::Val(v.clone()) };
        // a statement: a [TryFunction]'s error is not turned into `false`
        self.pending_discard = true;
        self.invoke(pr, vec![arg], inst, rec, None)?;
        Ok(())
    }

    /// `target := v` as a procedure call. Returns false (nothing done) when `target` is a
    /// variable, field or property, or no one-parameter procedure of that name exists. Only
    /// targets whose base is a plain variable (or `Rec` / `this`) are considered, so no
    /// expression is evaluated twice.
    pub fn try_assign_call(&mut self, f: &mut Frame, target: &Expr, v: &Val) -> R<bool> {
        match target {
            Expr::Ident(n) => {
                if f.locals.iter().any(|x| x.name.eq_ignore_ascii_case(n))
                    || f.globals.vars.iter().any(|x| x.name.eq_ignore_ascii_case(n))
                    || f.with.iter().any(|r| r.borrow().tdef.field(n).is_some())
                    || f.rec.as_ref().is_some_and(|r| r.borrow().tdef.field(n).is_some())
                {
                    return Ok(false);
                }
                let co = self.code(f.code);
                let own = self.one_param_proc(f.code, n);
                let base = co.extends.and_then(|b| self.cat.code_by_key.get(&b).copied()).and_then(|bc| self.one_param_proc(bc, n));
                match own.or(base) {
                    Some(pr) => {
                        self.call_with_value(pr, v, f.inst.clone(), f.rec.clone())?;
                        Ok(true)
                    }
                    None => Ok(false),
                }
            }
            Expr::Member(base, name) => {
                let Some(b) = self.plain_value(f, base) else { return Ok(false) };
                match b {
                    Val::Object(inst) | Val::Iface(Some(inst)) => {
                        let co = self.code(inst.code);
                        let mut pr = self.one_param_proc(inst.code, name);
                        if pr.is_none() && co.kind == ObjectKind::Page {
                            if let Some(pd) = self.cat.pages.get(&co.id) {
                                pr = pd.ext_code.iter().find_map(|&ec| self.one_param_proc(ec, name));
                            }
                        }
                        match pr {
                            Some(pr) => {
                                self.call_with_value(pr, v, Some(inst), None)?;
                                Ok(true)
                            }
                            None => Ok(false),
                        }
                    }
                    Val::Record(r) => {
                        let tdef = r.borrow().tdef;
                        if tdef.field(name).is_some() {
                            return Ok(false);
                        }
                        let pr = std::iter::once(tdef.code).chain(tdef.ext_code.iter().copied()).find_map(|c| self.one_param_proc(c, name));
                        match pr {
                            Some(pr) => {
                                self.call_with_value(pr, v, None, Some(r))?;
                                Ok(true)
                            }
                            None => Ok(false),
                        }
                    }
                    // Notification.Message := X, SessionSettings.LanguageId := X (crate::platobj)
                    Val::Plat(p) => self.plat_set(&p, name, v),
                    // File.TextMode := X / File.WriteMode := X: the File properties (crate::files)
                    Val::Opaque(t) if crate::files::is_file_val(&b) && (name.eq_ignore_ascii_case("textmode") || name.eq_ignore_ascii_case("writemode")) => {
                        self.file_method(f, t, base, &name.to_ascii_lowercase(), &[Expr::Bool(v.as_bool())], false)?;
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }

    /// The value of a variable, `Rec` or `this` without side effects; None for other expressions.
    fn plain_value(&mut self, f: &mut Frame, e: &Expr) -> Option<Val> {
        match e {
            Expr::Ident(n) => {
                if let Some(v) = f.locals.iter().rev().find(|x| x.name.eq_ignore_ascii_case(n)).or_else(|| f.globals.vars.iter().find(|x| x.name.eq_ignore_ascii_case(n))) {
                    return Some(v.slot.borrow().clone());
                }
                if n.eq_ignore_ascii_case("rec") {
                    return f.rec.clone().map(Val::Record);
                }
                None
            }
            Expr::This => match &f.inst {
                Some(i) => Some(Val::Object(i.clone())),
                None => f.rec.clone().map(Val::Record),
            },
            Expr::Member(b, n) if matches!(**b, Expr::This) => f.globals.vars.iter().find(|x| x.name.eq_ignore_ascii_case(n)).map(|v| v.slot.borrow().clone()),
            // CurrPage.<Part>.Page: the part's page instance (finding it has no side effects)
            Expr::Member(b, n) if n.eq_ignore_ascii_case("page") && matches!(&**b, Expr::Member(c, _) if matches!(&**c, Expr::Ident(cp) if cp.eq_ignore_ascii_case("currpage"))) => self.eval(f, e).ok(),
            _ => None,
        }
    }
}
