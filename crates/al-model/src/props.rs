//! Helpers for interpreting raw property-value tokens.

use al_syntax::{Body, Element, Member, Property, Tk, Token};

pub fn props(body: &Body) -> impl Iterator<Item = &Property> {
    body.members.iter().filter_map(|m| if let Member::Property(p) = m { Some(p) } else { None })
}

pub fn elements(body: &Body) -> impl Iterator<Item = &Element> {
    body.members.iter().filter_map(|m| if let Member::Element(e) = m { Some(e) } else { None })
}

pub fn prop<'a>(body: &'a Body, name: &str) -> Option<&'a Property> {
    props(body).filter(|p| p.name.eq_ignore_ascii_case(name)).last()
}

/// The raw value tokens of a property (empty when it is not set).
pub fn prop_tokens(body: &Body, name: &str) -> Vec<Token> {
    prop(body, name).map(|p| p.value.clone()).unwrap_or_default()
}

/// First string literal in the property value (Caption = 'x', Comment = ...).
pub fn prop_str(body: &Body, name: &str) -> Option<String> {
    prop(body, name).and_then(|p| p.value.iter().find(|t| t.kind == Tk::Str).map(|t| t.text.to_string()))
}

/// Single identifier / literal value (PageType = Card; SourceTable = Customer; Editable = false).
pub fn prop_word(body: &Body, name: &str) -> Option<String> {
    prop(body, name).and_then(|p| p.value.first().map(|t| t.text.to_string()))
}

pub fn prop_bool(body: &Body, name: &str) -> Option<bool> {
    let p = prop(body, name)?;
    if p.value.len() == 1 {
        let t = &p.value[0];
        if t.is_kw("true") {
            return Some(true);
        }
        if t.is_kw("false") {
            return Some(false);
        }
    }
    None
}

/// Comma separated list of names (ApplicationArea = Basic, Suite; DataCaptionFields = "No.", Name).
pub fn prop_list(body: &Body, name: &str) -> Vec<String> {
    match prop(body, name) {
        Some(p) => split_commas(&p.value).into_iter().map(|g| tokens_text(g)).collect(),
        None => vec![],
    }
}

pub fn split_commas(toks: &[Token]) -> Vec<&[Token]> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            Tk::LParen | Tk::LBrack => depth += 1,
            Tk::RParen | Tk::RBrack => depth -= 1,
            Tk::Comma if depth == 0 => {
                out.push(&toks[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < toks.len() || !toks.is_empty() {
        out.push(&toks[start..]);
    }
    out
}

/// Render tokens as compact text (used for filter values and display).
pub fn tokens_text(toks: &[Token]) -> String {
    let mut s = String::new();
    for (i, t) in toks.iter().enumerate() {
        let piece: &str = match t.kind {
            Tk::Ident | Tk::QIdent | Tk::Str | Tk::Int | Tk::Dec | Tk::BigInt => &t.text,
            Tk::Date => {
                s.push_str(&t.text);
                "D"
            }
            Tk::Time => {
                s.push_str(&t.text);
                "T"
            }
            Tk::DateTime => {
                s.push_str(&t.text);
                "DT"
            }
            Tk::LParen => "(",
            Tk::RParen => ")",
            Tk::LBrack => "[",
            Tk::RBrack => "]",
            Tk::Comma => ",",
            Tk::Dot => ".",
            Tk::DotDot => "..",
            Tk::Eq => "=",
            Tk::Ne => "<>",
            Tk::Lt => "<",
            Tk::Le => "<=",
            Tk::Gt => ">",
            Tk::Ge => ">=",
            Tk::Plus => "+",
            Tk::Minus => "-",
            Tk::Star => "*",
            Tk::Slash => "/",
            Tk::Pipe => "|",
            Tk::Amp => "&",
            Tk::At => "@",
            Tk::ColonColon => "::",
            Tk::Colon => ":",
            Tk::Semi => ";",
            _ => "",
        };
        // keep words separated by a space when two words are adjacent
        if i > 0 && t.kind == Tk::Ident && matches!(toks[i - 1].kind, Tk::Ident) {
            s.push(' ');
        }
        s.push_str(piece);
    }
    s
}

/// Render tokens back to AL source text without losing quoting, so the result can be
/// re-lexed with `al_syntax::lex` and parsed again. Quoted identifiers keep their double
/// quotes, string literals their single quotes (embedded quotes are doubled), and adjacent
/// word-like tokens are separated by one space. Unlike [`tokens_text`], `Rec."No." <> ''`
/// survives the round trip.
pub fn tokens_source(toks: &[Token]) -> String {
    let mut s = String::new();
    let mut prev_word = false;
    for t in toks {
        let word = matches!(t.kind, Tk::Ident | Tk::QIdent | Tk::Str | Tk::Int | Tk::BigInt | Tk::Dec | Tk::Date | Tk::Time | Tk::DateTime);
        if word && prev_word {
            s.push(' ');
        }
        match t.kind {
            Tk::QIdent => {
                s.push('"');
                s.push_str(&t.text.replace('"', "\"\""));
                s.push('"');
            }
            Tk::Str => {
                s.push('\'');
                s.push_str(&t.text.replace('\'', "''"));
                s.push('\'');
            }
            Tk::LBrace => s.push('{'),
            Tk::RBrace => s.push('}'),
            Tk::Assign => s.push_str(":="),
            Tk::Question => s.push('?'),
            _ => s.push_str(&tokens_text(std::slice::from_ref(t))),
        }
        prev_word = word;
    }
    s
}

/// Property value as kept in the `prop_src` maps of the catalog: the unescaped content for a
/// value that is a single string literal (`ToolTip = 'x'`), otherwise [`tokens_source`].
pub fn prop_value_src(p: &Property) -> String {
    if p.value.len() == 1 && p.value[0].kind == Tk::Str {
        p.value[0].text.to_string()
    } else {
        tokens_source(&p.value)
    }
}

pub fn arg_name(el: &Element, i: usize) -> Option<String> {
    let a = el.args.get(i)?;
    if a.len() == 1 && (a[0].is_name() || a[0].kind == Tk::Int) {
        Some(a[0].text.to_string())
    } else if a.is_empty() {
        None
    } else {
        Some(tokens_text(a))
    }
}
