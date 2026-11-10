//! A Windows checkout of the AL sources has CRLF line endings (BCApps' .gitattributes is
//! `* text=auto`, Git for Windows defaults to core.autocrlf=true). Lexing and parsing must give the
//! same tokens (text, line, column) and the same syntax tree as the LF checkout on Linux.

use al_syntax::{lex, parse_file};
use std::collections::HashSet;

const SRC: &str = "\u{feff}namespace Microsoft.Test;

using System.Utilities;

#pragma warning disable AA0005
/// <summary>doc</summary>
codeunit 50100 \"CRLF Test\"
{
    #region vars
    var
        Lbl: Label 'two
lines', Comment = 'x';
    #endregion

    procedure Run(var Rec: Record Customer): Decimal
    var
        i: Integer;
    begin
#if CLEAN24
        i := 1;
#elif CLEAN23
        i := 2;
#else
        i := 3; // comment
#endif
        /* block
           comment */
        if Rec.\"No.\" <> '' then
            exit(1.5 + i);
        Message('it''s %1', 0D);
    end;
}
";

fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn tokens(src: &str, syms: &HashSet<String>) -> Vec<(al_syntax::Tk, String, u32, u32)> {
    lex(src, syms).unwrap().into_iter().map(|t| (t.kind, t.text.to_string(), t.line, t.col)).collect()
}

#[test]
fn crlf_lexes_like_lf() {
    for syms in [HashSet::new(), HashSet::from(["CLEAN24".to_string()]), HashSet::from(["CLEAN23".to_string()])] {
        let lf = tokens(SRC, &syms);
        assert_eq!(lf, tokens(&crlf(SRC), &syms));
        assert!(lf.iter().any(|t| t.1 == "two\nlines"), "multi-line label keeps one LF");
    }
}

#[test]
fn crlf_parses_like_lf() {
    let syms = HashSet::new();
    let lf = format!("{:?}", parse_file(SRC, &syms).unwrap());
    assert_eq!(lf, format!("{:?}", parse_file(&crlf(SRC), &syms).unwrap()));
}

/// Real Base App objects, when vendor/bcapps is present.
#[test]
fn crlf_bcapps_files_parse_like_lf() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bcapps/src/Layers/W1/BaseApp");
    let files = ["Sales/Customer/Customer.Table.al", "Sales/Posting/SalesPost.Codeunit.al", "Sales/Document/SalesHeader.Table.al"];
    let syms = HashSet::new();
    for f in files {
        let Ok(src) = std::fs::read_to_string(root.join(f)) else {
            eprintln!("{f} missing, skipping");
            continue;
        };
        let lf = src.replace("\r\n", "\n");
        assert_eq!(tokens(&lf, &syms), tokens(&crlf(&lf), &syms), "{f}");
        assert_eq!(format!("{:?}", parse_file(&lf, &syms).unwrap()), format!("{:?}", parse_file(&crlf(&lf), &syms).unwrap()), "{f}");
    }
}
