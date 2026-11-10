//! I15 T, real answers instead of guessed defaults (crate::assume, docs/assumptions.md):
//! CurrentTransactionType follows BC's TransactionManager (set before the transaction's first
//! statement, a stronger type after it is an error, Commit() starts over), RoundDateTime rounds as
//! ALSystemDate.ALRoundDateTime, CurrPage.Editable / PageVar.Editable are the page's Editable
//! property until AL sets them (NavForm.Editable), NavTestExecution.IsInTestMode is true while a
//! test runs, and the remaining .NET shims are recorded per member.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP_JSON: &str = r#"{ "id": "8d2f61b0-3c4e-4a57-9b1d-6e0f2a7c5b19", "name": "T15 Defaults Test", "publisher": "bc-rust", "version": "1.0.0.0" }"#;

const APP: &str = r#"
dotnet
{
    assembly("Microsoft.Dynamics.Nav.Ncl")
    {
        type(Microsoft.Dynamics.Nav.Runtime.NavTestExecution; T15NavTestExecution) { }
    }
}

table 50770 "T15 Row"
{
    fields
    {
        field(1; "No."; Integer) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

page 50771 "T15 Read Only"
{
    PageType = Card;
    SourceTable = "T15 Row";
    Editable = false;

    procedure GetEditable(): Boolean
    begin
        exit(CurrPage.Editable);
    end;

    procedure SetEditable(Value: Boolean): Boolean
    begin
        CurrPage.Editable(Value);
        exit(CurrPage.Editable());
    end;

    procedure AssignEditable(Value: Boolean): Boolean
    begin
        CurrPage.Editable := Value;
        exit(CurrPage.Editable);
    end;
}

page 50772 "T15 Editable"
{
    PageType = Card;
    SourceTable = "T15 Row";

    procedure GetEditable(): Boolean
    begin
        exit(CurrPage.Editable);
    end;
}

codeunit 50773 "T15 Defaults"
{
    procedure TxnDefault(): Text
    begin
        exit(Format(CurrentTransactionType() = TransactionType::UpdateNoLocks));
    end;

    procedure TxnSetBeforeStatements(): Text
    var
        Before: TransactionType;
    begin
        Before := CurrentTransactionType;
        CurrentTransactionType(TransactionType::Update);
        if CurrentTransactionType() <> TransactionType::Update then
            exit('not set');
        CurrentTransactionType := TransactionType::Snapshot;
        if CurrentTransactionType <> TransactionType::Snapshot then
            exit('not assigned');
        CurrentTransactionType(Before);
        exit(Format(CurrentTransactionType() = TransactionType::UpdateNoLocks));
    end;

    procedure TxnAfterWrite(): Text
    var
        Row: Record "T15 Row";
        Result: Text;
    begin
        Commit();
        CurrentTransactionType(TransactionType::UpdateNoLocks);
        Row."No." := 1;
        Row.Insert();
        // the same or weaker after a statement: ignored
        CurrentTransactionType(TransactionType::Browse);
        Result := Format(CurrentTransactionType() = TransactionType::UpdateNoLocks);
        // stronger after a statement: BC's error (asserterror then rolls the transaction back)
        asserterror CurrentTransactionType(TransactionType::Update);
        Result += '|' + CopyStr(GetLastErrorText(), 1, 71);
        // Commit: the next statement starts a new transaction, the type may change
        Commit();
        CurrentTransactionType(TransactionType::Update);
        Result += '|' + Format(CurrentTransactionType() = TransactionType::Update);
        // Update after a statement: any request is ignored
        Row."No." := 2;
        Row.Insert();
        CurrentTransactionType(TransactionType::UpdateNoLocks);
        Result += '|' + Format(CurrentTransactionType() = TransactionType::Update);
        exit(Result);
    end;

    procedure Round(): Text
    var
        D: Date;
        DT: DateTime;
        Result: Text;
    begin
        D := 20240131D;
        DT := CreateDateTime(D, 103045.678T);
        Result := Format(RoundDateTime(DT) = CreateDateTime(D, 103046T));
        Result += '|' + Format(RoundDateTime(DT, 60000, '<') = CreateDateTime(D, 103000T));
        Result += '|' + Format(RoundDateTime(DT, 60000) = CreateDateTime(D, 103100T));
        Result += '|' + Format(RoundDateTime(DT, 3600000, '>') = CreateDateTime(D, 110000T));
        Result += '|' + Format(RoundDateTime(DT, 1000 * 60 * 60 * 24, '>') = CreateDateTime(D + 1, 000000T));
        // a half rounds up
        Result += '|' + Format(RoundDateTime(CreateDateTime(D, 103045.5T)) = CreateDateTime(D, 103046T));
        // exact values stay
        Result += '|' + Format(RoundDateTime(CreateDateTime(D, 103000T), 60000, '>') = CreateDateTime(D, 103000T));
        // an undefined DateTime stays undefined
        Result += '|' + Format(RoundDateTime(0DT) = 0DT);
        exit(Result);
    end;

    procedure PageVarEditable(): Text
    var
        ReadOnly: Page "T15 Read Only";
        Editable: Page "T15 Editable";
        Result: Text;
    begin
        Result := Format(ReadOnly.Editable()) + '|' + Format(Editable.Editable());
        Editable.Editable(false);
        ReadOnly.Editable := true;
        Result += '|' + Format(ReadOnly.Editable()) + '|' + Format(Editable.Editable());
        exit(Result);
    end;

    procedure InTestMode(): Text
    var
        NavTestExecution: DotNet T15NavTestExecution;
    begin
        exit(Format(NavTestExecution.IsInTestMode()));
    end;

    procedure Sids(): Text
    begin
        exit('[' + Sid() + '|' + Sid('CONTOSO\T15USER') + ']');
    end;

    procedure UnknownMember(): Text
    var
        NavTestExecution: DotNet T15NavTestExecution;
    begin
        NavTestExecution.T15NoSuchMember();
        exit('done');
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("t15-defaults-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call_in(s: &mut Session, cat: &Catalog, object: &str, proc: &str, args: Vec<al_runtime::interp::Arg>) -> String {
    let ci = cat.code.iter().position(|c| c.name == object).unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, args, Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    call_in(s, cat, "T15 Defaults", proc, vec![])
}

#[test]
fn real_answers_instead_of_guessed_defaults() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    al_runtime::assume::reset();

    // CurrentTransactionType (Ncl TransactionManager)
    assert_eq!(call(&mut s, cat, "TxnDefault"), "Yes");
    assert_eq!(call(&mut s, cat, "TxnSetBeforeStatements"), "Yes");
    assert_eq!(
        call(&mut s, cat, "TxnAfterWrite"),
        "Yes|The transaction type can only be set before the transaction reads or wr|Yes|Yes"
    );
    // the host ends the unit of work: the next one starts as UpdateNoLocks
    s.commit().unwrap();
    assert_eq!(call(&mut s, cat, "TxnDefault"), "Yes");

    // RoundDateTime (ALSystemDate.ALRoundDateTime)
    assert_eq!(call(&mut s, cat, "Round"), "Yes|Yes|Yes|Yes|Yes|Yes|Yes|Yes");

    // CurrPage.Editable / PageVar.Editable (NavForm.Editable)
    assert_eq!(call_in(&mut s, cat, "T15 Read Only", "GetEditable", vec![]), "No");
    assert_eq!(call_in(&mut s, cat, "T15 Editable", "GetEditable", vec![]), "Yes");
    assert_eq!(call_in(&mut s, cat, "T15 Read Only", "SetEditable", vec![al_runtime::interp::Arg::Val(al_runtime::Val::Bool(true))]), "Yes");
    assert_eq!(call_in(&mut s, cat, "T15 Read Only", "AssignEditable", vec![al_runtime::interp::Arg::Val(al_runtime::Val::Bool(true))]), "Yes");
    assert_eq!(call(&mut s, cat, "PageVarEditable"), "No|Yes|Yes|No");

    // Sid: the session user's Windows security id (none here), '' for any name (no Windows accounts)
    assert_eq!(call(&mut s, cat, "Sids"), "[|]");

    // none of the above is a guessed default any more
    let r = al_runtime::assume::take();
    assert!(r.assumed.is_empty(), "{r:?}");

    // NavTestExecution.IsInTestMode: false outside a test, true while one runs
    assert_eq!(call(&mut s, cat, "InTestMode"), "No");
    let ci = cat.code.iter().position(|c| c.name == "T15 Defaults").unwrap() as u32;
    let p = cat.code[ci as usize].find("InTestMode")[0];
    s.test = Some(al_runtime::testing::TestCtx::new(cat, Instance::new(ci), p).unwrap());
    assert_eq!(call(&mut s, cat, "InTestMode"), "Yes");
    s.test = None;
    assert!(al_runtime::assume::take().assumed.is_empty());

    // an unknown .NET member is still a guessed default, recorded with its type and member
    assert_eq!(call(&mut s, cat, "UnknownMember"), "done");
    let r = al_runtime::assume::take();
    assert_eq!(r.assumed, vec![("DotNet.Neutral", 1), ("DotNet.Neutral:T15NavTestExecution.T15NoSuchMember", 1)]);
    assert!(r.flagging());
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("t15-defaults-test-{}", std::process::id())));
}
