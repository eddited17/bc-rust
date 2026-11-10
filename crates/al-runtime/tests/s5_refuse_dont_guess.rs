//! I14 S5, refuse don't guess (crate::assume, docs/assumptions.md): a registered guessed default
//! is recorded when AL code touches it, a refusal swallowed by asserterror or a TryFunction is
//! recorded, an ordinary caught AL error is not; and Decimal overflow raises BC's error instead of
//! a rust_decimal panic ("Pow overflowed").

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP_JSON: &str = r#"{ "id": "5a0c3e7d-91b4-4f26-8d1e-2b7f6c4a9e30", "name": "S5 Refuse Test", "publisher": "bc-rust", "version": "1.0.0.0" }"#;

const APP: &str = r#"
codeunit 50760 "S5 Guess"
{
    procedure GuessedDefault(): Text
    begin
        exit(Format(DebugMode()));
    end;

    procedure CaughtAlError(): Text
    begin
        asserterror Error('An ordinary error');
        exit(GetLastErrorText());
    end;

    procedure SwallowedByAssertError(): Text
    var
        J: JsonObject;
    begin
        asserterror J.S5NoSuchMethod();
        exit(CopyStr(GetLastErrorText(), 1, 26));
    end;

    procedure SwallowedByTryFunction(): Text
    begin
        exit(Format(TryRefused()));
    end;

    [TryFunction]
    local procedure TryRefused()
    var
        J: JsonObject;
    begin
        J.S5NoSuchMethod();
    end;

    procedure PowerOverflow(): Text
    var
        D: Decimal;
    begin
        asserterror D := Power(10, 30);
        exit(GetLastErrorText());
    end;

    procedure MulOverflow(): Text
    var
        D: Decimal;
    begin
        D := Power(10, 28);
        asserterror D := D * 10;
        exit(GetLastErrorText());
    end;

    procedure AddOverflow(): Text
    var
        D: Decimal;
    begin
        D := Power(10, 28) * 7;
        asserterror D := D + D;
        exit(GetLastErrorText());
    end;

    procedure RoundOverflow(): Text
    var
        D: Decimal;
    begin
        asserterror D := Round(Power(10, 27), 0.00001);
        exit(GetLastErrorText() + '|' + Format(Round(2.345, 0.01)));
    end;

    procedure InRange(): Text
    begin
        exit(Format(Power(2, 10)) + '|' + Format(Power(10, 28) * 7));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("s5-refuse-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "S5 Guess").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn guessed_defaults_swallowed_refusals_and_decimal_overflow() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");

    al_runtime::assume::reset();
    assert_eq!(call(&mut s, cat, "GuessedDefault"), "Yes");
    let r = al_runtime::assume::take();
    assert_eq!(r.assumed, vec![("System.ExportObjects", 1)]);
    assert_eq!(r.swallowed_n, 0);

    assert_eq!(call(&mut s, cat, "CaughtAlError"), "An ordinary error");
    let r = al_runtime::assume::take();
    assert!(r.assumed.is_empty() && r.swallowed_n == 0, "{r:?}");

    assert_eq!(call(&mut s, cat, "SwallowedByAssertError"), "Not implemented in bc-rust");
    let r = al_runtime::assume::take();
    assert_eq!(r.swallowed_n, 1, "{r:?}");
    assert!(r.swallowed[0].starts_with("Not implemented in bc-rust: JsonObject."), "{r:?}");

    assert_eq!(call(&mut s, cat, "SwallowedByTryFunction"), "No");
    assert_eq!(al_runtime::assume::take().swallowed_n, 1);

    // BC's Decimal18 overflow text, not a panic
    let ovf = "Value was either too large or too small for a Decimal.";
    assert_eq!(call(&mut s, cat, "PowerOverflow"), ovf);
    assert_eq!(call(&mut s, cat, "MulOverflow"), ovf);
    assert_eq!(call(&mut s, cat, "AddOverflow"), ovf);
    assert_eq!(call(&mut s, cat, "RoundOverflow"), format!("{ovf}|2.35"));
    assert_eq!(call(&mut s, cat, "InRange"), "1,024|70,000,000,000,000,000,000,000,000,000");
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("s5-refuse-test-{}", std::process::id())));
}
