//! I15 T: SessionInformation.Callstack in BC's format [src:A29.1] instead of '' (a guessed
//! default): innermost frame first, each
//! followed by '\', `<Object>(<ObjectType> <Id>).<Method>[(Trigger)] line <N> - <App> by
//! <Publisher> version <Version>`, N relative to the procedure's signature line, field triggers
//! named `<Field> - <Trigger>`, names quoted unless they are plain identifiers.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP_JSON: &str = r#"{ "id": "c41d7e09-5b3a-4f62-8e17-2a9f0b6d3c58", "name": "T15 Callstack Test", "publisher": "bc-rust", "version": "1.2.3.4" }"#;

const APP: &str = r#"
table 50776 "T15 Stack Row"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            trigger OnValidate()
            var
                Stack: Codeunit T15Stack;
            begin
                Rec.Stack := Stack.Get();
            end;
        }
        field(2; Stack; Text[2048]) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

codeunit 50775 T15Stack
{
    procedure Outer(): Text
    var
        T: Text;
    begin
        T := Inner();
        exit(T);
    end;

    local procedure Inner(): Text
    begin
        exit(SessionInformation.Callstack());
    end;

    procedure InLoop(): Text
    var
        I: Integer;
        T: Text;
    begin
        while I < 1 do begin
            I += 1;
            if StrLen(Inner()) > 0 then
                T := Inner();
        end;
        exit(T);
    end;

    procedure FromTrigger(): Text
    var
        Row: Record "T15 Stack Row";
    begin
        Row.Validate("No.", 'A');
        exit(Row.Stack);
    end;

    procedure Get(): Text
    begin
        exit(SessionInformation.Callstack);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("t15-callstack-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "T15Stack").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn callstack_in_bc_format() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    al_runtime::assume::reset();
    let app = " - T15 Callstack Test by bc-rust version 1.2.3.4";

    assert_eq!(call(&mut s, cat, "Outer"), format!("T15Stack(CodeUnit 50775).Inner line 2{app}\\T15Stack(CodeUnit 50775).Outer line 4{app}\\"));
    // a call in a loop condition and in the loop body: the line of the statement that calls
    assert_eq!(call(&mut s, cat, "InLoop"), format!("T15Stack(CodeUnit 50775).Inner line 2{app}\\T15Stack(CodeUnit 50775).InLoop line 8{app}\\"));
    // a field trigger: "<Field> - <Trigger>"(Trigger), then the Validate caller
    assert_eq!(
        call(&mut s, cat, "FromTrigger"),
        format!(
            "T15Stack(CodeUnit 50775).Get line 2{app}\\\"T15 Stack Row\"(Table 50776).\"No. - OnValidate\"(Trigger) line 4{app}\\T15Stack(CodeUnit 50775).FromTrigger line 4{app}\\"
        )
    );
    // the frames of finished calls are gone: the stack is empty again
    assert!(s.call_stack.is_empty() && s.frame_lines.is_empty());
    assert!(al_runtime::assume::take().assumed.is_empty());
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("t15-callstack-test-{}", std::process::id())));
}
