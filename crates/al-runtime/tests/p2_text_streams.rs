//! Text reading rules P2 matched to BC (Ncl ALStream / NavFile): InStream.ReadText reads to a line
//! feed or a zero byte, drops every CR and returns the byte count of what it kept (an empty line
//! gives 0); File.TextMode set before Open / Create applies to the opened file and survives Close,
//! and a text-mode File.Read reads one line without its CR LF.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50950 "P2 Blob"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Data; Blob) { }
    }
    keys { key(PK; "No.") { } }
}

dotnet
{
    assembly("System.Drawing")
    {
        type("System.Drawing.Bitmap"; "P2 Bitmap") { }
        type("System.Drawing.Imaging.ImageFormat"; "P2 ImageFormat") { }
    }
}

codeunit 50951 "P2 Tests"
{
    // .NET writing through an InStream reaches the Blob it was created on (Bitmap.Save(InStr, ...))
    procedure BitmapSaveToInStream(): Text
    var
        B: Record "P2 Blob";
        Bmp: DotNet "P2 Bitmap";
        Fmt: DotNet "P2 ImageFormat";
        InS: InStream;
        InS2: InStream;
        Head: Text;
    begin
        B.Data.CreateInStream(InS);
        Bmp := Bmp.Bitmap(1, 1);
        Bmp.Save(InS, Fmt.Png);
        B.Data.CreateInStream(InS2);
        InS2.ReadText(Head, 4);
        exit(Format(B.Data.HasValue()) + '|' + CopyStr(Head, 2, 3));
    end;

    procedure ReadTextLines(): Text
    var
        B: Record "P2 Blob";
        OutS: OutStream;
        InS: InStream;
        CR: Text[1];
        LF: Text[1];
        Line: Text;
        Res: Text;
        N: Integer;
    begin
        CR[1] := 13;
        LF[1] := 10;
        B.Data.CreateOutStream(OutS);
        OutS.WriteText('ab' + CR + 'c' + CR + LF + LF + 'xyz');
        B.Data.CreateInStream(InS);
        N := InS.ReadText(Line);
        Res := Line + ':' + Format(N);
        N := InS.ReadText(Line);
        Res += '|' + Line + ':' + Format(N);
        N := InS.ReadText(Line, 2);
        Res += '|' + Line + ':' + Format(N);
        N := InS.ReadText(Line);
        Res += '|' + Line + ':' + Format(N);
        N := InS.ReadText(Line);
        exit(Res + '|' + Line + ':' + Format(N));
    end;

    procedure FileTextMode(): Text
    var
        F: File;
        Line: Text;
        Res: Text;
    begin
        F.WriteMode := true;
        F.TextMode := true;
        F.Create('p2-text.txt');
        F.Write('one');
        F.Write('two');
        F.Close();
        Res := Format(F.TextMode);
        F.Open('p2-text.txt');
        F.Read(Line);
        Res += '|' + Line + ':' + Format(StrLen(Line));
        F.Read(Line);
        Res += '|' + Line + ':' + Format(StrLen(Line));
        Res += '|' + Format(F.Len);
        F.Close();
        exit(Res);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p2-text-streams-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "P2 Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn text_reading_rules() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    // CRs dropped anywhere in the line; the empty line returns 0; a length limit leaves the rest
    assert_eq!(call(&mut s, cat, "ReadTextLines"), "abc:3|:0|xy:2|z:1|:0");
    // TextMode before Create / Open; Write adds CR LF; Read gives the line without it
    assert_eq!(call(&mut s, cat, "FileTextMode"), "Yes|one:3|two:3|10");
    // Bitmap.Save into an InStream writes the PNG into the Blob field
    assert_eq!(call(&mut s, cat, "BitmapSaveToInStream"), "Yes|PNG");
}
