//! Interpreting raw AL property values from the catalog: literals, enums, object
//! references, `AccessByPermission`, and printing expressions back as AL source.

use al_model::{Catalog, Control};
use al_syntax::{BinOp, Expr, ObjectKind, Tk, Token, UnOp};
use std::collections::HashSet;

/// Lex a property source string (as kept in `prop_src`) without the trailing Eof token.
pub(crate) fn lex_src(src: &str) -> Vec<Token> {
    let mut toks = al_syntax::lex(src, &HashSet::new()).unwrap_or_default();
    if toks.last().is_some_and(|t| t.kind == Tk::Eof) {
        toks.pop();
    }
    toks
}

pub(crate) fn expr_of(src: &str) -> Option<Expr> {
    let toks = lex_src(src);
    if toks.is_empty() {
        return None;
    }
    al_syntax::parse_expr(&toks).ok()
}

/// `true` / `false` (also `yes` / `no`, which old code uses).
pub(crate) fn bool_lit(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" => Some(true),
        "false" | "no" => Some(false),
        _ => None,
    }
}

/// Lossy word value of a control property (`Importance = Promoted` -> `Promoted`).
pub(crate) fn word<'a>(c: &'a Control, k: &str) -> Option<&'a str> {
    c.props.get(k).map(|s| s.as_str())
}

/// Lossless source of a control property.
pub(crate) fn src<'a>(c: &'a Control, k: &str) -> Option<&'a str> {
    c.prop_src.get(k).or_else(|| c.props.get(k)).map(|s| s.as_str())
}

pub(crate) fn cbool(c: &Control, k: &str) -> Option<bool> {
    word(c, k).and_then(bool_lit)
}

/// `ApplicationArea = Basic, Suite` -> `#Basic,#Suite` (the NST form; `#All` and
/// `#Basic,#Suite` are among the web server's cached literals).
pub(crate) fn app_area(s: &str) -> String {
    s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| if p.starts_with('#') { p.to_string() } else { format!("#{p}") })
        .collect::<Vec<_>>()
        .join(",")
}

/// Case-insensitive lookup in a name -> value table.
pub(crate) fn enum_val(table: &[(&str, i64)], s: &str) -> Option<i64> {
    let s = s.trim().trim_matches('"');
    table.iter().find(|(n, _)| n.eq_ignore_ascii_case(s)).map(|&(_, v)| v)
}

pub(crate) const PAGE_TYPES: &[(&str, i64)] = &[
    ("Card", 0),
    ("List", 1),
    ("RoleCenter", 2),
    ("CardPart", 3),
    ("ListPart", 4),
    ("Document", 5),
    ("Worksheet", 6),
    ("ListPlus", 7),
    ("ConfirmationDialog", 8),
    ("NavigatePage", 9),
    ("StandardDialog", 10),
    ("API", 11),
    ("HeadlinePart", 12),
    ("ReportPreview", 13),
    ("ReportProcessingOnly", 14),
    ("XmlPort", 15),
    ("ReportViewer", 16),
    ("FilterPage", 17),
    ("ListQuery", 18),
    ("BannerPart", 19),
    ("PromptDialog", 20),
    ("ConfigurationDialog", 21),
    ("UserControlHost", 22),
];
pub(crate) const USAGE_CATEGORIES: &[(&str, i64)] =
    &[("None", 0), ("Lists", 1), ("Tasks", 2), ("ReportsAndAnalysis", 3), ("Documents", 4), ("History", 5), ("Administration", 6)];
pub(crate) const IMPORTANCE: &[(&str, i64)] = &[("Standard", 0), ("Promoted", 1), ("Additional", 2)];
pub(crate) const EXTENDED_DATATYPES: &[(&str, i64)] = &[
    ("None", 1),
    ("PhoneNo", 2),
    ("URL", 3),
    ("EMail", 4),
    ("Ratio", 5),
    ("Masked", 6),
    ("Person", 7),
    ("Resource", 8),
    ("RichContent", 9),
    ("Barcode", 10),
    ("Document", 11),
    ("Task", 12),
];
pub(crate) const STYLES: &[(&str, i64)] = &[
    ("None", 0),
    ("Standard", 1),
    ("StandardAccent", 2),
    ("Strong", 3),
    ("StrongAccent", 4),
    ("Attention", 5),
    ("AttentionAccent", 6),
    ("Favorable", 7),
    ("Unfavorable", 8),
    ("Ambiguous", 9),
    ("Subordinate", 10),
];
pub(crate) const BLANK_NUMBERS: &[(&str, i64)] =
    &[("DontBlank", 0), ("BlankNeg", 1), ("BlankNegAndZero", 2), ("BlankZero", 3), ("BlankZeroAndPos", 4), ("BlankPos", 5)];
