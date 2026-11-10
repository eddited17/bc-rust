//! Next() read-ahead (al-runtime recops `next`) must not serve rows that another record variable of
//! the session changed after they were read. On PostgreSQL every Next() reads up to 50 rows ahead;
//! a custom sort order (SetCurrentKey on fields no key starts with) reads the whole set ahead on
//! every backend. Found by the full PostgreSQL run (docs/pg-vs-memory.md): SCM Bugfixes planning
//! deleted a paired Reservation Entry through a second variable, and the loop's Next() returned it
//! again ("The Reservation Entry does not exist").
//! Memory always; PostgreSQL with `BC_PG_TEST=<scratch connection>` (a company schema
//! `c_s3ara<pid>` with an empty table stays in that scratch database).

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session, Val};
use bc_store::{Backend, MemBackend, PgBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50970 "S3a RA Row"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Decimal) { }
        field(3; Note; Text[30]) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

codeunit 50971 "S3a RA Tests"
{
    procedure DeletedByOtherVariable(CustomSort: Boolean): Text
    var
        R: Record "S3a RA Row";
        R2: Record "S3a RA Row";
        Seen: Text;
    begin
        Fill();
        if CustomSort then
            R.SetCurrentKey(Amount);
        R.FindSet();
        Seen := R."No.";
        R.Next();
        Seen += R."No.";
        R.Next(); // C; on PostgreSQL D and E are read ahead now
        Seen += R."No.";
        R2.Get('D');
        R2.Delete();
        if R.Next() <> 0 then
            Seen += R."No.";
        exit(Seen);
    end;

    procedure ModifiedByOtherVariable(CustomSort: Boolean): Text
    var
        R: Record "S3a RA Row";
        R2: Record "S3a RA Row";
    begin
        Fill();
        if CustomSort then
            R.SetCurrentKey(Amount);
        R.FindSet();
        R.Next();
        R.Next(); // C; D and E read ahead
        R2.Get('D');
        R2.Note := 'changed';
        R2.Modify();
        R.Next(); // D as it is now
        exit(R."No." + ':' + R.Note);
    end;

    procedure OwnModifyKeepsLoop(CustomSort: Boolean): Text
    var
        R: Record "S3a RA Row";
        Seen: Text;
    begin
        Fill();
        if CustomSort then
            R.SetCurrentKey(Amount);
        R.FindSet();
        repeat
            R.Note := 'seen';
            R.Modify();
            Seen += R."No.";
        until R.Next() = 0;
        exit(Seen);
    end;

    local procedure Fill()
    begin
        Ins('A', 1);
        Ins('B', 2);
        Ins('C', 3);
        Ins('D', 4);
        Ins('E', 5);
    end;

    local procedure Ins(No: Code[20]; Amt: Decimal)
    var
        R: Record "S3a RA Row";
    begin
        R."No." := No;
        R.Amount := Amt;
        R.Insert();
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("s3a-readahead-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

/// Run every case on `db` (each in its own transaction, rolled back); the results in order.
fn run(cat: &'static Catalog, db: Arc<dyn Backend>, company: &str) -> Vec<String> {
    let rt = Runtime::new(cat, db);
    let mut s = Session::new(&rt, company);
    let ci = cat.code.iter().position(|c| c.name == "S3a RA Tests").unwrap() as u32;
    let mut out = vec![];
    for name in ["DeletedByOtherVariable", "ModifiedByOtherVariable", "OwnModifyKeepsLoop"] {
        for custom in [false, true] {
            let p = cat.code[ci as usize].find(name)[0];
            let arg = al_runtime::interp::Arg::Val(Val::Bool(custom));
            match s.invoke(ProcRef { code: ci, proc: p }, vec![arg], Some(Instance::new(ci)), None, None) {
                Ok(v) => out.push(format!("{name}({custom}) = {}", v.to_text())),
                Err(Flow::Err(e)) => out.push(format!("{name}({custom}) error: {}", e.msg)),
                Err(_) => out.push(format!("{name}({custom}) unexpected flow")),
            }
            s.rollback();
        }
    }
    out
}

fn expected() -> Vec<String> {
    let mut v = vec![];
    for (name, want) in [("DeletedByOtherVariable", "ABCE"), ("ModifiedByOtherVariable", "D:changed"), ("OwnModifyKeepsLoop", "ABCDE")] {
        for custom in [false, true] {
            v.push(format!("{name}({custom}) = {want}"));
        }
    }
    v
}

#[test]
fn read_ahead_sees_other_variables_writes_memory() {
    let cat = catalog();
    assert_eq!(run(cat, Arc::new(MemBackend::new(Store::memory())), "CRONUS"), expected());
}

#[test]
fn read_ahead_sees_other_variables_writes_postgres() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("BC_PG_TEST not set: skipped");
        return;
    };
    let cat = catalog();
    let company = format!("S3ARA{}", std::process::id());
    let be = PgBackend::connect_url(&url).expect("BC_PG_TEST");
    assert_eq!(run(cat, Arc::new(be), &company), expected());
}
