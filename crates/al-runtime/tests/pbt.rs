//! Page background tasks (crate::pbt): EnqueueBackgroundTask with its child session, the
//! Completed / Error triggers (page and page extension), error levels, cancel, the timeout,
//! read-only child sessions and TestPage.RunPageBackgroundTask.

use al_model::{Catalog, ProcRef};
use al_runtime::pbt::{self, ChildSpec, ErrorLevel, Outcome};
use al_runtime::{Flow, Instance, Runtime, Session, TestCtx, Val};
use bc_store::{MemBackend, Store};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const APP: &str = r#"
table 50850 "PBT Cue"
{
    fields
    {
        field(1; "Key"; Code[10]) { }
        field(2; "Sum"; Decimal) { }
        field(3; "Err"; Text[250]) { }
        field(4; "Ext Seen"; Boolean) { }
        field(5; "Tasks Done"; Integer) { }
    }
    keys
    {
        key(PK; "Key") { Clustered = true; }
    }
}

codeunit 50850 "PBT Calc"
{
    trigger OnRun()
    var
        Params: Dictionary of [Text, Text];
        Results: Dictionary of [Text, Text];
        Cue: Record "PBT Cue";
        A: Decimal;
        B: Decimal;
        I: Integer;
    begin
        Params := Page.GetBackgroundParameters();
        if Params.ContainsKey('fail') then
            Error('boom %1', Params.Get('fail'));
        if Params.ContainsKey('write') then begin
            Cue."Key" := 'W';
            Cue.Insert();
        end;
        if Params.ContainsKey('loop') then
            while true do
                I += 1;
        Evaluate(A, Params.Get('a'));
        Evaluate(B, Params.Get('b'));
        Results.Add('sum', Format(A + B));
        Results.Add('workdate', Format(WorkDate(), 0, 9));
        Page.SetBackgroundTaskResult(Results);
    end;
}

page 50850 "PBT Card"
{
    PageType = CardPart;
    SourceTable = "PBT Cue";

    layout
    {
        area(Content)
        {
            field("Sum"; Rec."Sum") { }
            field("Err"; Rec."Err") { }
            field("Ext Seen"; Rec."Ext Seen") { }
            field("Tasks Done"; Rec."Tasks Done") { }
        }
    }

    trigger OnOpenPage()
    begin
        if not Rec.Get() then begin
            Rec.Init();
            Rec.Insert();
        end;
        Start(Mode);
    end;

    var
        Mode: Text;
        TaskId: Integer;
        LastTaskId: Integer;
        HandleErrors: Boolean;
        CompletedIds: Text;

    procedure SetMode(NewMode: Text; NewHandle: Boolean)
    begin
        Mode := NewMode;
        HandleErrors := NewHandle;
    end;

    procedure Start(StartMode: Text)
    var
        Input: Dictionary of [Text, Text];
    begin
        case StartMode of
            '', 'sum':
                begin
                    Input.Add('a', '2');
                    Input.Add('b', '3.5');
                    CurrPage.EnqueueBackgroundTask(TaskId, Codeunit::"PBT Calc", Input, 1000);
                end;
            'fail':
                begin
                    Input.Add('fail', 'x');
                    CurrPage.EnqueueBackgroundTask(TaskId, Codeunit::"PBT Calc", Input);
                end;
            'fail-ignore':
                begin
                    Input.Add('fail', 'y');
                    CurrPage.EnqueueBackgroundTask(TaskId, Codeunit::"PBT Calc", Input, 1000, PageBackgroundTaskErrorLevel::Ignore);
                end;
            'write':
                begin
                    Input.Add('write', '1');
                    CurrPage.EnqueueBackgroundTask(TaskId, Codeunit::"PBT Calc", Input);
                end;
        end;
        LastTaskId := TaskId;
    end;

    procedure Cancel(Id: Integer): Boolean
    begin
        exit(CurrPage.CancelBackgroundTask(Id));
    end;

    procedure GetTaskId(): Integer
    begin
        exit(LastTaskId);
    end;

    procedure GetCompleted(): Text
    begin
        exit(CompletedIds);
    end;

    trigger OnPageBackgroundTaskCompleted(TaskId: Integer; Results: Dictionary of [Text, Text])
    begin
        Evaluate(Rec."Sum", Results.Get('sum'));
        Rec."Tasks Done" += 1;
        Rec.Modify();
        CompletedIds += Format(TaskId) + ';';
    end;

    trigger OnPageBackgroundTaskError(TaskId: Integer; ErrorCode: Text; ErrorText: Text; ErrorCallStack: Text; var IsHandled: Boolean)
    begin
        Rec."Err" := CopyStr(ErrorCode + '|' + ErrorText, 1, 250);
        Rec.Modify();
        IsHandled := HandleErrors;
    end;
}