pub(crate) const RUN_PAGE_MODES: &[(&str, i64)] = &[("View", 0), ("Edit", 1), ("Create", 2)];
pub(crate) const SCOPES: &[(&str, i64)] = &[("Page", 0), ("PageFilter", 1), ("Repeater", 2), ("RepeaterRow", 3), ("RepeaterSelection", 4)];
pub(crate) const DATA_CLASSIFICATIONS: &[(&str, i64)] = &[
    ("CustomerContent", 0),
    ("ToBeClassified", 1),
    ("EndUserIdentifiableInformation", 2),
    ("AccountData", 3),
    ("EndUserPseudonymousIdentifiers", 4),
    ("OrganizationIdentifiableInformation", 5),
    ("SystemMetadata", 6),
];
pub(crate) const OBSOLETE_STATES: &[(&str, i64)] = &[("No", 0), ("Pending", 1), ("Removed", 2), ("PendingMove", 3), ("Moved", 4)];
pub(crate) const ACCESS: &[(&str, i64)] = &[("Public", 0), ("Internal", 1), ("Protected", 2), ("Local", 3)];
pub(crate) const BLOB_SUBTYPES: &[(&str, i64)] = &[("UserDefined", 0), ("Bitmap", 1), ("Memo", 2), ("Json", 3)];
pub(crate) const MASK_TYPES: &[(&str, i64)] = &[("None", 0), ("Concealed", 1)];
pub(crate) const SYSTEM_PARTS: &[(&str, i64)] = &[("Outlook", 0), ("Notes", 1), ("MyNotes", 2), ("Links", 3), ("RecordLinks", 3), ("Summary", 4)];
pub(crate) const UPDATE_PROPAGATION: &[(&str, i64)] = &[("SubPart", 0), ("Both", 1)];
pub(crate) const TREE_STATES: &[(&str, i64)] = &[("ExpandAll", 0), ("CollapseAll", 1)];
pub(crate) const GRID_LAYOUTS: &[(&str, i64)] = &[("Rows", 1), ("Columns", 2), ("Wide", 3)];
pub(crate) const CUSTOM_ACTION_TYPES: &[(&str, i64)] = &[("Flow", 0), ("FlowTemplate", 1), ("FlowTemplateGallery", 2)];
pub(crate) const TABLE_TYPES: &[(&str, i64)] =
    &[("Normal", 0), ("CRM", 1), ("ExternalSQL", 2), ("Exchange", 3), ("MicrosoftGraph", 4), ("Query", 5), ("Temporary", 6)];

/// AL object kind word -> catalog kind and `RunObjectType` (Page 1, Report 2, Codeunit 3,
/// XMLport 4, Query 5).
fn run_kind(w: &str) -> Option<(ObjectKind, i64)> {
    Some(match w.to_ascii_lowercase().as_str() {
        "page" => (ObjectKind::Page, 1),
        "report" => (ObjectKind::Report, 2),
        "codeunit" => (ObjectKind::Codeunit, 3),
        "xmlport" => (ObjectKind::XmlPort, 4),
        "query" => (ObjectKind::Query, 5),
        _ => return None,
    })
}

/// `RunObject = Page "Customer Card"` -> (RunObjectType, object id).
pub(crate) fn run_object(cat: &Catalog, s: &str) -> Option<(i64, i64)> {
    let toks = lex_src(s);
    let (kind, rt) = run_kind(&toks.first()?.text)?;
    let rest: Vec<&Token> = toks[1..].iter().filter(|t| t.kind != Tk::ColonColon).collect();
    let name = object_name(&rest)?;
    Some((rt, cat.id_of(kind, &name)?))
}

/// Object name from tokens like `"Sales Header"`, `21`, `Microsoft.Sales.Customer."Customer Card"`.
fn object_name(toks: &[&Token]) -> Option<String> {
    let parts: Vec<&str> = toks.iter().filter(|t| t.kind != Tk::Dot).map(|t| &*t.text).collect();
    parts.last().map(|s| s.to_string())
}

