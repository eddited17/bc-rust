//! Runtime semantics fixed by P10: Decimal `mod` / `div` with a fractional divisor (no false
//! "Division by zero"), and Date/Time format pictures (`<Month Text,3>`, `<Week>`, `<Quarter>`,
//! abbreviated field names like `<H>:<M>:<S>`, widths and `<Filler Character>`).

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
codeunit 50710 "P10 Tests"
{
    procedure DecimalMod(): Text
    var
        Qty: Decimal;
        Prec: Decimal;
    begin
        Qty := 1.5;
        Prec := 0.5;
        exit(Format(Qty mod Prec) + '|' + Format(1.25 mod 0.5) + '|' + Format(7.5 div 2.5) + '|' + Format(7 mod 3));
    end;

    procedure DecimalModZero(): Text
    var
        Prec: Decimal;
    begin
        asserterror Prec := 1.5 mod Prec;
        exit(GetLastErrorText());
    end;

    procedure DatePictures(): Text
    var
        D: Date;
    begin
        D := DMY2Date(5, 1, 2026);
        exit(Format(D, 0, '<Month Text,3> <Year4>') + '|' + Format(D, 0, '<Week>.<Year4>') + '|' + Format(D, 0, '<Quarter>/<Year4>') + '|' +
          Format(D, 0, '<Year4><Month,2><Day,2>') + '|' + Format(D, 0, '<Weekday Text>, <Month Text> <Day>') + '|' + Format(0D, 0, '<Year4>'));
    end;

    procedure CalcDateDefault(): Text
    var
        BucketSize: DateFormula;
    begin
        // the one-argument CalcDate is relative to Today, whatever WorkDate is
        WorkDate(DMY2Date(1, 1, 2001));
        Evaluate(BucketSize, '<1W>');
        exit(Format(CalcDate(BucketSize) - Today) + '|' + Format(CalcDate('<1D>', WorkDate()) - WorkDate()));
    end;

    procedure OptionStep(): Text
    var
        Step: Option Intro,"Company Details","Communication Details","Payment Details",Done;
        Seen: Text;
    begin
        Step := Step + 1;
        case Step of
            Step::Intro:
                Seen := 'intro';
            Step::"Company Details":
                Seen := 'company';
            Step::Done:
                Seen := 'done';
            else
                Seen := 'none';
        end;
        Step := Step - 1;
        exit(Seen + '|' + Format(Step) + '|' + Format(Step = Step::Intro));
    end;

    procedure TimePictures(): Text
    var
        T: Time;
    begin
        T := 170509T;
        exit(Format(T, 0, '<H>:<M>:<S>') + '|' + Format(T, 0, '<Hours24,2>:<Minutes,2>:<Seconds,2>') + '|' + Format(T, 0, '<Hours12>:<Minutes,2> <AM/PM>') + '|' +
          Format(080509T, 0, '<Hours24,2><Filler Character,0><Minutes,2>'));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p10-sem-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "P10 Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn decimal_mod_and_date_pictures() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    assert_eq!(call(&mut s, cat, "DecimalMod"), "0|0.25|3|1");
    assert_eq!(call(&mut s, cat, "DecimalModZero"), "Attempted to divide by zero.");
    assert_eq!(call(&mut s, cat, "DatePictures"), "Jan 2026|2.2026|1/2026|20260105|Monday, January 5|");
    assert_eq!(call(&mut s, cat, "TimePictures"), "17:5:9|17:05:09|5:05 PM|0805");
    assert_eq!(call(&mut s, cat, "CalcDateDefault"), "7|1");
    assert_eq!(call(&mut s, cat, "OptionStep"), "company|Intro|Yes");
}