pageextension 50851 "PBT Card Ext" extends "PBT Card"
{
    trigger OnPageBackgroundTaskCompleted(TaskId: Integer; Results: Dictionary of [Text, Text])
    begin
        Rec."Ext Seen" := true;
        Rec.Modify();
    end;
}

codeunit 50851 "PBT Tests"
{
    Subtype = Test;

    [Test]
    procedure OpenRunsTaskAtOnce()
    var
        Card: TestPage "PBT Card";
    begin
        // no host (tests): the task and its completion trigger run inside EnqueueBackgroundTask
        Card.OpenView();
        if Card."Sum".AsDecimal() <> 5.5 then
            Error('Sum = %1', Card."Sum".Value());
        if not Card."Ext Seen".AsBoolean() then
            Error('the page extension''s OnPageBackgroundTaskCompleted did not run');
        Card.Close();
    end;

    [Test]
    procedure RunPageBackgroundTaskReturnsResults()
    var
        Card: TestPage "PBT Card";
        Input: Dictionary of [Text, Text];
        Results: Dictionary of [Text, Text];
    begin
        Card.OpenView();
        Input.Add('a', '40');
        Input.Add('b', '2');
        Results := Card.RunPageBackgroundTask(Codeunit::"PBT Calc", Input);
        if Results.Get('sum') <> '42' then
            Error('sum = %1', Results.Get('sum'));
        Card.Close();
    end;

    [Test]
    procedure RunPageBackgroundTaskRaisesErrors()
    var
        Card: TestPage "PBT Card";
        Input: Dictionary of [Text, Text];
    begin
        Card.OpenView();
        Input.Add('fail', 'z');
        asserterror Card.RunPageBackgroundTask(Codeunit::"PBT Calc", Input);
        if GetLastErrorText() <> 'boom z' then
            Error('error = %1', GetLastErrorText());
        Card.Close();
    end;

    [Test]
    procedure ParametersOutsideChildSession()
    var
        Params: Dictionary of [Text, Text];
    begin
        asserterror Params := Page.GetBackgroundParameters();
        if StrPos(GetLastErrorText(), 'can only run inside a page background task') = 0 then
            Error('error = %1', GetLastErrorText());
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join("pbt-test");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(format!("App.al.{}.{:?}.part", std::process::id(), std::thread::current().id()));
    std::fs::write(&tmp, APP).unwrap();
    std::fs::rename(&tmp, dir.join("App.al")).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn runtime() -> &'static Runtime {
    let cat = catalog();
    Box::leak(Box::new(Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())))))
}

/// `r` or a panic with the AL error.
fn ok<T>(r: Result<T, Flow>) -> T {
    match r {
        Ok(v) => v,
        Err(Flow::Err(e)) => panic!("{} {:?}", e.msg, e.stack),
        Err(_) => panic!("unexpected flow"),
    }
}

fn run_test(s: &mut Session, cat: &'static Catalog, name: &str) {
    let ci = cat.code.iter().position(|c| c.name == "PBT Tests").unwrap() as u32;
    let pi = cat.code[ci as usize].find(name)[0];
    let inst = Instance::new(ci);
    s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
    let r = s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None);
    s.test = None;
    match r {
        Ok(_) => {}
        Err(Flow::Err(e)) => panic!("{name}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{name}: unexpected flow"),
    }
}

#[test]
fn tests_run_tasks_at_once() {
    let rt = runtime();
    let mut s = Session::new(rt, "CRONUS");
    for t in ["OpenRunsTaskAtOnce", "RunPageBackgroundTaskReturnsResults", "RunPageBackgroundTaskRaisesErrors", "ParametersOutsideChildSession"] {
        run_test(&mut s, rt.cat, t);
    }
}

/// The page (and its extension instance) as a host such as bc-nst holds it.
struct Page {
    page: std::rc::Rc<Instance>,
    ext: std::rc::Rc<Instance>,
}

