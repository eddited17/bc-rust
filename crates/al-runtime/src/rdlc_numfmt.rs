//! Report textbox Format strings (.NET numeric format strings) as Excel number format codes, for
//! the cells of an RDLC layout saved as an xlsx workbook (rdlc_xlsx).
//!
//! The code fixes what the cell displays, which both sides document: .NET for the textbox
//! (learn.microsoft.com/dotnet/standard/base-types/standard-numeric-format-strings and
//! .../custom-numeric-format-strings), Excel for the cell (support.microsoft.com/office/
//! number-format-codes-5026bbd6-04bc-48cd-bf33-80f18b4eae68). The exact strings Business Central
//! writes are not documented; where the public sources leave a point open, the conversion takes a
//! documented fallback (usually General) and records an assume hit (`crate::assume`) instead of
//! guessing. Each fallback names its open item (O-n), which a probe on a real BC settles.
//!
//! The code depends only on (format, culture, kind), never on the cell's value: Excel and .NET
//! both pick a section per value inside the code, so one textbox Format gives one style.
//!
//! # Negative values written as "sign, then the positive display" (O-19)
//!
//! .NET documents what a negative shows; for most formats that is the culture's negative sign
//! followed by the positive display (N with NumberNegativePattern 1, F, D, E, and custom formats
//! of one section). Excel's page says a one-section code "is used for all numbers" and does not say
//! whether Excel then adds a minus sign, and ECMA-376 §18.8.30/§18.8.31 add nothing to that. What
//! the page does document is a code of two sections: "the first is for positive numbers and zeros,
//! and the second for negative numbers", and a `-` written in a section is displayed as is.
//!
//! From the public sources alone, this module's choice was an explicit second section, `pos;-pos`:
//! the only form whose negative display they fix. Probe P1 on a real BC 29.1 (vectors
//! u2-xlsx-numfmt, 48 cultures; tests/vectors/xlsx-numfmt/u2-p1-compact.tsv) settles it per class,
//! and [NEG_FORM] holds the result: N and C write an explicit negative section in every culture
//! (`#,##0.00;-#,##0.00`; en-GB C `"£"#,##0.00;\-"£"#,##0.00`), F, D, E, P and custom formats of one
//! section write one section. For those BC writes `0.00`, `#0`, `0.000000E+000`, `#,##0.00%` and
//! `#,##0.00`; the converter writes the same, except for P, where it writes `#,##0.00 %` (the
//! invariant PercentPositivePattern 0) while BC writes `#,##0.00%` in all 48 cultures. A class the
//! table does not list keeps the explicit section with the assume hit `U2.NegativeForm`.
//!
//! # What BC writes and what this module writes
//!
//! BC prefixes every code with `[$-01<LCID>]`, the LCID of the textbox's culture (`[$-010409]` for
//! en-US); this module omits the prefix. Beyond that, BC passes custom formats through nearly as
//! written and quotes single letters (`"d""d"-"M""M"-"y""y"`), where this module quotes runs of
//! literal text (`"dd"-"MM"-"yy"`). The vector file marks each P1 row: `=` (the same code),
//! `spelling` (another spelling of the same display), `differs` (BC's code shows something other
//! than the .NET display this module follows: P's `%` without the space, a kept trailing `.`, `0/0`
//! as a fraction, `[Red]` as a colour, `#VALUE!` cells for `#0`, `00`, `%0`), `culture` (C and c need
//! the culture's data, O-3), `error` (BC writes a `#VALUE!` cell, this module General) and
//! `open:O-n` (the fallback of an open item). The spec's R-CANON says the P1 code wins where they
//! differ; that is not done yet.

use crate::assume;

/// Integral: the value is an AL Integer/BigInteger (or an RDLC integer expression).
/// Fractional: Decimal, or unknown. (R-KIND: the standard D, X and B are "Integral types only".)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumKind {
    Integral,
    Fractional,
}

/// The NumberFormatInfo values the conversion reads. Field meanings and value ranges are those of
/// the NumberFormatInfo property pages on Learn (learn.microsoft.com/dotnet/api/
/// system.globalization.numberformatinfo).
#[derive(Debug, PartialEq, Eq)]
pub struct NumCulture {
    /// "" = invariant
    pub name: &'static str,
    pub negative_sign: &'static str,
    pub number_decimal_digits: u32,
    /// 0..=4
    pub number_negative_pattern: u8,
    pub number_group_sizes: &'static [u8],
    pub currency_symbol: &'static str,
    pub currency_decimal_digits: u32,
    /// 0..=3
    pub currency_positive_pattern: u8,
    /// 0..=16
    pub currency_negative_pattern: u8,
    pub currency_group_sizes: &'static [u8],
    pub percent_symbol: &'static str,
    pub percent_decimal_digits: u32,
    /// 0..=3
    pub percent_positive_pattern: u8,
    /// 0..=11
    pub percent_negative_pattern: u8,
    pub percent_group_sizes: &'static [u8],
}

/// The invariant culture: each value is the "default for InvariantInfo" sentence of its
/// NumberFormatInfo property page on Learn (NegativeSign "-", NumberDecimalDigits 2,
/// NumberNegativePattern 1, NumberGroupSizes {3}, CurrencySymbol "¤", CurrencyDecimalDigits 2,
/// CurrencyPositivePattern 0, CurrencyNegativePattern 0, PercentSymbol "%", PercentDecimalDigits 2,
/// PercentPositivePattern 0, PercentNegativePattern 0).
pub static INVARIANT: NumCulture = NumCulture {
    name: "",
    negative_sign: "-",
    number_decimal_digits: 2,
    number_negative_pattern: 1,
    number_group_sizes: &[3],
    currency_symbol: "\u{a4}",
    currency_decimal_digits: 2,
    currency_positive_pattern: 0,
    currency_negative_pattern: 0,
    currency_group_sizes: &[3],
    percent_symbol: "%",
    percent_decimal_digits: 2,
    percent_positive_pattern: 0,
    percent_negative_pattern: 0,
    percent_group_sizes: &[3],
};

/// Named cultures, with the values BC's server uses. Only probe output may fill this (R-CUL-3,
/// O-3: probes P1 and P3); until then every name resolves to the invariant culture.
static CULTURES: &[NumCulture] = &[];

/// Culture by name, case-insensitive. Unknown names give the invariant culture and an assume hit
/// (`U2.CultureData`, R-CUL-3).
pub fn culture(name: &str) -> &'static NumCulture {
    if name.is_empty() {
        return &INVARIANT;
    }
    if let Some(c) = CULTURES.iter().find(|c| c.name.eq_ignore_ascii_case(name)) {
        return c;
    }
    assume::hit("U2.CultureData");
    &INVARIANT
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExcelNumFmt {
    General,
    Code(String),
}

