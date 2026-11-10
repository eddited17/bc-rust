//! System table 2000000164 "Time Zone": the Windows time zones of a BC 29.1 service tier, in
//! GetSystemTimeZones order and numbered from 1 (Ncl TimeZoneDataProvider). My Settings (page 9204)
//! shows the user's time zone through codeunit "Time Zone Selection Impl.", whose lookups are
//! repeated here.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 2000000164 "Time Zone"
{
    DataPerCompany = False;

    fields
    {
        field(1; ID; Text[180]) { }
        field(2; "Display Name"; Text[250]) { }
        field(3; "No."; Integer) { }
    }
    keys
    {
        key(pk; "No.") { }
        key(fk; "Display Name") { }
    }
}

codeunit 50870 "TZ Tests"
{
    // Time Zone Selection Impl.: FindTimeZoneNo + GetTimeZoneDisplayName
    procedure DisplayName(TimeZoneText: Text[180]): Text
    var
        TimeZone: Record "Time Zone";
        No: Integer;
    begin
        TimeZone.SetRange(ID, TimeZoneText);
        if not TimeZone.FindFirst() then begin
            TimeZone.SetFilter(ID, '''@*' + TimeZoneText + '*''');
            TimeZone.Find('=<>');
        end;
        No := TimeZone."No.";
        Clear(TimeZone);
        if TimeZone.Get(No) then
            exit(TimeZone."Display Name");
    end;

    procedure Summary(): Text
    var
        TimeZone: Record "Time Zone";
        First: Text;
    begin
        TimeZone.FindFirst();
        First := Format(TimeZone."No.") + ' ' + TimeZone.ID;
        TimeZone.FindLast();
        exit(Format(TimeZone.Count()) + '|' + First + '|' + Format(TimeZone."No.") + ' ' + TimeZone.ID);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join("tz-test");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(format!("App.al.{}.{:?}.part", std::process::id(), std::thread::current().id()));
    std::fs::write(&tmp, APP).unwrap();
    std::fs::rename(&tmp, dir.join("App.al")).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str, args: Vec<al_runtime::Val>) -> String {
    let ci = cat.code.iter().position(|c| c.name == "TZ Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    let args = args.into_iter().map(al_runtime::interp::Arg::Val).collect();
    match s.invoke(ProcRef { code: ci, proc: p }, args, Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn time_zone_table_lists_the_windows_time_zones() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let n = al_runtime::virt::windows_time_zones().count();
    assert!(n > 130, "{n} zones");
    assert_eq!(call(&mut s, cat, "Summary", vec![]), format!("{n}|1 Dateline Standard Time|{n} Line Islands Standard Time"));
    let t = |s: &mut Session, id: &str| call(s, cat, "DisplayName", vec![al_runtime::Val::Text(id.into())]);
    assert_eq!(t(&mut s, "UTC"), "(UTC) Coordinated Universal Time");
    assert_eq!(t(&mut s, "W. Europe Standard Time"), "(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna");
    // an exact id wins over ids that contain it ("Pacific Standard Time (Mexico)" comes first)
    assert_eq!(t(&mut s, "Pacific Standard Time"), "(UTC-08:00) Pacific Time (US & Canada)");
    // a partial id: the wildcard filter's first match
    assert_eq!(t(&mut s, "romance"), "(UTC+01:00) Brussels, Copenhagen, Madrid, Paris");
}
