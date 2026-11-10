//! The deterministic clock and Guids (al_runtime::clock, al-test `--clock virtual`): readings of
//! CurrentDateTime always move forward, work and Sleep move the clock on, CreateGuid gives
//! distinct version 4 Guids, and a unit started again with the same seed repeats every value.

use al_model::{Catalog, ProcRef};
use al_runtime::{clock, Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
codeunit 50795 "Clock Tests"
{
    procedure Readings(): Text
    var
        A: DateTime;
        B: DateTime;
        C: DateTime;
        D: DateTime;
        G1: Guid;
        G2: Guid;
        i: Integer;
        x: Integer;
    begin
        A := CurrentDateTime();
        B := CurrentDateTime();
        if B <= A then
            Error('the clock did not move forward');
        for i := 1 to 2000 do
            x += i;
        C := CurrentDateTime();
        if C - B < 10 then
            Error('2000 iterations took less than 10 ms');
        Sleep(60000);
        D := CurrentDateTime();
        if D - C < 60000 then
            Error('Sleep(60000) did not move the clock on');
        if DT2Date(D) <> Today() then
            Error('Today is not the date of CurrentDateTime');
        G1 := CreateGuid();
        G2 := CreateGuid();
        if G1 = G2 then
            Error('CreateGuid repeated');
        exit(Format(A, 0, 9) + '|' + Format(D, 0, 9) + '|' + Format(G1) + '|' + Format(G2));
    end;

    procedure TwoGuids(): Text
    var
        G1: Guid;
        G2: Guid;
    begin
        G1 := CreateGuid();
        G2 := CreateGuid();
        exit(Format(G1) + '|' + Format(G2));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    // one fixed directory, the file replaced atomically (nothing is deleted)
    let dir = std::env::temp_dir().join("stab12-clock-test");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(format!("App.al.{}.{:?}.part", std::process::id(), std::thread::current().id()));
    std::fs::write(&tmp, APP).unwrap();
    std::fs::rename(&tmp, dir.join("App.al")).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(cat: &'static Catalog, proc: &str) -> String {
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = cat.code.iter().position(|c| c.name == "Clock Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn virtual_clock_and_guids_repeat_per_unit() {
    let cat = catalog();
    let start = clock::local_to_ms(chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap().and_hms_opt(10, 0, 0).unwrap());
    clock::set_virtual(start, 7);
    let first = call(cat, "Readings");
    clock::set_virtual(start, 7);
    let again = call(cat, "Readings");
    clock::set_wall();
    assert_eq!(first, again);
    let parts: Vec<&str> = first.split('|').collect();
    assert_eq!(parts.len(), 4, "{first}");
    for g in &parts[2..] {
        let u = uuid::Uuid::parse_str(g.trim_matches(|c| c == '{' || c == '}')).unwrap();
        assert_eq!(u.get_version_num(), 4, "{g}");
    }
    // another seed, other Guids
    clock::set_virtual(start, 8);
    let other = call(cat, "TwoGuids");
    clock::set_wall();
    assert_ne!(other, format!("{}|{}", parts[2], parts[3]));
}

#[test]
fn wall_clock_guids_differ() {
    let cat = catalog();
    clock::set_wall();
    assert_ne!(call(cat, "TwoGuids"), call(cat, "TwoGuids"));
}