/// The open items (spec section 4) a conversion can fall back on: General, with an assume hit
/// `U2.OpenFormat` and the item as its member (`U2.OpenFormat:O-7`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// G with a precision, X, B: no Excel code shows them.
    O1,
    /// An invalid format (FormatException in .NET), a padded standard string, a single non-ASCII
    /// letter, an unterminated quote, a trailing backslash, an over-large precision.
    O7,
    /// A section with a `.` but no digit placeholder.
    O9,
    /// More than three sections, or an empty section.
    O10,
    /// Several `%` in a section, `‰`, a percent symbol other than `%`.
    O11,
    /// A comma that is neither grouping nor scaling.
    O12,
    /// An exponent section outside R-CUS-EXP.
    O13,
    /// A format over [MAX_FORMAT] characters: not converted, so the work per cell stays bounded.
    Length,
}

impl Open {
    pub fn id(self) -> &'static str {
        match self {
            Open::O1 => "O-1",
            Open::O7 => "O-7",
            Open::O9 => "O-9",
            Open::O10 => "O-10",
            Open::O11 => "O-11",
            Open::O12 => "O-12",
            Open::O13 => "O-13",
            Open::Length => "length",
        }
    }
}

/// A conversion and what it relied on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub fmt: ExcelNumFmt,
    /// The open item it fell back on (the result is then General).
    pub open: Option<Open>,
    /// It wrote a negative by an O-19 row no probe shows ([NEG_FORM]).
    pub negative_form: bool,
    /// It read the culture's data (patterns, symbols, digit counts, negative sign).
    pub culture_used: bool,
}

/// The Excel number format for a number cell whose textbox has `format` (as evaluated, not
/// trimmed). Total: never panics, never returns an empty Code, every Code is shorter than 255
/// characters.
pub fn to_excel(format: &str, culture: &NumCulture, kind: NumKind) -> ExcelNumFmt {
    let o = convert(format, culture, kind);
    record(&o);
    o.fmt
}

/// [to_excel] for a culture given by name, which is looked up (and its assume hit recorded) only
/// when the conversion reads culture data. Returns the format and whether the culture was read.
pub fn to_excel_in(format: &str, culture_name: &str, kind: NumKind) -> (ExcelNumFmt, bool) {
    let probe = convert(format, &INVARIANT, kind);
    if !probe.culture_used {
        record(&probe);
        return (probe.fmt, false);
    }
    let o = convert(format, culture(culture_name), kind);
    record(&o);
    (o.fmt, true)
}

fn record(o: &Outcome) {
    if let Some(open) = o.open {
        assume::hit_member("U2.OpenFormat", open.id());
    }
    if o.negative_form {
        assume::hit("U2.NegativeForm");
    }
}

// ------------------------------------------------------------------ negative form (O-19)

/// The format classes whose negative can display as "negative sign, then the positive display".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignClass {
    /// `N` with NumberNegativePattern 1
    N,
    F,
    D,
    E,
    /// `C` whose negative pattern is the sign before the positive pattern
    C,
    /// `P` whose negative pattern is the sign before the positive pattern
    P,
    /// a custom format of one section
    Custom,
}

/// How such a negative is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NegForm {
    /// The positive code alone.
    OneSection,
    /// `pos;` then the negative sign as a literal and the positive code again.
    Explicit,
}

/// One row of [NEG_FORM]: class, NumberNegativePattern, negative sign (None: any), the form, and
/// whether a probe on a real BC shows it (false: an assumption, recorded as `U2.NegativeForm`).
pub type NegRow = (Option<SignClass>, Option<u8>, Option<&'static str>, NegForm, bool);

/// The O-19 table, first matching row wins. Rows from probe P1 (T4, u2-xlsx-numfmt, 48 cultures):
/// N, N0-N99 and C, C0-C99 write two sections; F, D, E, P and one-section custom formats write
/// one (for P, BC's section is `#,##0.00%`, this module's `#,##0.00 %`: a `differs` row).
pub const NEG_FORM: &[NegRow] = &[
    (Some(SignClass::N), None, None, NegForm::Explicit, true),
    (Some(SignClass::C), None, None, NegForm::Explicit, true),
    (Some(SignClass::F), None, None, NegForm::OneSection, true),
    (Some(SignClass::D), None, None, NegForm::OneSection, true),
    (Some(SignClass::E), None, None, NegForm::OneSection, true),
    (Some(SignClass::P), None, None, NegForm::OneSection, true),
    (Some(SignClass::Custom), None, None, NegForm::OneSection, true),
    // anything else: the explicit section the public sources document (module documentation)
    (None, None, None, NegForm::Explicit, false),
];

/// The form for `class` in `culture` (the one place that decides O-19), and whether it is shown
/// by a probe.
pub fn neg_form(class: SignClass, culture: &NumCulture) -> (NegForm, bool) {
    NEG_FORM
        .iter()
        .find(|(c, p, s, _, _)| c.is_none_or(|c| c == class) && p.is_none_or(|p| p == culture.number_negative_pattern) && s.is_none_or(|s| s == culture.negative_sign))
        .map(|r| (r.3, r.4))
        .unwrap_or((NegForm::Explicit, false))
}

// ------------------------------------------------------------------ code pieces

/// A piece of an Excel section: code text written as is, or literal text to quote.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Code(String),
    Lit(String),
}

/// Characters Excel shows without quotation marks (support.microsoft.com "Number format codes":
/// "$ + ( : ^ ' { < = - / ) ! & ~ } > and the space character"). Only those that can never start
/// a code are used bare here: space, `-`, `+`, `(`, `)`, `$` (R-CUS-LIT).
fn bare(c: char) -> bool {
    matches!(c, ' ' | '-' | '+' | '(' | ')' | '$')
}

/// Literal text as Excel code that shows it unchanged and gives it no meaning (R-CUS-LIT): bare
/// characters as they are, every other maximal run inside double quotes, a `"` (and, so no reader
/// takes it for an escape inside quotes, a `\`) after a backslash.
fn encode_lit(s: &str, out: &mut String) {
    let mut quoted = false;
    for c in s.chars() {
        let plain = !bare(c) && c != '"' && c != '\\';
        if plain != quoted {
            out.push('"');
            quoted = plain;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    if quoted {
        out.push('"');
    }
}

/// A section's pieces as code: adjacent literals are one run. A section of nothing but an empty
/// literal (`''`) shows nothing; BC spells it `""` (T4 probe P1: `0;-0;''` gives `0;-0;""`).
fn encode(pieces: &[Piece]) -> String {
    if !pieces.is_empty() && pieces.iter().all(|p| *p == Piece::Lit(String::new())) {
        return "\"\"".into();
    }
    let mut out = String::new();
    let mut lit = String::new();
    for p in pieces {
        match p {
            Piece::Lit(s) => lit.push_str(s),
            Piece::Code(c) => {
                encode_lit(&lit, &mut out);
                lit.clear();
                out.push_str(c);
            }
        }
    }
    encode_lit(&lit, &mut out);
    out
}

/// The conversion state: the culture and what was read from it.
struct Cx<'a> {
    c: &'a NumCulture,
    used: bool,
    negative_form: bool,
}

impl<'a> Cx<'a> {
    fn culture(&mut self) -> &'a NumCulture {
        self.used = true;
        self.c
    }
}

enum Fail {
    Open(Open),
    /// The code would reach 255 characters: General without an assume hit (R-LIMIT).
    TooLong,
}