/// `AccessByPermission = TableData "Sales Header" = R` -> (AccessByObjectType, object id,
/// PermissionMask). Object types: Page 1, Report 2, Codeunit 3, XMLport 4, TableData 5,
/// Table 6, Query 7, System 8. Mask letters: R 1, I 2, M 4, D 8, X 16.
pub(crate) fn access_by_permission(cat: &Catalog, s: &str) -> Option<(i64, i64, i64)> {
    let toks = lex_src(s);
    let eq = toks.iter().position(|t| t.kind == Tk::Eq)?;
    let kind = toks.first()?.text.to_ascii_lowercase();
    let name_toks: Vec<&Token> = toks[1..eq].iter().filter(|t| t.kind != Tk::ColonColon).collect();
    let name = object_name(&name_toks)?;
    let (ty, ok) = match kind.as_str() {
        "page" => (1, Some(ObjectKind::Page)),
        "report" => (2, Some(ObjectKind::Report)),
        "codeunit" => (3, Some(ObjectKind::Codeunit)),
        "xmlport" => (4, Some(ObjectKind::XmlPort)),
        "tabledata" => (5, Some(ObjectKind::Table)),
        "table" => (6, Some(ObjectKind::Table)),
        "query" => (7, Some(ObjectKind::Query)),
        "system" => (8, None),
        _ => return None,
    };
    let id = match ok {
        Some(k) => cat.id_of(k, &name)?,
        None => name.parse().unwrap_or(0),
    };
    let mut mask = 0;
    for t in &toks[eq + 1..] {
        for ch in t.text.chars() {
            mask |= match ch.to_ascii_uppercase() {
                'R' => 1,
                'I' => 2,
                'M' => 4,
                'D' => 8,
                'X' => 16,
                _ => 0,
            };
        }
    }
    Some((ty, id, mask))
}

/// Identifier as AL source: bare when it is a plain word, otherwise double-quoted.
pub(crate) fn ident_src(n: &str) -> String {
    let plain = n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        n.to_string()
    } else {
        format!("\"{}\"", n.replace('"', "\"\""))
    }
}

fn binop(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::IntDiv => "div",
        BinOp::Mod => "mod",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Xor => "xor",
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::In => "in",
    }
}

fn sub_src(e: &Expr) -> String {
    match e {
        Expr::Binary(..) | Expr::Ternary(..) | Expr::Is(..) | Expr::As(..) => format!("({})", expr_src(e)),
        _ => expr_src(e),
    }
}

/// Print an expression back as AL source (used for `DFD_8 SourceExpression` and for the
/// expressions the dataset code evaluates per row).
pub(crate) fn expr_src(e: &Expr) -> String {
    let list = |xs: &[Expr]| xs.iter().map(expr_src).collect::<Vec<_>>().join(", ");
    match e {
        Expr::Int(i) | Expr::BigInt(i) => i.to_string(),
        Expr::Dec(s) => s.to_string(),
        Expr::Str(s) => format!("'{}'", s.replace('\'', "''")),
        Expr::Bool(b) => b.to_string(),
        Expr::Date(s) => format!("{s}D"),
        Expr::Time(s) => format!("{s}T"),
        Expr::DateTime(s) => format!("{s}DT"),
        Expr::Ident(n) => ident_src(n),
        Expr::This => "this".into(),
        Expr::Member(b, n) => format!("{}.{}", sub_src(b), ident_src(n)),
        Expr::Scope(b, n) => format!("{}::{}", sub_src(b), ident_src(n)),
        Expr::Call(f, args) => format!("{}({})", sub_src(f), list(args)),
        Expr::Index(b, args) => format!("{}[{}]", sub_src(b), list(args)),
        Expr::Unary(UnOp::Not, x) => format!("not {}", sub_src(x)),
        Expr::Unary(UnOp::Neg, x) => format!("-{}", sub_src(x)),
        Expr::Unary(UnOp::Plus, x) => format!("+{}", sub_src(x)),
        Expr::Binary(op, a, b) => format!("{} {} {}", sub_src(a), binop(op), sub_src(b)),
        Expr::Range(a, b) => format!("{}..{}", a.as_deref().map(expr_src).unwrap_or_default(), b.as_deref().map(expr_src).unwrap_or_default()),
        Expr::Set(xs) => format!("[{}]", list(xs)),
        Expr::Ternary(c, a, b) => format!("{} ? {} : {}", sub_src(c), sub_src(a), sub_src(b)),
        Expr::Is(x, n) => format!("{} is {}", sub_src(x), ident_src(n)),
        Expr::As(x, n) => format!("{} as {}", sub_src(x), ident_src(n)),
    }
}

/// `Database::Customer` / `Database::"Sales Header"` -> table id; other constants unchanged.
pub(crate) fn resolve_scope_const(cat: &Catalog, v: &str) -> String {
    let t = v.trim();
    if let Some((scope, name)) = t.split_once("::") {
        if scope.trim().eq_ignore_ascii_case("database") {
            if let Some(id) = cat.id_of(ObjectKind::Table, name.trim().trim_matches('"')) {
                return id.to_string();
            }
        }
    }
    t.to_string()
}
