//! Record.Insert and SystemId, as BC does it (NavRecord.ALInsert -> RecordImplementation.PopulateSystemdId(
//! onlyOverwriteIfBlank: insertWithSystemId)): a record that is not temporary gets a new SystemId on every Insert
//! unless Insert(RunTrigger, true) asks to keep the one it has, so Copy + Insert never duplicates the source's
//! SystemId (MCP Config's CopyConfiguration did, and GetBySystemId then found either row). Temporary records keep
//! theirs.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50796 "Stab12 SysId Item"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

codeunit 50797 "Stab12 SysId Tests"
{
    procedure CopyInsert(): Text
    var
        A: Record "Stab12 SysId Item";
        B: Record "Stab12 SysId Item";
        Found: Record "Stab12 SysId Item";
    begin
        A."No." := 'A';
        A.Insert();
        B.Copy(A);
        B."No." := 'B';
        B.Insert();
        if B.SystemId = A.SystemId then
            exit('duplicate');
        if IsNullGuid(B.SystemId) then
            exit('blank');
        Found.GetBySystemId(B.SystemId);
        exit(Found."No.");
    end;

    procedure KeepWithSystemId(): Text
    var
        A: Record "Stab12 SysId Item";
        Id: Guid;
    begin
        Id := '{0F1E2D3C-4B5A-4978-8695-A4B3C2D1E0F9}';
        A."No." := 'K';
        A.SystemId := Id;
        A.Insert(false, true);
        if A.SystemId <> Id then
            exit('changed');
        A.Get('K');
        if A.SystemId <> Id then
            exit('not stored');
        exit('kept');
    end;

    procedure PlainInsertReplaces(): Text
    var
        A: Record "Stab12 SysId Item";
        Id: Guid;
    begin
        Id := '{1F1E2D3C-4B5A-4978-8695-A4B3C2D1E0F9}';
        A."No." := 'P';
        A.SystemId := Id;
        A.Insert();
        if A.SystemId = Id then
            exit('kept');
        exit('new');
    end;

    procedure TemporaryKeeps(): Text
    var
        T: Record "Stab12 SysId Item" temporary;
        Id: Guid;
    begin
        Id := '{2F1E2D3C-4B5A-4978-8695-A4B3C2D1E0F9}';
        T."No." := 'T';
        T.SystemId := Id;
        T.Insert();
        if T.SystemId = Id then
            exit('kept');
        exit('new');
    end;
}
"#;

fn catalog() -> &'static Catalog {
    // one fixed directory, the file replaced atomically (nothing is deleted)
    let dir = std::env::temp_dir().join("stab12-systemid-test");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(format!("App.al.{}.{:?}.part", std::process::id(), std::thread::current().id()));
    std::fs::write(&tmp, APP).unwrap();
    std::fs::rename(&tmp, dir.join("App.al")).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "Stab12 SysId Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn insert_gives_a_new_system_id_unless_asked_to_keep_it() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    assert_eq!(call(&mut s, cat, "CopyInsert"), "B");
    assert_eq!(call(&mut s, cat, "KeepWithSystemId"), "kept");
    assert_eq!(call(&mut s, cat, "PlainInsertReplaces"), "new");
    assert_eq!(call(&mut s, cat, "TemporaryKeeps"), "kept");
}