fn open_page(s: &mut Session<'static>, mode: &str, handle: bool) -> Page {
    let cat = s.cat;
    let pd = &cat.pages[&50850];
    let page = Instance::new(pd.code);
    let rec = s.new_record(50850, false);
    *page.rec.borrow_mut() = Some(rec.clone());
    let ext = Instance::new(pd.ext_code[0]);
    *ext.rec.borrow_mut() = Some(rec);
    let set_mode = cat.code[pd.code as usize].find("SetMode")[0];
    ok(s.invoke(ProcRef { code: pd.code, proc: set_mode }, vec![al_runtime::interp::Arg::Val(Val::Text(mode.into())), al_runtime::interp::Arg::Val(Val::Bool(handle))], Some(page.clone()), page.rec.borrow().clone(), None));
    let open = cat.code[pd.code as usize].trigger("OnOpenPage").unwrap();
    if let Err(Flow::Err(e)) = s.invoke(ProcRef { code: pd.code, proc: open }, vec![], Some(page.clone()), page.rec.borrow().clone(), None) {
        panic!("OnOpenPage: {}", e.msg);
    }
    Page { page, ext }
}

fn page_call(s: &mut Session<'static>, p: &Page, name: &str, args: Vec<Val>) -> Val {
    let code = p.page.code;
    let pr = s.cat.code[code as usize].find(name)[0];
    let args = args.into_iter().map(al_runtime::interp::Arg::Val).collect();
    match s.invoke(ProcRef { code, proc: pr }, args, Some(p.page.clone()), p.page.rec.borrow().clone(), None) {
        Ok(v) => v,
        Err(Flow::Err(e)) => panic!("{name}: {}", e.msg),
        Err(_) => panic!("{name}: unexpected flow"),
    }
}

fn field(p: &Page, name: &str) -> String {
    let rec = p.page.rec.borrow().clone().unwrap();
    let r = rec.borrow();
    let fi = r.tdef.field(name).unwrap();
    format!("{:?}", r.row[fi])
}

/// Run a queued task in a child session (as a host does) and wait for its outcome.
fn run_child(rt: &'static Runtime, s: &Session<'static>, t: &pbt::Task) -> (Outcome, Duration) {
    let (tx, rx) = mpsc::channel();
    let spec = ChildSpec { id: t.id, company: s.company.clone(), user: s.user_id.clone(), work_date: s.work_date, language: 1033, codeunit: t.codeunit, params: t.params.clone(), timeout_ms: t.timeout_ms, cancel: t.cancel.clone() };
    let t0 = Instant::now();
    pbt::spawn_child(rt, spec, Box::new(move |o| tx.send(o).unwrap())).unwrap();
    let o = rx.recv_timeout(Duration::from_secs(20)).expect("child outcome");
    (o, t0.elapsed())
}

#[test]
fn host_runs_child_sessions_and_completion() {
    let rt = runtime();
    let mut s = Session::new(rt, "CRONUS");
    s.sys.pbt.host = true;
    let p = open_page(&mut s, "sum", false);
    let _ = s.commit();
    assert_eq!(s.sys.pbt.queued.len(), 1, "OnOpenPage enqueued one task");
    let t = s.sys.pbt.queued.remove(0);
    assert_eq!(t.id, page_call(&mut s, &p, "GetTaskId", vec![]).as_i64() as i32, "TaskId var holds the id");
    assert_eq!(t.timeout_ms, Some(1000));
    assert_eq!(t.level, ErrorLevel::Error);
    assert!(std::rc::Rc::ptr_eq(&t.page, &p.page));
    let (o, _) = run_child(rt, &s, &t);
    match &o {
        Outcome::Done(r) => {
            assert_eq!(r[0], ("sum".to_string(), "5.5".to_string()));
            assert_eq!(r[1].0, "workdate");
        }
        other => panic!("{other:?}"),
    }
    // the completion trigger runs on the page and its extension
    let insts = vec![p.page.clone(), p.ext.clone()];
    let rec = p.page.rec.borrow().clone();
    let done = ok(s.pbt_complete(&insts, rec.as_ref(), t.bookmark.as_deref(), t.id, o, t.level));
    assert!(done);
    assert!(field(&p, "Sum").contains("5.5"), "{}", field(&p, "Sum"));
    assert!(field(&p, "Ext Seen").contains("true"), "{}", field(&p, "Ext Seen"));
    assert_eq!(page_call(&mut s, &p, "GetCompleted", vec![]).to_text(), format!("{};", t.id));
    // cancel: true for a live task, false afterwards and for unknown ids
    page_call(&mut s, &p, "Start", vec![Val::Text("sum".into())]);
    let t2 = s.sys.pbt.queued.remove(0);
    assert_eq!(page_call(&mut s, &p, "Cancel", vec![Val::Int(t2.id)]).as_bool(), true);
    assert!(t2.cancel.load(Ordering::Relaxed), "the cancel flag is set");
    assert_eq!(page_call(&mut s, &p, "Cancel", vec![Val::Int(t2.id)]).as_bool(), false);
    assert_eq!(page_call(&mut s, &p, "Cancel", vec![Val::Int(987654)]).as_bool(), false);
    // record changed since the task was enqueued: no completion trigger
    let moved = vec![bc_store::Value::Text("OTHER".into())];
    let before = page_call(&mut s, &p, "GetCompleted", vec![]).to_text();
    let ran = ok(s.pbt_complete(&insts, rec.as_ref(), Some(&moved), 4242, Outcome::Done(vec![("sum".into(), "1".into())]), ErrorLevel::Error));
    assert!(!ran);
    assert_eq!(page_call(&mut s, &p, "GetCompleted", vec![]).to_text(), before);
}

