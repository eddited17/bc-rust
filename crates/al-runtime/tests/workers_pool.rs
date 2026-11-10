//! The background pool (al_runtime::workers, docs/job-queue.md) on the embedded engine: scheduled
//! tasks run in sessions of their own, work a task committed survives its later error, the failure
//! codeunit gets the error text, finished tasks are removed, StartSession runs at once in its own
//! session, StopSession ends a running session. Its own test binary: the pool is process-wide.

use al_model::{Catalog, ProcRef};
use al_runtime::{workers, Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;
use std::time::{Duration, Instant};

const APP: &str = r#"
table 50800 "JQT Item"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Integer) { }
        field(3; Flag; Boolean) { }
        field(4; Note; Text[50]) { }
        field(5; TaskId; Guid) { }
    }
    keys { key(PK; "No.") { } }
}

table 2000000175 "Scheduled Task"
{
    DataPerCompany = false;
    fields
    {
        field(1; ID; Guid) { }
        field(3; "User Name"; Text[50]) { }
        field(10; Company; Text[30]) { }
        field(11; "Is Ready"; Boolean) { }
        field(12; "Not Before"; DateTime) { }
        field(20; "Run Codeunit"; Integer) { }
        field(21; "Failure Codeunit"; Integer) { }
        field(22; "Record"; RecordID) { }
        field(23; "Timeout"; Duration) { }
    }
    keys { key(PK; ID) { } }
}

codeunit 50800 "JQT Task"
{
    TableNo = "JQT Item";

    trigger OnRun()
    begin
        Rec.Amount += 1;
        Rec.Note := Format(SessionId() <> 1);
        Rec.Modify();
        Commit();
        if Rec.Flag then
            Error('task failed');
    end;
}

codeunit 50801 "JQT Failure"
{
    TableNo = "JQT Item";

    trigger OnRun()
    var
        F: Record "JQT Item";
    begin
        F."No." := 'FAILED-' + Rec."No.";
        F.Amount := StrLen(GetLastErrorText());
        F.Insert();
    end;
}

codeunit 50802 "JQT Loop"
{
    TableNo = "JQT Item";

    trigger OnRun()
    begin
        while true do
            Rec.Amount += 1;
    end;
}

codeunit 50810 "JQT Tests"
{
    procedure Schedule(): Text
    var
        Item: Record "JQT Item";
        i: Integer;
    begin
        for i := 1 to 6 do begin
            Item.Init();
            Item."No." := 'T' + Format(i);
            Item.Flag := i = 6;
            Item.Insert();
            Item.TaskId := TaskScheduler.CreateTask(Codeunit::"JQT Task", Codeunit::"JQT Failure", true, CompanyName(), 0DT, Item.RecordId);
            Item.Modify();
        end;
        exit(Format(TaskScheduler.CanCreateTask()));
    end;

    procedure State(): Text
    var
        Item: Record "JQT Item";
        R: Text;
    begin
        Item.SetFilter("No.", 'T*');
        if Item.FindSet() then
            repeat
                R += Format(Item.Amount) + Item.Note + Format(TaskScheduler.TaskExists(Item.TaskId)) + ',';
            until Item.Next() = 0;
        if Item.Get('FAILED-T6') then
            R += 'F' + Format(Item.Amount);
        exit(R);
    end;

    procedure StartLoop(): Integer
    var
        Item: Record "JQT Item";
        Id: Integer;
    begin
        Item."No." := 'L1';
        Item.Insert();
        Commit();
        if not Session.StartSession(Id, Codeunit::"JQT Loop", CompanyName(), Item) then
            exit(-1);
        exit(Id);
    end;

    procedure ScheduleTimeout(): Text
    var
        Item: Record "JQT Item";
    begin
        Item."No." := 'TO1';
        Item.Insert();
        TaskScheduler.CreateTask(Codeunit::"JQT Loop", Codeunit::"JQT Failure", true, CompanyName(), 0DT, Item.RecordId, 300);
        exit('ok');
    end;

    procedure TimedOut(): Text
    var
        Item: Record "JQT Item";
    begin
        if Item.Get('FAILED-TO1') then
            exit(Format(Item.Amount));
        exit('');
    end;

    procedure Active(Id: Integer): Boolean
    begin
        exit(IsSessionActive(Id));
    end;

    procedure Stop(Id: Integer)
    begin
        StopSession(Id);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("jq12-pool-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str, args: Vec<al_runtime::Val>) -> al_runtime::Val {
    let ci = cat.code.iter().position(|c| c.name == "JQT Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    let args = args.into_iter().map(al_runtime::interp::Arg::Val).collect();
    match s.invoke(ProcRef { code: ci, proc: p }, args, Some(Instance::new(ci)), None, None) {
        Ok(v) => v,
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

/// Poll `f` until it returns true or `secs` pass.
fn wait(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn pool_runs_tasks_and_sessions() {
    let cat = catalog();
    let rt: &'static Runtime = Box::leak(Box::new(Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())))));
    let mut cfg = workers::Config::with_workers(3, "CRONUS");
    cfg.poll_ms = 20;
    workers::start(rt, cfg);

    let mut s = Session::new(rt, "CRONUS");
    assert_eq!(call(&mut s, cat, "Schedule", vec![]).to_text(), "Yes", "CanCreateTask with the pool");
    s.commit().unwrap();

    // every task ran once in a background session (SessionId() <> 1) and is gone; T6's increment
    // was committed before its error and survives, and its failure codeunit saw the error text
    let want = "1YesNo,1YesNo,1YesNo,1YesNo,1YesNo,1YesNo,F11";
    let mut last = String::new();
    let done = wait(20, || {
        let mut r = Session::new(rt, "CRONUS");
        last = call(&mut r, cat, "State", vec![]).to_text();
        last == want
    });
    assert!(done, "tasks: {last}");
    assert_eq!(workers::pool().unwrap().stats.tasks_failed.load(std::sync::atomic::Ordering::Relaxed), 1);

    // StartSession returns at once with a running session; StopSession ends it
    let id = call(&mut s, cat, "StartLoop", vec![]).as_i64();
    assert!(id > 1, "session id {id}");
    assert!(call(&mut s, cat, "Active", vec![al_runtime::Val::Int(id as i32)]).as_bool(), "running");
    assert!(!call(&mut s, cat, "Active", vec![al_runtime::Val::Int(999_999)]).as_bool());
    call(&mut s, cat, "Stop", vec![al_runtime::Val::Int(id as i32)]);
    assert!(wait(10, || !call(&mut s, cat, "Active", vec![al_runtime::Val::Int(id as i32)]).as_bool()), "stopped");
    assert_eq!(workers::pool().unwrap().stats.sessions_failed.load(std::sync::atomic::Ordering::Relaxed), 1);

    // CreateTask's Timeout (300 ms): the looping task is stopped and its failure codeunit gets the error
    assert_eq!(call(&mut s, cat, "ScheduleTimeout", vec![]).to_text(), "ok");
    s.commit().unwrap();
    let want = workers::CANCEL_MESSAGE.len().to_string();
    let mut got = String::new();
    let stopped = wait(15, || {
        let mut r = Session::new(rt, "CRONUS");
        got = call(&mut r, cat, "TimedOut", vec![]).to_text();
        got == want
    });
    assert!(stopped, "timeout: failure codeunit saw {got:?}, want {want}");
}