impl From<Open> for Fail {
    fn from(o: Open) -> Fail {
        Fail::Open(o)
    }
}

/// The longest code Excel takes is 254 characters ([MS-OI29500] §2.1.712: "less than 255").
const MAX_CODE: usize = 254;

/// The full conversion.
pub fn convert(format: &str, culture: &NumCulture, kind: NumKind) -> Outcome {
    let mut cx = Cx { c: culture, used: false, negative_form: false };
    let r = convert_in(format, kind, &mut cx);
    let (fmt, open) = match r {
        Ok(None) => (ExcelNumFmt::General, None),
        Ok(Some(code)) if code.is_empty() => (ExcelNumFmt::General, Some(Open::O7)),
        Ok(Some(code)) if code.encode_utf16().count() > MAX_CODE => (ExcelNumFmt::General, None),
        Ok(Some(code)) => (ExcelNumFmt::Code(code), None),
        Err(Fail::TooLong) => (ExcelNumFmt::General, None),
        Err(Fail::Open(o)) => (ExcelNumFmt::General, Some(o)),
    };
    let negative_form = cx.negative_form && matches!(fmt, ExcelNumFmt::Code(_));
    Outcome { fmt, open, negative_form, culture_used: cx.used }
}

/// Characters an XML 1.0 document cannot hold (the code goes into styles.xml): such a format
/// can't be written as a code (W-INVALID).
fn xml_invalid(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}')
}

/// The longest Format converted. A longer one gives General (with `U2.OpenFormat:length`) before
/// it is tokenized; no BCApps layout comes near it (the longest literal of spec section 3.1 has 66
/// characters), and a code from it would rarely stay under 255 characters.
pub const MAX_FORMAT: usize = 1024;

/// Ok(None): General (R-CELL-2, R-STD-G); Ok(Some(code)).
fn convert_in(format: &str, kind: NumKind, cx: &mut Cx) -> Result<Option<String>, Fail> {
    // R-CELL-2: no Format, or one that evaluates to empty (.NET: empty means "G")
    if format.is_empty() {
        return Ok(None);
    }
    if format.chars().nth(MAX_FORMAT).is_some() {
        return Err(Open::Length.into());
    }
    if format.chars().any(xml_invalid) {
        return Err(Open::O7.into());
    }
    if let Some((letter, precision)) = standard(format) {
        return std_code(letter, precision, kind, cx);
    }
    // R-STD-0 / O-7: a single non-ASCII letter, a standard string padded with whitespace
    let mut chars = format.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_alphabetic() {
            return Err(Open::O7.into());
        }
    }
    let trimmed = format.trim();
    if trimmed != format && standard(trimmed).is_some() {
        return Err(Open::O7.into());
    }
    custom_code(format, cx).map(Some)
}

// ------------------------------------------------------------------ standard formats

/// R-STD-0: one ASCII letter, optionally followed by decimal digits (the precision; None when
/// absent, u64::MAX when it does not fit).
fn standard(format: &str) -> Option<(char, Option<u64>)> {
    let mut chars = format.chars();
    let letter = chars.next().filter(|c| c.is_ascii_alphabetic())?;
    let digits = chars.as_str();
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.is_empty() {
        return Some((letter, None));
    }
    let significant = digits.trim_start_matches('0');
    let p = if significant.len() > 18 { u64::MAX } else { significant.parse::<u64>().unwrap_or(0) };
    Some((letter, Some(p)))
}

/// .NET 7+ takes precisions up to 999,999,999 (standard numeric format strings, "Precision
/// specifier"); above that the format is invalid (R-STD-MAX).
const MAX_PRECISION: u64 = 999_999_999;

/// `0` digits after a `.` when p > 0.
fn decimals(p: u64) -> String {
    if p == 0 {
        String::new()
    } else {
        format!(".{}", "0".repeat(p as usize))
    }
}

/// NumberNegativePattern 0..=4 (Learn, NumberFormatInfo.NumberNegativePattern): `-` is the
/// negative sign, `n` the number.
const NUMBER_NEG: [&str; 5] = ["(n)", "-n", "- n", "n-", "n -"];
/// CurrencyPositivePattern 0..=3: `$` is the currency symbol.
const CURRENCY_POS: [&str; 4] = ["$n", "n$", "$ n", "n $"];
/// CurrencyNegativePattern 0..=16.
const CURRENCY_NEG: [&str; 17] = ["($n)", "-$n", "$-n", "$n-", "(n$)", "-n$", "n-$", "n$-", "-n $", "-$ n", "n $-", "$ n-", "$ -n", "n- $", "($ n)", "(n $)", "$- n"];
/// PercentPositivePattern 0..=3: `%` is the percent symbol.
const PERCENT_POS: [&str; 4] = ["n %", "n%", "%n", "% n"];
/// PercentNegativePattern 0..=11.
const PERCENT_NEG: [&str; 12] = ["-n %", "-n%", "-%n", "%-n", "%n-", "n-%", "n%-", "-% n", "n %-", "% n-", "% -n", "n- %"];

/// A NumberFormatInfo pattern as pieces: `n` the number code, `-` the negative sign, `$` the
/// currency symbol, `%` Excel's percent (which, like .NET's P, multiplies by 100), anything else
/// literal.
fn pattern(p: &str, number: &str, c: &NumCulture) -> Vec<Piece> {
    p.chars()
        .map(|ch| match ch {
            'n' => Piece::Code(number.to_string()),
            '-' => Piece::Lit(c.negative_sign.to_string()),
            '$' => Piece::Lit(c.currency_symbol.to_string()),
            '%' => Piece::Code("%".into()),
            o => Piece::Lit(o.to_string()),
        })
        .collect()
}

/// A code from the positive pattern and the negative one. Where the negative pattern is the sign
/// before the positive pattern, the O-19 choice decides; otherwise the negative needs its own
/// section (Excel: "the second section is for negative numbers").
fn with_negative(pos: &str, neg: &str, number: &str, class: SignClass, cx: &mut Cx) -> String {
    let c = cx.culture();
    let pos_code = encode(&pattern(pos, number, c));
    if neg.strip_prefix('-') == Some(pos) {
        let (form, shown) = neg_form(class, c);
        cx.negative_form |= !shown;
        if form == NegForm::OneSection {
            return pos_code;
        }
    }
    format!("{pos_code};{}", encode(&pattern(neg, number, c)))
}