#[test]
fn host_error_levels_and_handling() {
    let rt = runtime();
    let mut s = Session::new(rt, "CRONUS");
    s.sys.pbt.host = true;
    // not handled, level Error: the child's error comes back from the completion
    let p = open_page(&mut s, "fail", false);
    let _ = s.commit();
    let t = s.sys.pbt.queued.remove(0);
    assert_eq!(t.timeout_ms, Some(pbt::DEFAULT_TIMEOUT_MS as u64), "no Timeout argument: the default");
    let (o, _) = run_child(rt, &s, &t);
    match &o {
        Outcome::Failed { text, .. } => assert_eq!(text, "boom x"),
        other => panic!("{other:?}"),
    }
    let insts = vec![p.page.clone(), p.ext.clone()];
    let rec = p.page.rec.borrow().clone();
    match s.pbt_complete(&insts, rec.as_ref(), t.bookmark.as_deref(), t.id, o.clone(), t.level) {
        Err(Flow::Err(e)) => assert_eq!(e.msg, "boom x"),
        _ => panic!("an unhandled error is raised"),
    }
    assert!(field(&p, "Err").contains("boom x"), "the error trigger ran: {}", field(&p, "Err"));
    // handled: no error
    page_call(&mut s, &p, "SetMode", vec![Val::Text("fail".into()), Val::Bool(true)]);
    assert!(ok(s.pbt_complete(&insts, rec.as_ref(), t.bookmark.as_deref(), t.id, o.clone(), t.level)));
    // level Ignore: no error either
    page_call(&mut s, &p, "SetMode", vec![Val::Text("fail-ignore".into()), Val::Bool(false)]);
    page_call(&mut s, &p, "Start", vec![Val::Text("fail-ignore".into())]);
    let t3 = s.sys.pbt.queued.remove(0);
    assert_eq!(t3.level, ErrorLevel::Ignore);
    let (o3, _) = run_child(rt, &s, &t3);
    assert!(ok(s.pbt_complete(&insts, rec.as_ref(), t3.bookmark.as_deref(), t3.id, o3, t3.level)));
    // the child session is read-only
    page_call(&mut s, &p, "Start", vec![Val::Text("write".into())]);
    let t4 = s.sys.pbt.queued.remove(0);
    let (o4, _) = run_child(rt, &s, &t4);
    match &o4 {
        Outcome::Failed { text, .. } => assert_eq!(text, pbt::READ_ONLY_TEXT),
        other => panic!("{other:?}"),
    }
}

#[test]
fn host_timeout_stops_the_child() {
    let rt = runtime();
    let s = Session::new(rt, "CRONUS");
    let cancel = Arc::new(AtomicBool::new(false));
    let t = pbt::Task {
        id: pbt::next_task_id(),
        page: Instance::new(0),
        codeunit: 50850,
        params: vec![("loop".into(), "1".into())],
        timeout_ms: pbt::effective_timeout(300),
        level: ErrorLevel::Error,
        bookmark: None,
        cancel: cancel.clone(),
    };
    let (o, took) = run_child(rt, &s, &t);
    assert_eq!(o, Outcome::timeout());
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(cancel.load(Ordering::Relaxed), "the child is told to stop");
    assert_eq!(pbt::effective_timeout(0), None);
    assert_eq!(pbt::effective_timeout(10_000_000), Some(pbt::MAX_TIMEOUT_MS as u64));
}
