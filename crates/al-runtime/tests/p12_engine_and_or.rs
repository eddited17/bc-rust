//! AL `and` / `or` evaluate both operands, left to right, without short-circuiting, in both
//! engines (P12): the tree-walker (`BC_ENGINE=tree`) and the compiled engine (`BC_ENGINE=compiled`,
//! the default since 2026-11-08; crate::compiled). Posting relies on it, e.g. Item Jnl.-Post Line:
//! `X := X or SetupTempSplitItemJnlLine(..)`.
//!
//! The engine is chosen once per process from BC_ENGINE, so the test runs itself in one child
//! process per engine.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const TEST: &str = "and_or_evaluate_both_operands_in_both_engines";
const CHILD: &str = "P12_AND_OR_CHILD";

const APP_JSON: &str = r#"{ "id": "3e9d6a51-7b2c-4f08-a1d3-5c6e7f809a1b", "name": "P12 And Or Test", "publisher": "bc-rust", "version": "1.0.0.0" }"#;

const APP: &str = r#"
codeunit 50730 "P12 And Or"
{
    var
        Calls: Integer;
        Order: Text;

    local procedure Bump(Tag: Text; Result: Boolean): Boolean
    begin
        Calls += 1;
        Order += Tag;
        exit(Result);
    end;

    procedure Run(): Text
    var
        T: Boolean;
        F: Boolean;
        X: Boolean;
        Y: Boolean;
        Z: Boolean;
        W: Boolean;
        N: Integer;
    begin
        T := true;
        F := false;
        // the posting pattern: the left operand is already true, the call must still run
        X := T;
        X := X or Bump('a', false);
        Y := T or Bump('b', false);
        Z := F and Bump('c', true);
        // both operands are calls: left before right
        W := Bump('d', false) and Bump('e', true);
        if T or Bump('f', false) then
            N += 1;
        if F and Bump('g', true) then
            N += 10;
        while F and Bump('h', true) do
            N += 100;
        exit(Format(Calls) + '|' + Order + '|' + Format(X) + Format(Y) + Format(Z) + Format(W) + '|' + Format(N));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p12-and-or-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn run_al() -> String {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = cat.code.iter().position(|c| c.name == "P12 And Or").unwrap() as u32;
    let p = cat.code[ci as usize].find("Run")[0];
    let out = match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("Run: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("Run: unexpected flow"),
    };
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("p12-and-or-test-{}", std::process::id())));
    out
}

#[test]
fn and_or_evaluate_both_operands_in_both_engines() {
    if let Ok(engine) = std::env::var(CHILD) {
        assert_eq!(al_runtime::compiled::enabled(), engine == "compiled", "BC_ENGINE={engine} did not select that engine");
        // 8 calls, in source order; a short-circuit would skip b, c, f, g and h (3 calls: "ade")
        assert_eq!(run_al(), "8|abcdefgh|YesYesNoNo|1", "BC_ENGINE={engine}");
        println!("P12-AND-OR-OK {engine}");
        return;
    }
    for engine in ["tree", "compiled"] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, engine)
            .env("BC_ENGINE", engine)
            .output()
            .expect("run the test binary");
        let (o, e) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success() && o.contains(&format!("P12-AND-OR-OK {engine}")), "BC_ENGINE={engine}:\n{o}\n{e}");
    }
}