fn std_code(letter: char, precision: Option<u64>, kind: NumKind, cx: &mut Cx) -> Result<Option<String>, Fail> {
    // the kind (W-KIND) decides no code here: probe P1 shows the same codes for Integer and Decimal
    // columns, D included. Only X differs by column, and it has no code either way: a `#VALUE!` cell
    // on a Decimal column, the hex digits as a text cell (General) on an Integer column; B gives
    // `#VALUE!` cells on both
    let _ = kind;
    if precision.is_some_and(|p| p > MAX_PRECISION) {
        return Err(Open::O7.into());
    }
    // every code below holds at least p characters
    if precision.is_some_and(|p| p as usize > MAX_CODE) {
        return Err(Fail::TooLong);
    }
    let p = |default: u32| precision.unwrap_or(default as u64);
    Ok(Some(match letter.to_ascii_uppercase() {
        'N' => {
            let c = cx.culture();
            let number = format!("#,##0{}", decimals(p(c.number_decimal_digits)));
            let neg = NUMBER_NEG.get(c.number_negative_pattern as usize).ok_or(Open::O7)?;
            with_negative("n", neg, &number, SignClass::N, cx)
        }
        'F' => {
            let number = format!("0{}", decimals(p(cx.culture().number_decimal_digits)));
            with_negative("n", "-n", &number, SignClass::F, cx)
        }
        'C' => {
            let c = cx.culture();
            let number = format!("#,##0{}", decimals(p(c.currency_decimal_digits)));
            let pos = CURRENCY_POS.get(c.currency_positive_pattern as usize).ok_or(Open::O7)?;
            let neg = CURRENCY_NEG.get(c.currency_negative_pattern as usize).ok_or(Open::O7)?;
            with_negative(pos, neg, &number, SignClass::C, cx)
        }
        'P' => {
            let c = cx.culture();
            if c.percent_symbol != "%" {
                return Err(Open::O11.into());
            }
            let number = format!("#,##0{}", decimals(p(c.percent_decimal_digits)));
            let pos = PERCENT_POS.get(c.percent_positive_pattern as usize).ok_or(Open::O7)?;
            let neg = PERCENT_NEG.get(c.percent_negative_pattern as usize).ok_or(Open::O7)?;
            with_negative(pos, neg, &number, SignClass::P, cx)
        }
        'E' => {
            // p decimals (default 6), an exponent of at least three digits that always has a sign;
            // BC writes `E` for `e` too (T4 probe P1: e, e0-e10)
            let number = format!("0{}E+000", decimals(p(6)));
            with_negative("n", "-n", &number, SignClass::E, cx)
        }
        // the general format, which is Excel's General
        'G' if precision.is_none() => return Ok(None),
        // a significant-digit limit has no Excel code; BC writes one that depends on the value
        // (T4 probe P1: G0-G29 give `0.0` or `0.0E+000`, O-14), which W-VALUE rules out
        'G' => return Err(Open::O1.into()),
        // D: .NET's "Integral types only" (FormatException on a Decimal), but BC writes the same
        // code for Integer and Decimal columns: `#0` without a precision, else p zeros, at least
        // one (T4 probe P1: D, d, D0, D1, D5, D10 on both)
        'D' => {
            let number = match precision {
                None => "#0".to_string(),
                Some(p) => "0".repeat(p.max(1) as usize),
            };
            with_negative("n", "-n", &number, SignClass::D, cx)
        }
        // hexadecimal and binary: no Excel number code shows them (BC: X on an Integer column is a
        // General text cell with the hex digits, X on a Decimal and B a `#VALUE!` cell; T4 probe P1)
        'X' | 'B' => return Err(Open::O1.into()),
        // R: .NET's round-trip specifier, not valid for Decimal (FormatException), but BC writes a
        // General number cell on both column types (T4 probe P1: R, r, R0-R29)
        'R' => return Ok(None),
        // every other letter: FormatException in .NET; BC writes a `#VALUE!` cell (T4 probe P1: A,
        // H, K, M, O, Q, T, U, Y, Z), which no number format shows
        _ => return Err(Open::O7.into()),
    }))
}

// ------------------------------------------------------------------ custom formats

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    /// `0` or `#`
    Digit(char),
    Dot,
    Comma,
    Percent,
    PerMille,
    /// `E`/`e`, the sign that followed it (`+`, `-` or none) and its zeros
    Exp(char, Option<char>, usize),
    Lit(String),
}

/// R-CUS-TOK: the format's sections, each a list of tokens.
fn tokenize(format: &str) -> Result<Vec<Vec<Tok>>, Open> {
    let cs: Vec<char> = format.chars().collect();
    let mut sections = vec![vec![]];
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        i += 1;
        let tok = match c {
            '0' | '#' => Tok::Digit(c),
            '.' => Tok::Dot,
            ',' => Tok::Comma,
            '%' => Tok::Percent,
            '\u{2030}' => Tok::PerMille,
            ';' => {
                sections.push(vec![]);
                continue;
            }
            '\\' => {
                // a trailing backslash: O-7
                let Some(&n) = cs.get(i) else { return Err(Open::O7) };
                i += 1;
                Tok::Lit(n.to_string())
            }
            '\'' | '"' => {
                let Some(len) = cs[i..].iter().position(|&x| x == c) else { return Err(Open::O7) };
                let s: String = cs[i..i + len].iter().collect();
                i += len + 1;
                Tok::Lit(s)
            }
            'E' | 'e' => {
                let sign = cs.get(i).copied().filter(|s| *s == '+' || *s == '-');
                let at = i + sign.is_some() as usize;
                let zeros = cs[at.min(cs.len())..].iter().take_while(|&&z| z == '0').count();
                if zeros == 0 {
                    Tok::Lit(c.to_string())
                } else {
                    i = at + zeros;
                    Tok::Exp(c, sign, zeros)
                }
            }
            o => Tok::Lit(o.to_string()),
        };
        if let Some(s) = sections.last_mut() {
            s.push(tok);
        }
    }
    Ok(sections)
}

/// A custom format's code (R-CUS-*).
fn custom_code(format: &str, cx: &mut Cx) -> Result<String, Fail> {
    let sections = tokenize(format)?;
    // R-CUS-SEC: up to three; more, or an empty one, is O-10
    if sections.len() > 3 || sections.iter().any(|s| s.is_empty()) {
        return Err(Open::O10.into());
    }
    let mut codes = vec![];
    for s in &sections {
        codes.push(section(s, cx)?);
    }
    if codes.len() > 1 {
        // sections map one to one (R-CUS-SEC), a negative section shows no sign of its own
        return Ok(codes.iter().map(|p| encode(p)).collect::<Vec<_>>().join(";"));
    }
    // one section: a negative shows the culture's negative sign before the whole result
    // (R-CUS-SIGN)
    let pos = &codes[0];
    let c = cx.culture();
    let (form, shown) = neg_form(SignClass::Custom, c);
    cx.negative_form |= !shown;
    Ok(match form {
        NegForm::OneSection => encode(pos),
        NegForm::Explicit => {
            let mut neg = vec![Piece::Lit(c.negative_sign.to_string())];
            neg.extend(pos.iter().cloned());
            format!("{};{}", encode(pos), encode(&neg))
        }
    })
}

