//! Record.Get(RecordId) with the RecordId of another table finds nothing (BC
//! RecordImplementation.CheckRecordIdValidity). It used to copy the other table's key values into
//! the record's key fields; on PostgreSQL that sent a Code value for an Option column and failed
//! ("error serializing parameter 0": WF Demo General Journal Batch, CRM Notes Synch Job's
//! `SalesHeader.Get(RecordLink."Record ID")` for a link on a journal batch). Memory always;
//! PostgreSQL with `BC_PG_TEST=<scratch connection>` (a company schema `c_s3agr<pid>` stays there).

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{Backend, MemBackend, PgBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50980 "S3a Doc"
{
    fields
    {
        field(1; "Document Type"; Option) { OptionMembers = Quote,Order; }
        field(2; "No."; Code[20]) { }
    }
    keys
    {
        key(PK; "Document Type", "No.") { Clustered = true; }
    }
}

table 50981 "S3a Batch"
{
    fields
    {
        field(1; "Template"; Code[10]) { }
        field(2; Name; Code[10]) { }
    }
    keys
    {
        key(PK; "Template", Name) { Clustered = true; }
    }
}

codeunit 50982 "S3a GetRecId"
{
    procedure Probe(): Text
    var
        Doc: Record "S3a Doc";
        Batch: Record "S3a Batch";
        Result: Text;
    begin
        Doc."Document Type" := Doc."Document Type"::Order;
        Doc."No." := '1001';
        Doc.Insert();
        Batch.Template := 'GENERAL';
        Batch.Name := 'DEFAULT';
        Batch.Insert();
        if Doc.Get(Batch.RecordId) then
            Result := 'found'
        else
            Result := 'not found';
        // the record keeps its values
        Result += '|' + Doc."No.";
        if Doc.Get(Doc.RecordId) then
            Result += '|own found';
        exit(Result);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("s3a-getrecid-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn run(cat: &'static Catalog, db: Arc<dyn Backend>, company: &str) -> String {
    let rt = Runtime::new(cat, db);
    let mut s = Session::new(&rt, company);
    let ci = cat.code.iter().position(|c| c.name == "S3a GetRecId").unwrap() as u32;
    let p = cat.code[ci as usize].find("Probe")[0];
    let out = match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => format!("error: {}", e.msg),
        Err(_) => "unexpected flow".into(),
    };
    s.rollback();
    out
}

const EXPECTED: &str = "not found|1001|own found";

#[test]
fn get_with_another_tables_record_id_memory() {
    assert_eq!(run(catalog(), Arc::new(MemBackend::new(Store::memory())), "CRONUS"), EXPECTED);
}

#[test]
fn get_with_another_tables_record_id_postgres() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("BC_PG_TEST not set: skipped");
        return;
    };
    let be = PgBackend::connect_url(&url).expect("BC_PG_TEST");
    assert_eq!(run(catalog(), Arc::new(be), &format!("S3AGR{}", std::process::id())), EXPECTED);
}