/// One section's pieces.
fn section(toks: &[Tok], cx: &mut Cx) -> Result<Vec<Piece>, Fail> {
    let count = |f: &dyn Fn(&Tok) -> bool| toks.iter().filter(|t| f(t)).count();
    // R-CUS-PERMILLE, R-CUS-PCT
    if count(&|t| *t == Tok::PerMille) > 0 || count(&|t| *t == Tok::Percent) > 1 {
        return Err(Open::O11.into());
    }
    if count(&|t| *t == Tok::Percent) == 1 && cx.culture().percent_symbol != "%" {
        return Err(Open::O11.into());
    }
    let digits: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| matches!(t, Tok::Digit(_))).map(|(i, _)| i).collect();
    let dot = toks.iter().position(|t| *t == Tok::Dot);
    let exp: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| matches!(t, Tok::Exp(..))).map(|(i, _)| i).collect();
    let has_comma = toks.contains(&Tok::Comma);
    let Some(&last_digit) = digits.last() else {
        // R-CUS-NODIGIT: literal text only
        if dot.is_some() {
            return Err(Open::O9.into());
        }
        if has_comma {
            return Err(Open::O12.into());
        }
        if !exp.is_empty() {
            return Err(Open::O13.into());
        }
        if toks.contains(&Tok::Percent) {
            return Err(Open::O7.into());
        }
        return Ok(toks.iter().filter_map(|t| if let Tok::Lit(s) = t { Some(Piece::Lit(s.clone())) } else { None }).collect());
    };
    // the integer part ends at the first `.` or the exponent
    let int_end = dot.into_iter().chain(exp.first().copied()).min().unwrap_or(toks.len());
    let int_digits: Vec<usize> = digits.iter().copied().filter(|&i| i < int_end).collect();
    // R-CUS-EXP: one exponent after exactly one integer placeholder, no commas, no placeholder
    // after it; anything else is O-13
    if let Some(&e) = exp.first() {
        if exp.len() > 1 || int_digits.len() != 1 || has_comma || last_digit > e {
            return Err(Open::O13.into());
        }
    }
    // R-CUS-DOT: the first `.` only, and only when a placeholder follows it
    let keep_dot = dot.filter(|&d| last_digit > d);
    // R-CUS-GRP, R-CUS-SCALE, R-CUS-COMMA-OTHER, in one pass: `run_end` ends the run of commas
    // right after the last integer placeholder
    let run_end = int_digits.last().map(|&l| (l + 1..toks.len()).find(|&j| toks[j] != Tok::Comma).unwrap_or(toks.len()));
    let mut first_group: Option<usize> = None;
    let mut scale = 0usize;
    for (i, t) in toks.iter().enumerate() {
        if *t != Tok::Comma {
            continue;
        }
        let (Some(&first_int), Some(&last_int), Some(run_end)) = (int_digits.first(), int_digits.last(), run_end) else {
            return Err(Open::O12.into());
        };
        if i > int_end || i < first_int {
            return Err(Open::O12.into());
        }
        if i < last_int {
            first_group.get_or_insert(i);
            continue;
        }
        // after the last integer placeholder: scaling when only commas lie between that
        // placeholder and this comma (it is in the run), and between this comma and an explicit
        // `.` (the run reaches the `.`)
        if i < run_end && dot.is_none_or(|d| run_end == d) {
            scale += 1;
        } else {
            return Err(Open::O12.into());
        }
    }
    let mut out = vec![];
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Digit(c) => {
                out.push(Piece::Code(c.to_string()));
                if i == last_digit && scale > 0 {
                    out.push(Piece::Code(",".repeat(scale)));
                }
            }
            Tok::Dot if Some(i) == keep_dot => out.push(Piece::Code(".".into())),
            Tok::Dot => {}
            Tok::Comma if Some(i) == first_group => out.push(Piece::Code(",".into())),
            Tok::Comma => {}
            Tok::Percent => out.push(Piece::Code("%".into())),
            Tok::PerMille => {}
            // `E+` shows the sign always, `E-` (and .NET's E without a sign) only when negative
            Tok::Exp(e, sign, zeros) => out.push(Piece::Code(format!("{e}{}{}", if *sign == Some('+') { '+' } else { '-' }, "0".repeat(*zeros)))),
            Tok::Lit(s) => out.push(Piece::Lit(s.clone())),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A culture with probe-like data for V52 (negative sign U+2212, NumberNegativePattern 1).
    static MINUS: NumCulture = NumCulture { negative_sign: "\u{2212}", name: "x-minus", ..copy_invariant() };

    const fn copy_invariant() -> NumCulture {
        NumCulture {
            name: "",
            negative_sign: "-",
            number_decimal_digits: 2,
            number_negative_pattern: 1,
            number_group_sizes: &[3],
            currency_symbol: "\u{a4}",
            currency_decimal_digits: 2,
            currency_positive_pattern: 0,
            currency_negative_pattern: 0,
            currency_group_sizes: &[3],
            percent_symbol: "%",
            percent_decimal_digits: 2,
            percent_positive_pattern: 0,
            percent_negative_pattern: 0,
            percent_group_sizes: &[3],
        }
    }

    fn conv(f: &str, kind: NumKind) -> Outcome {
        convert(f, &INVARIANT, kind)
    }

    fn code(f: &str) -> String {
        match conv(f, NumKind::Fractional).fmt {
            ExcelNumFmt::Code(c) => c,
            ExcelNumFmt::General => panic!("{f:?}: General"),
        }
    }

    /// A complete code (section 3.2, "the code column is complete").
    fn full(f: &str, want: &str) {
        let o = conv(f, NumKind::Fractional);
        assert_eq!(o.fmt, ExcelNumFmt::Code(want.into()), "{f:?}");
        assert_eq!(o.open, None, "{f:?}");
    }

    /// A `P:` vector: the positive code is fixed, the negative form is O-19: the code is the
    /// positive code alone, or it, `;` and a section that shows `neg` (the negative display).
    /// `explicit`: the form probe P1 shows for the format's class ([NEG_FORM]).
    fn pos_with(f: &str, kind: NumKind, c: &NumCulture, pos: &str, neg: &str, explicit: bool) {
        let o = convert(f, c, kind);
        let ExcelNumFmt::Code(got) = &o.fmt else { panic!("{f:?}: General") };
        assert!(got == pos || *got == format!("{pos};{neg}"), "{f:?}: {got:?}, want {pos:?} or {pos:?};{neg:?}");
        assert!(!o.negative_form, "{f:?}: O-19 is settled for every class");
        assert_eq!(o.open, None, "{f:?}");
        let want = if explicit { format!("{pos};{neg}") } else { pos.to_string() };
        assert_eq!(*got, want, "{f:?}");
    }

    /// One section (F, D, E, P, custom formats).
    fn pos(f: &str, p: &str) {
        pos_with(f, NumKind::Fractional, &INVARIANT, p, &format!("-{p}"), false);
    }

    /// An explicit `-` section (N).
    fn pos_n(f: &str, p: &str) {
        pos_with(f, NumKind::Fractional, &INVARIANT, p, &format!("-{p}"), true);
    }

    fn general(f: &str, kind: NumKind, open: Option<Open>) {
        let o = conv(f, kind);
        assert_eq!(o.fmt, ExcelNumFmt::General, "{f:?}");
        assert_eq!(o.open, open, "{f:?}");
    }

    #[test]
    fn spec_vectors() {
        use NumKind::*;
        // V01 (R-CELL-2)
        general("", Fractional, None);
        // V02-V06 (R-STD-N)
        pos_n("N", "#,##0.00");
        pos_n("N0", "#,##0");
        pos_n("N2", "#,##0.00");
        pos_n("n2", "#,##0.00");
        pos_n("n1", "#,##0.0");
        pos_n("n", "#,##0.00");
        // V07-V09 (R-STD-F)
        pos("F2", "0.00");
        pos("F1", "0.0");
        pos("f", "0.00");
        // V10 (R-STD-C), complete
        full("C", "\"\u{a4}\"#,##0.00;(\"\u{a4}\"#,##0.00)");
        // V11 (R-STD-P): invariant patterns 0 / 0, "n %" and "-n %"
        pos("P1", "#,##0.0 %");
        // V12-V13 (R-STD-E)
        pos("E", "0.000000E+000");
        // `e`: BC writes `E` (T4 probe P1)
        pos("e3", "0.000E+000");
        // V14 (R-STD-G)
        general("G", Fractional, None);
        general("g", Fractional, None);
        // V15: D on a Decimal; .NET throws (R-STD-ERR, O-7), BC writes `#0` (T4 probe P1)
        pos("D", "#0");
        pos("d", "#0");
        // V16 (R-STD-D, Integral)
        pos_with("D5", Integral, &INVARIANT, "00000", "-00000", false);
        // V17 (R-STD-ERR)
        general("T", Fractional, Some(Open::O7));
        general("t", Fractional, Some(Open::O7));
        // V18 (R-STD-X)
        general("X", Integral, Some(Open::O1));
        // V19-V27 (custom, one section)
        pos("#,##0", "#,##0");
        pos("#,##0.00", "#,##0.00");
        pos("0.00", "0.00");
        pos("###0.00", "###0.00");
        pos("#,0.00", "#,0.00");
        pos("#,##0.00###", "#,##0.00###");
        pos("#0.00######", "#0.00######");
        pos("#,#####0.00###", "#,#####0.00###");
        pos("#,##0.#####", "#,##0.#####");
        // V28-V32 (sections)
        full("0.00;(0.00)", "0.00;(0.00)");
        full("#,0.00;(#,0.00)", "#,0.00;(#,0.00)");
        // an empty literal section: BC's spelling `""` (T4 probe P1), which shows nothing
        full("0.00;- 0.00;''", "0.00;- 0.00;\"\"");
        full("0;-0;''", "0;-0;\"\"");
        full("0.00;(0.00);''", "0.00;(0.00);\"\"");
        // V33
        pos("******** #,##0.00", "\"********\" #,##0.00");
        // V34
        full("#,##0.00;#,##0.00-", "#,##0.00;#,##0.00-");
        // V35-V37 (trailing period, scaling)
        pos("#,##0.", "#,##0");
        pos("0,.00", "0.00,");
        pos("#,##0,", "#,##0,");
        // V38 (percent)
        pos("0%", "0%");
        pos("0.0%", "0.0%");
        // V39-V40 (exponent)
        pos("0.00E+00", "0.00E+00");
        pos("0.###E-0", "0.###E-0");
        // V41-V45 (literals)
        pos("\\#0", "\"#\"0");
        pos("'#'0", "\"#\"0");
        pos("0.00\\%", "0.00\"%\"");
        pos("USD #,##0.00", "\"USD\" #,##0.00");
        pos("#,##0.00 kr.", "#,##0.00 \"kr\"");
        pos("0.00 'pcs'", "0.00 \"pcs\"");
        pos("0.00 \"pcs\"", "0.00 \"pcs\"");
        // V46 (T2-BCFMT example)
        full("#,##0.00;(#,##0.00);Zero", "#,##0.00;(#,##0.00);\"Zero\"");
        // V47
        pos("[Red]0", "\"[Red]\"0");
        // V48-V49 (no digit placeholder)
        pos("dd", "\"dd\"");
        pos("abc", "\"abc\"");
        pos("dd-MM-yy", "\"dd\"-\"MM\"-\"yy\"");
        // V50 (per mille)
        general("#0.00\u{2030}", Fractional, Some(Open::O11));
        // V51 (R-LIMIT)
        general(&format!("0{}", "#".repeat(300)), Fractional, None);
        // V52: a culture whose negative sign is U+2212, pattern 1
        pos_with("N2", Fractional, &MINUS, "#,##0.00", "\"\u{2212}\"#,##0.00", true);
        // V53 (empty sections)
        general("0.00;;", Fractional, Some(Open::O10));
        general("0;", Fractional, Some(Open::O10));
        general(";;", Fractional, Some(Open::O10));
        // V54 (a `.` and no placeholder)
        general("Fields!BreakdownAmt3Format.Value", Fractional, Some(Open::O9));
    }

    /// tools/probes escapes: `\\ \t \n \r \xNN`.
    fn unesc(s: &str) -> String {
        let mut out = String::new();
        let mut cs = s.chars();
        while let Some(c) = cs.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match cs.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('x') => {
                    let h: String = cs.by_ref().take(2).collect();
                    out.push(u8::from_str_radix(&h, 16).map(char::from).unwrap_or('?'));
                }
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        }
        out
    }

    /// Probe P1 on a real BC 29.1, compacted (tests/vectors/xlsx-numfmt/u2-p1-compact.tsv, whose
    /// header says how): each row's `expect` says how the converter's code relates to BC's.
    #[test]
    fn p1_vectors() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/vectors/xlsx-numfmt/u2-p1-compact.tsv");
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut bad = vec![];
        let mut n = 0;
        // header comments (`# `) come before the column line; formats may start with `#`
        let body = text.lines().skip_while(|l| !l.starts_with("format\t")).skip(1);
        for line in body {
            let f: Vec<String> = line.split('\t').map(unesc).collect();
            let [format, kind, bc, expect] = &f[..] else { panic!("bad vector line {line:?}") };
            n += 1;
            let kind = if kind == "Integral" { NumKind::Integral } else { NumKind::Fractional };
            let o = convert(format, &INVARIANT, kind);
            let got = match &o.fmt {
                ExcelNumFmt::General => "General".to_string(),
                ExcelNumFmt::Code(c) => c.clone(),
            };
            let ok = match expect.as_str() {
                "=" => got == *bc,
                // same display, other spelling (R-CANON): a code with as many sections
                "spelling" => matches!(o.fmt, ExcelNumFmt::Code(_)) && well_formed(&got).ok() == well_formed(bc.split(" | ").next().unwrap_or("")).ok(),
                // a documented fallback: General with its open item
                e if e.starts_with("open:") => o.fmt == ExcelNumFmt::General && o.open.is_some_and(|x| x.id() == &e[5..]),
                // BC writes an error cell; a number format can't
                "error" => o.fmt == ExcelNumFmt::General && o.open.is_some(),
                // needs the culture's data (O-3: BC's "invariant" textbox culture is en-US)
                "culture" => true,
                // BC's code shows something other than the .NET display the converter follows
                // (R-CANON says the P1 code wins; open): only well-formed
                "differs" => match &o.fmt {
                    ExcelNumFmt::General => true,
                    ExcelNumFmt::Code(c) => well_formed(c).is_ok(),
                },
                _ => false,
            };
            if !ok {
                bad.push(format!("{format:?}\t{kind:?}\tbc {bc:?}\tgot {got:?}\topen {:?}\texpect {expect:?}", o.open));
            }
        }
        assert!(n > 600, "{n} vectors");
        assert!(bad.is_empty(), "{} of {n} vectors:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn t1_vector_code_keeps_its_decimals_in_30_characters() {
        // T1-CBG: an amount column's cell code contains `0.00` within the 30 characters BC's
        // reader keeps (T1-RV, T1-XB); the strings the XFormat field holds are O-15, so this
        // checks the usual ones
        for f in ["#,##0.00", "N2", "#,##0.00;-#,##0.00", "C2"] {
            let c = code(f);
            assert!(c.chars().take(30).collect::<String>().contains("0.00"), "{f:?}: {c:?}");
        }
    }

    #[test]
    fn open_items_fall_back_to_general() {
        use NumKind::*;
        for (f, kind, open) in [
            ("G5", Fractional, Open::O1),
            ("G0", Fractional, Open::O1),
            ("x", Integral, Open::O1),
            ("B", Integral, Open::O1),
            ("Q", Fractional, Open::O7),
            ("N1000000000", Fractional, Open::O7),
            ("N99999999999999999999999", Fractional, Open::O7),
            ("\u{c4}", Fractional, Open::O7),
            (" N2", Fractional, Open::O7),
            ("N2 ", Fractional, Open::O7),
            ("'abc", Fractional, Open::O7),
            ("0\"x", Fractional, Open::O7),
            ("0\\", Fractional, Open::O7),
            ("0\u{1}", Fractional, Open::O7),
            ("%", Fractional, Open::O7),
            ("0;0;0;0", Fractional, Open::O10),
            (";0", Fractional, Open::O10),
            ("0%%", Fractional, Open::O11),
            (",0", Fractional, Open::O12),
            ("0.0,0", Fractional, Open::O12),
            ("0,\"x\".00", Fractional, Open::O12),
            ("a,b", Fractional, Open::O12),
            ("E+0", Fractional, Open::O13),
            ("e0 x", Fractional, Open::O13),
            ("#0.0e0", Fractional, Open::O13),
            ("#,##0.0E+0", Fractional, Open::O13),
            ("0E+0E+0", Fractional, Open::O13),
        ] {
            let o = conv(f, kind);
            assert_eq!((o.fmt, o.open), (ExcelNumFmt::General, Some(open)), "{f:?}");
        }
        // R: General on both column types, no assume hit (T4 probe P1)
        general("R", Fractional, None);
        general("r", Integral, None);
        general("R15", Integral, None);
        // an `E` not followed by zeros is a literal (R-CUS-TOK), not an open exponent; `E0` is
        // the standard E with precision 0
        pos("0.0E", "0.0\"E\"");
        pos("E0", "0E+000");
    }

    #[test]
    fn standard_patterns_follow_the_culture() {
        // NumberNegativePattern 0, 2-4: an explicit section, no O-19 choice
        for (p, neg) in [(0u8, "(#,##0.00)"), (2, "- #,##0.00"), (3, "#,##0.00-"), (4, "#,##0.00 -")] {
            let c = NumCulture { number_negative_pattern: p, ..copy_invariant() };
            let o = convert("N", &c, NumKind::Fractional);
            assert_eq!(o.fmt, ExcelNumFmt::Code(format!("#,##0.00;{neg}")), "pattern {p}");
            assert!(!o.negative_form && o.culture_used);
        }
        // currency: symbol quoted, patterns from the tables; "sign, then the positive pattern"
        // only for (0,1), (1,5), (2,9), (3,8)
        let c = NumCulture { currency_symbol: "kr.", currency_positive_pattern: 3, currency_negative_pattern: 8, currency_decimal_digits: 0, ..copy_invariant() };
        let o = convert("C", &c, NumKind::Fractional);
        assert_eq!(o.fmt, ExcelNumFmt::Code("#,##0 \"kr.\";-#,##0 \"kr.\"".into()));
        assert!(!o.negative_form);
        let c = NumCulture { currency_symbol: "$", currency_positive_pattern: 0, currency_negative_pattern: 16, ..copy_invariant() };
        assert_eq!(convert("C1", &c, NumKind::Fractional).fmt, ExcelNumFmt::Code("$#,##0.0;$- #,##0.0".into()));
        // percent
        let c = NumCulture { percent_positive_pattern: 1, percent_negative_pattern: 11, ..copy_invariant() };
        assert_eq!(convert("P0", &c, NumKind::Fractional).fmt, ExcelNumFmt::Code("#,##0%;#,##0- %".into()));
        let c = NumCulture { percent_symbol: "\u{2030}", ..copy_invariant() };
        assert_eq!(convert("P", &c, NumKind::Fractional).open, Some(Open::O11));
        // R-CUL-5: grouping by three whatever the group sizes
        let c = NumCulture { number_group_sizes: &[3, 2], ..copy_invariant() };
        assert_eq!(convert("N0", &c, NumKind::Fractional).fmt, ExcelNumFmt::Code("#,##0;-#,##0".into()));
        // precision defaults come from the culture
        let c = NumCulture { number_decimal_digits: 3, ..copy_invariant() };
        assert_eq!(convert("F", &c, NumKind::Fractional).fmt, ExcelNumFmt::Code("0.000".into()));
    }

    #[test]
    fn culture_is_read_only_where_it_matters() {
        // two or three sections: no culture data, no O-19
        let o = conv("0.00;(0.00)", NumKind::Fractional);
        assert!(!o.culture_used && !o.negative_form);
        assert!(!conv("G", NumKind::Fractional).culture_used);
        assert!(conv("#,##0", NumKind::Fractional).culture_used);
        // unknown names resolve to the invariant culture (with an assume hit)
        assert_eq!(culture("fr-FR"), &INVARIANT);
        assert_eq!(culture(""), &INVARIANT);
        crate::assume::reset();
        let (f, used) = to_excel_in("0.00;(0.00)", "de-DE", NumKind::Fractional);
        assert_eq!((f, used), (ExcelNumFmt::Code("0.00;(0.00)".into()), false));
        assert!(crate::assume::take().assumed.is_empty());
        let (_, used) = to_excel_in("N2", "de-DE", NumKind::Fractional);
        assert!(used);
        let names: Vec<&str> = crate::assume::take().assumed.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["U2.CultureData"]);
        to_excel("Q", &INVARIANT, NumKind::Fractional);
        let names: Vec<&str> = crate::assume::take().assumed.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["U2.OpenFormat", "U2.OpenFormat:O-7"]);
    }

    #[test]
    fn literals_never_become_codes() {
        // W-LITERAL: characters that mean something in Excel are quoted or escaped
        assert_eq!(code("0 dmyhsEeG?_*@[]/:"), "0 \"dmyhsEeG?_*@[]/:\"");
        assert_eq!(code("0 '1,2'"), "0 \"1,2\"");
        assert_eq!(code("0 '\"'"), "0 \\\"");
        assert_eq!(code("0'\\'"), "0\\\\");
        assert_eq!(code("$(0)+"), "$(0)+");
        // an empty literal adds nothing; a whitespace format is custom (W-TRIM)
        assert_eq!(code("0''"), "0");
        assert_eq!(code("  "), "  ");
        // a format of one empty literal shows nothing
        assert_eq!(code("''"), "\"\"");
    }

    #[test]
    fn long_formats_are_bounded() {
        // up to MAX_FORMAT characters a format is converted; one more gives General
        let ok = format!("0{}", ",".repeat(MAX_FORMAT - 1));
        assert_eq!(conv(&ok, NumKind::Fractional).open, None, "a code that long is General by R-LIMIT, with no open item");
        let long = format!("0{}", ",".repeat(MAX_FORMAT));
        assert_eq!(conv(&long, NumKind::Fractional).open, Some(Open::Length));
        // the comma scan is linear: 60,000 commas stop at the cap, and just under it many scaling
        // or grouping commas still convert quickly
        let t = std::time::Instant::now();
        for f in [format!("0{}", ",".repeat(60_000)), "#,".repeat(30_000) + "0", format!("0{}.0", ",".repeat(MAX_FORMAT - 4)), "#,".repeat(511) + "0"] {
            let o = conv(&f, NumKind::Fractional);
            assert_eq!(o.fmt, ExcelNumFmt::General, "{}", f.len());
        }
        assert!(t.elapsed() < std::time::Duration::from_millis(200), "{:?}", t.elapsed());
    }

    #[test]
    fn grouping_scaling_and_dots() {
        pos("#,##,##0", "#,####0");
        pos("#,,##0", "#,##0");
        pos("0,, \"M\"", "0,, \"M\"");
        pos("#,##0,,.0", "#,##0.0,,");
        pos("0.00.00", "0.0000");
        pos(".00", ".00");
        pos("0.", "0");
        pos("00.##", "00.##");
        pos("#", "#");
    }

    /// Formats for the property test: the literals of section 3.1, standard letters across
    /// precisions, custom and malformed strings.
    fn formats() -> Vec<String> {
        let mut v: Vec<String> = [
            "d", "N", "g", "D", "0.00;(0.00)", "f", "#,##0", "#,##0.00", "N2", "T", "#,0.00;(#,0.00)", "dd-MM-yy", "dd-MM-yyyy", "0.00", "n2", "N0", "#,##0.00###", "F2", "t", "0.00;- 0.00;''", "###0.00",
            "#,0.00", "MM/dd/yyyy", "MM-dd-yyyy", "dd-mm-yy", "d. MMMM yyyy", "Fields!Prepayment_Inv__Line_Buffer__Amount_Control160Format.Value", "#0.00######", "******** #,##0.00", "n1", "n", "#,#####0.00###",
            "#,##0.#####", "dd/MM/yy", "d MMMM yyyy", "F1", "0;-0;''", "M/d/yyyy", "Fields!BreakdownAmt3Format.Value", "dd.MM.yy", "dd.MM.yyyy", "0.00;(0.00);''", "", " ", ";", ";;", ";;;", "0;0;0;0;0", "'", "\"", "\\",
            "0\\", "'abc", "E", "e+", "E-0", "0E", "0e+", "0E+0", "#E0", "#,#E+0", "0,E0", "\u{2030}", "%%", "0%%", "0\u{2030}", "\u{c4}", "\u{c4}2", "N 2", " N2", "N2 ", "\t", "0\t0", "0\n", "\u{0}", "\u{ffff}", "0\"\"0",
            "0''0", "''", "\"\"", "0;''", "'';0", "#,##0;(#,##0);-", "[Red]#,##0;[Blue]-#,##0", "0.0,", ",", ",,", ".", "..", "0..0", "0,.,0", "#,##0.00 \u{20ac}", "\u{20ac} #,##0.00", "0 \"a\\\"b\"",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for l in b'A'..=b'Z' {
            for case in [l, l.to_ascii_lowercase()] {
                for p in ["", "0", "1", "2", "5", "15", "99", "250", "254", "255", "300", "999999999", "1000000000", "000000000000000000000002", "99999999999999999999"] {
                    v.push(format!("{}{p}", case as char));
                }
            }
        }
        v.push("0".repeat(254));
        v.push("0".repeat(255));
        v.push(format!("0.{}", "#".repeat(260)));
        v.push(format!("'{}'0", "x".repeat(250)));
        v
    }

    fn cultures() -> Vec<NumCulture> {
        let mut v = vec![copy_invariant(), NumCulture { negative_sign: "\u{2212}", ..copy_invariant() }];
        for p in 0..=16u8 {
            v.push(NumCulture { currency_negative_pattern: p, currency_positive_pattern: p % 4, percent_negative_pattern: p % 12, percent_positive_pattern: (p + 1) % 4, number_negative_pattern: p % 5, currency_symbol: ["$", "kr.", "\u{20ac}", "\"", "CHF"][p as usize % 5], ..copy_invariant() });
        }
        v.push(NumCulture { percent_symbol: "\u{66a}", number_decimal_digits: 0, currency_decimal_digits: 3, percent_decimal_digits: 1, number_group_sizes: &[3, 2], ..copy_invariant() });
        // out-of-range pattern data must not panic either
        v.push(NumCulture { number_negative_pattern: 9, currency_positive_pattern: 9, currency_negative_pattern: 99, percent_positive_pattern: 7, percent_negative_pattern: 40, ..copy_invariant() });
        v
    }

    /// Outside quotes: `;` count, quotes balanced, no dangling backslash.
    fn well_formed(code: &str) -> Result<usize, String> {
        let mut sections = 1;
        let mut quoted = false;
        let mut cs = code.chars();
        while let Some(c) = cs.next() {
            match c {
                '"' => quoted = !quoted,
                '\\' if !quoted => {
                    cs.next().ok_or("dangling backslash")?;
                }
                ';' if !quoted => sections += 1,
                _ => {}
            }
        }
        if quoted {
            return Err("unbalanced quotes".into());
        }
        Ok(sections)
    }

    #[test]
    fn properties_over_formats_cultures_and_kinds() {
        let cultures = cultures();
        for f in formats() {
            for c in &cultures {
                for kind in [NumKind::Integral, NumKind::Fractional] {
                    let a = std::panic::catch_unwind(|| convert(&f, c, kind)).unwrap_or_else(|_| panic!("{f:?} panicked"));
                    let b = convert(&f, c, kind);
                    assert_eq!(a, b, "{f:?}: not deterministic");
                    if let ExcelNumFmt::Code(code) = &a.fmt {
                        assert!(!code.is_empty(), "{f:?}: empty code");
                        assert!(code.encode_utf16().count() < 255, "{f:?}: {} characters", code.len());
                        let n = well_formed(code).unwrap_or_else(|e| panic!("{f:?} -> {code:?}: {e}"));
                        assert!(n <= 4, "{f:?} -> {code:?}: {n} sections");
                        assert!(!code.chars().any(xml_invalid), "{f:?}");
                        assert!(a.open.is_none());
                    }
                }
            }
        }
    }
}
