//! AL DateFormula: reading formula text (Evaluate, CalcDate with a Text formula), the stored
//! form, Format, and CalcDate.
//!
//! Sources, cited per rule (rule ids R.. and requirements Q.. are those of the U1 spec):
//! - Learn: CalcDate method <https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/methods-auto/system/system-calcdate-string-date-method>,
//!   DateFormula data type <https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/methods-auto/dateformula/dateformula-data-type>,
//!   "Enter date ranges" <https://learn.microsoft.com/en-us/dynamics365/business-central/ui-enter-date-ranges>;
//! - the AL Language.Tests corpus (`LT <codeunit> <test>`), measured on a real BC 29.0;
//! - BCApps tests (file names below);
//! - our probes P01..P20 on a real BC 29.1 (`T4:Pnn`, tools/probes probe u1-dateformula); a compact
//!   selection of their rows is crates/al-runtime/tests/vectors/u1-dateformula.tsv, which
//!   crates/al-runtime/tests/u1_vectors.rs replays.
//!
//! What no source settles returns [DfError::Unsettled] with the probe that will settle it; callers
//! raise it as `U1 unsettled: Pnn`. The reader's character classes ([class]), the letter rows per
//! language ([ROWS], [INVARIANT_LANGS]) and the number ranges ([position_range]) are tables.
//!
//! Nothing here is global or thread state: the session language is an argument of every read
//! and format call (Q5), and the parse cache ([Cache]) belongs to the session (Q7).
//!
//! Line breaks in the messages: the corpus (BC 29.0, LT 73800 Invalid_Formula_Evaluate) records
//! a `\` for each line break, the probes on BC 29.1 record CR LF. The texts keep the corpus form,
//! as the U1 spec (2.9, Q11) asks.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};

/// The language whose letters are the invariant ones (R56: `C D WD W M Q Y`).
pub const ENGLISH: i32 = 1033;
/// German, the one other language with an evidenced text for a Format error (T4:P15fmt).
const GERMAN: i32 = 1031;
/// Longer input is refused (T4:P10: 32 characters accepted, 33 refused, whatever they hold).
pub const MAX_INPUT_LEN: usize = 32;

/// The probes of the U1 spec (section 4) whose points are still open after the probe runs (T4);
/// the others are rules now. Each one is a row of `assume::REGISTRY` (`assume_name`), so every
/// probe [note_assumed] can record is registered (tested below).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Probe {
    /// characters of an unsettled class
    P09,
    /// a text longer than 32 characters that also fails to read, or the case of its `%1`
    P10,
    /// letters of a language that are not measured (Vietnamese C and D), unknown language ids
    P11,
    /// input in such a language (Evaluate)
    P12,
    /// error texts outside English
    P13,
    /// the order of different DateFormula values
    P14,
    /// a Format length with a format number other than 0, a format string
    P15,
    /// input in such a language (CalcDate with a Text formula)
    P17,
}

impl Probe {
    /// Every probe, in order.
    pub const ALL: [Probe; 8] = [Probe::P09, Probe::P10, Probe::P11, Probe::P12, Probe::P13, Probe::P14, Probe::P15, Probe::P17];

    pub fn id(self) -> &'static str {
        match self {
            Probe::P09 => "P09",
            Probe::P10 => "P10",
            Probe::P11 => "P11",
            Probe::P12 => "P12",
            Probe::P13 => "P13",
            Probe::P14 => "P14",
            Probe::P15 => "P15",
            Probe::P17 => "P17",
        }
    }

    /// What the probe covers (shown after the id in the runtime error).
    pub fn what(self) -> &'static str {
        match self {
            Probe::P09 => "a character of the date formula reader",
            Probe::P10 => "a date formula longer than 32 characters",
            Probe::P11 => "date formula letters of the session language",
            Probe::P12 => "date formula input in the session language",
            Probe::P13 => "date formula error texts outside English",
            Probe::P14 => "the order of DateFormula values",
            Probe::P15 => "Format and Evaluate numbers of a DateFormula",
            Probe::P17 => "CalcDate(Text) in the session language",
        }
    }

    /// Its row in `assume::REGISTRY`.
    pub fn assume_name(self) -> &'static str {
        match self {
            Probe::P09 => "DateFormula.P09",
            Probe::P10 => "DateFormula.P10",
            Probe::P11 => "DateFormula.P11",
            Probe::P12 => "DateFormula.P12",
            Probe::P13 => "DateFormula.P13",
            Probe::P14 => "DateFormula.P14",
            Probe::P15 => "DateFormula.P15",
            Probe::P17 => "DateFormula.P17",
        }
    }
}

impl fmt::Display for Probe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// The start of the runtime error for an unsettled point (spec section 4).
pub const UNSETTLED_PREFIX: &str = "U1 unsettled: ";

/// The runtime error text for an unsettled point: `U1 unsettled: P13 (...)`.
pub fn unsettled_text(p: Probe) -> String {
    format!("{UNSETTLED_PREFIX}{} ({})", p.id(), p.what())
}

/// Record that the running test reached an unsettled point (its row in `assume::REGISTRY`).
pub fn note_assumed(p: Probe) {
    crate::assume::hit(p.assume_name());
}

// ------------------------------------------------------------------------------------ model

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Unit {
    D,
    WD,
    W,
    M,
    Q,
    Y,
}

impl Unit {
    const ALL: [Unit; 6] = [Unit::D, Unit::WD, Unit::W, Unit::M, Unit::Q, Unit::Y];

    fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Sign {
    /// no sign written (only the first term may have none, R28)
    None,
    Plus,
    Minus,
}

/// A number as written: its value and its digit count (leading zeros stay in the stored text,
/// T4:P01 `0001D`, T4:P02 `Q00001`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Num {
    pub value: u16,
    pub digits: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    /// `30D`: number then unit
    Count(Num),
    /// `D15`, `WD2`: unit then number
    Position(Num),
    /// `CM`: C then unit
    Current,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Term {
    pub sign: Sign,
    pub unit: Unit,
    pub kind: Kind,
}

/// The most terms 32 characters can hold: the shortest term is 2 characters, 3 with its sign.
const CAP: usize = 1 + (MAX_INPUT_LEN - 2) / 3;

const NO_TERM: Term = Term { sign: Sign::None, unit: Unit::D, kind: Kind::Current };

/// A parsed date formula: its terms in input order (R44, R70). `Copy`, no heap.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Formula {
    len: u8,
    terms: [Term; CAP],
}

impl Default for Formula {
    fn default() -> Self {
        Formula::EMPTY
    }
}

impl Formula {
    /// The empty formula (R1, R31, R62).
    pub const EMPTY: Formula = Formula { len: 0, terms: [NO_TERM; CAP] };

    pub fn terms(&self) -> &[Term] {
        &self.terms[..self.len as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The stored text (R44): invariant letters, no spaces or brackets, terms in input order,
    /// the first term's sign only if written, numbers with their leading zeros (T4:P01).
    pub fn stored(&self) -> String {
        let mut s = String::with_capacity(MAX_INPUT_LEN);
        // the invariant row has every letter
        let _ = render(self, &INVARIANT, &mut s);
        s
    }

    /// The text with the letters of session language `lang` (format 0, R50).
    pub fn localized(&self, lang: i32) -> Result<String, DfError> {
        let row = row(lang).ok_or(DfError::Unsettled(Probe::P11))?;
        let mut s = String::with_capacity(MAX_INPUT_LEN + 8);
        render(self, row, &mut s)?;
        Ok(s)
    }

    /// A storable key to sort DateFormula values by (Q6). BC orders DateFormula keys differently
    /// in SQL and in temporary tables (T4:P14order-sql, P14order-temp), by a form this project
    /// does not store: no key yet.
    pub fn sort_key(&self) -> Result<SortKey, DfError> {
        Err(DfError::Unsettled(Probe::P14))
    }

    fn push(&mut self, t: Term) {
        if (self.len as usize) < CAP {
            self.terms[self.len as usize] = t;
            self.len += 1;
        }
    }
}

/// A storable sort key for DateFormula values (Q6; none exists yet, P14).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct SortKey(pub Box<[u8]>);

// ------------------------------------------------------------------------------------ errors

/// The reading errors (spec 2.9 E1..E4, and the length error of T4:P10).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadKind {
    /// E1 "should include a time unit"
    NoUnit,
    /// E2 "should include a number"
    NoNumber,
    /// E3 "must include plus (+) or minus (-)"
    NoSign,
    /// E4 "contains a number that is too large", with the range
    TooLarge { min: u16, max: u16 },
    /// "exceeds the maximum length of 32" (T4:P10)
    TooLong,
}

/// Every failure of this module. Codes: [DfError::code].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DfError {
    /// A refused text, code `Evaluate`; `quoted` is the `%1` of the message, or the probe that
    /// settles it.
    Read { kind: ReadKind, quoted: Result<Box<str>, Probe> },
    /// The text is refused for certain (Evaluate returns false), but which message is open.
    Refused(Probe),
    /// E5, code `Argument`: CalcDate on 0D; `formula` is the formula in the session's letters
    /// (T4:P07).
    UndefinedDate { formula: Box<str> },
    /// E6, code `DateInvalid`.
    DateInvalid,
    /// A `Wn` step into ISO week-year 10000, code `DateInvalid` (T4:P06 `<W1>` on 9999-12-01);
    /// `formula` is the stored text.
    WeekBeyondRange { formula: Box<str> },
    /// `Format(DF, 0, n)` for n other than 0, 1, 2, 9, code `InvalidStandardFormat` (R54, T4:P15fmt;
    /// deviation D1).
    InvalidStandardFormat(i64),
    /// No source settles this input yet.
    Unsettled(Probe),
}

impl DfError {
    /// GetLastErrorCode of the error; empty for an unsettled point (a refusal of bc-rust).
    pub fn code(&self) -> &'static str {
        match self {
            DfError::Read { .. } => "Evaluate",
            DfError::UndefinedDate { .. } => "Argument",
            DfError::DateInvalid | DfError::WeekBeyondRange { .. } => "DateInvalid",
            DfError::InvalidStandardFormat(_) => "InvalidStandardFormat",
            DfError::Refused(_) | DfError::Unsettled(_) => "",
        }
    }

    /// Whether the text was refused as a formula: Evaluate's return-value form gives false (R40).
    pub fn is_refusal(&self) -> bool {
        matches!(self, DfError::Read { .. } | DfError::Refused(_))
    }

    /// The error text in session language `lang` (2.9, Q11: verbatim, `\` for each line break), or
    /// the probe that settles it.
    ///
    /// Outside English the texts are open (P13): the probes saw the English date formula texts
    /// in a German session (T4:P13eval) and German ones in another German session (T4:P15eval).
    pub fn message(&self, lang: i32) -> Result<String, Probe> {
        let q = match self {
            DfError::Refused(p) | DfError::Unsettled(p) => return Err(*p),
            DfError::Read { quoted: Err(p), .. } => return Err(*p),
            DfError::Read { quoted: Ok(q), .. } => q.as_ref(),
            _ => "",
        };
        if let DfError::InvalidStandardFormat(n) = self {
            // R54: BC names the type 'Duration' here; this project names 'DateFormula' (deviation D1)
            return match lang {
                ENGLISH => Ok(format!("Standard format number {n} does not exist for the type 'DateFormula'.")),
                GERMAN => Ok(format!("Die Standardformatnummer {n} ist für den Typ 'DateFormula' nicht vorhanden.")),
                _ => Err(Probe::P13),
            };
        }
        if lang != ENGLISH {
            return Err(Probe::P13);
        }
        Ok(match self {
            DfError::Read { kind, .. } => match kind {
                // LT 73800 Invalid_Formula_Evaluate (abc); T4:P13eval
                ReadKind::NoUnit => format!(
                    "The date formula '{q}' should include a time unit.\\\\Time units can be: D,WD,W,M,Q, or Y.\\C, meaning current time unit based on date, can be specified as a prefix to the time unit."
                ),
                // LT 73800 Invalid_Formula_Evaluate (D alone); BCApps ERMAccountSchedule.Codeunit.al:32
                ReadKind::NoNumber => format!("The date formula '{q}' should include a number."),
                // LT 73800 Invalid_Formula_Evaluate (CM1)
                ReadKind::NoSign => format!("The date formula '{q}' must include plus (+) or minus (-)."),
                // LT 67857 SG_DayOfMonth_EdgeNumbersAndEvaluate, LT 73146 P128_CalcDate_DateTextIsNotAFormula
                ReadKind::TooLarge { min, max } => {
                    format!("The date formula '{q}' contains a number that is too large.\\\\The number must be in the range {min} - {max}.")
                }
                // T4:P10
                ReadKind::TooLong => format!("The date formula '{q}' exceeds the maximum length of {MAX_INPUT_LEN}."),
            },
            // LT 73800 CalcDate_Undefined_Base; BCApps ERMPurchDocReports.Codeunit.al:28; T4:P07
            DfError::UndefinedDate { formula } => {
                format!("You cannot base a date calculation on an undefined date.\\\\Date: 0D\\Formula: {formula}.")
            }
            // LT 73800 CalcDate_Text_Overload_Errors; T4:P06
            DfError::DateInvalid => "The date is not valid.".to_string(),
            // T4:P06
            DfError::WeekBeyondRange { formula } => format!(
                "The calculation of the date formula {formula} resulted in an invalid date.\\\\Only dates between Jan. 1, 0000 - Dec. 31, 9999 may be used."
            ),
            DfError::InvalidStandardFormat(_) | DfError::Refused(_) | DfError::Unsettled(_) => String::new(),
        })
    }

    /// The probe an unsettled result waits for.
    pub fn probe(&self) -> Option<Probe> {
        match self {
            DfError::Refused(p) | DfError::Unsettled(p) | DfError::Read { quoted: Err(p), .. } => Some(*p),
            _ => None,
        }
    }
}

impl fmt::Display for DfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message(ENGLISH) {
            Ok(m) => f.write_str(&m),
            Err(p) => f.write_str(&unsettled_text(p)),
        }
    }
}

/// One character upper-cased; characters whose upper case is longer stay as they are.
fn upper(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// `%1` of a reading error (2.9, T4:P13): the text as written, upper-cased, outer spaces kept,
/// one pair of outer angle brackets removed when the text starts with `<` and ends with `>`.
/// Built on the error path only (Q7).
fn quote(input: &str) -> Box<str> {
    let up: String = input.chars().map(upper).collect();
    let inner = if bracketed(&up) { &up[1..up.len() - 1] } else { &up[..] };
    inner.into()
}

fn read_err(input: &str, kind: ReadKind) -> DfError {
    DfError::Read { kind, quoted: Ok(quote(input)) }
}

// ------------------------------------------------------------------------------------ letters

/// The letters of one language. `None`: not evidenced (P11 for Format, P12/P17 for input).
pub struct Row {
    pub lang: i32,
    /// the "current" prefix (`C` in English)
    pub c: Option<&'static str>,
    /// per unit, in [Unit] order (D, WD, W, M, Q, Y)
    pub units: [Option<&'static str>; 6],
}

impl Row {
    fn complete(&self) -> bool {
        self.c.is_some() && self.units.iter().all(|u| u.is_some())
    }

    fn unit(&self, u: Unit) -> Option<&'static str> {
        self.units[u.idx()]
    }
}

/// The invariant letters: the stored text, bracketed input, English and most languages (R35,
/// R56, T4:P11all).
pub static INVARIANT: Row = Row { lang: ENGLISH, c: Some("C"), units: [Some("D"), Some("WD"), Some("W"), Some("M"), Some("Q"), Some("Y")] };

/// The languages with letters of their own (T4:P11all, T4:P11; checked against Learn: Danish
/// `-1U`, German `1J`, French `1S+1J`, Spanish `1S+1D`, and BCApps RetentionPeriodTest).
pub static ROWS: &[Row] = &[
    // generated from T4:P11all (Format of <CD+1WD+1W+1M+1Q+1Y>) and T4:P11 (<1D>); a two-letter CD splits one and one
    Row { lang: 1026, c: Some("Н"), units: [Some("Д"), Some("ДС"), Some("С"), Some("М"), Some("Т"), Some("Г")] }, // BGR
    Row { lang: 1029, c: Some("B"), units: [Some("D"), Some("DT"), Some("T"), Some("M"), Some("K"), Some("R")] }, // CSY
    Row { lang: 1030, c: Some("L"), units: [Some("D"), Some("UD"), Some("U"), Some("M"), Some("K"), Some("Å")] }, // DAN
    Row { lang: 1031, c: Some("L"), units: [Some("T"), Some("WT"), Some("W"), Some("M"), Some("Q"), Some("J")] }, // DEU
    Row { lang: 1032, c: Some("Τ"), units: [Some("Η"), Some("ΗΕ"), Some("Ε"), Some("Μ"), Some("ΤΡ"), Some("Χ")] }, // ELL
    Row { lang: 1034, c: Some("P"), units: [Some("D"), Some("DS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // ESP
    Row { lang: 1035, c: Some("N"), units: [Some("P"), Some("VP"), Some("VI"), Some("K"), Some("Q"), Some("V")] }, // FIN
    Row { lang: 1036, c: Some("F"), units: [Some("J"), Some("JS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // FRA
    Row { lang: 1038, c: Some("A"), units: [Some("N"), Some("MN"), Some("HÉ"), Some("H"), Some("NÉ"), Some("É")] }, // HUN
    Row { lang: 1039, c: Some("L"), units: [Some("D"), Some("VD"), Some("V"), Some("M"), Some("F"), Some("Á")] }, // ISL
    Row { lang: 1040, c: Some("C"), units: [Some("G"), Some("GS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // ITA
    Row { lang: 1043, c: Some("L"), units: [Some("D"), Some("WD"), Some("W"), Some("M"), Some("K"), Some("J")] }, // NLD
    Row { lang: 1044, c: Some("N"), units: [Some("D"), Some("UD"), Some("U"), Some("M"), Some("K"), Some("Å")] }, // NOR
    Row { lang: 1045, c: Some("B"), units: [Some("D"), Some("DT"), Some("T"), Some("M"), Some("K"), Some("R")] }, // PLK
    Row { lang: 1046, c: Some("C"), units: [Some("D"), Some("DS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // PTB
    Row { lang: 1048, c: Some("C"), units: [Some("Z"), Some("ZS"), Some("S"), Some("L"), Some("T"), Some("A")] }, // ROM
    Row { lang: 1049, c: Some("Т"), units: [Some("Д"), Some("БД"), Some("Н"), Some("М"), Some("К"), Some("Г")] }, // RUS
    Row { lang: 1050, c: Some("S"), units: [Some("D"), Some("RD"), Some("T"), Some("M"), Some("K"), Some("G")] }, // HRV
    Row { lang: 1051, c: Some("A"), units: [Some("D"), Some("PD"), Some("T"), Some("M"), Some("Š"), Some("R")] }, // SKY
    Row { lang: 1053, c: Some("L"), units: [Some("D"), Some("VD"), Some("V"), Some("M"), Some("K"), Some("Å")] }, // SVE
    Row { lang: 1055, c: Some("GÇ"), units: [Some("G"), Some("HG"), Some("H"), Some("A"), Some("Ç"), Some("Y")] }, // TRK
    Row { lang: 1058, c: Some("П"), units: [Some("Д"), Some("ДТ"), Some("Т"), Some("М"), Some("К"), Some("Р")] }, // UKR
    Row { lang: 1060, c: Some("S"), units: [Some("D"), Some("DT"), Some("T"), Some("M"), Some("Č"), Some("L")] }, // SLV
    Row { lang: 1061, c: Some("J"), units: [Some("P"), Some("NP"), Some("N"), Some("K"), Some("KV"), Some("A")] }, // ETI
    Row { lang: 1062, c: Some("P"), units: [Some("D"), Some("ND"), Some("N"), Some("M"), Some("C"), Some("G")] }, // LVI
    Row { lang: 1063, c: Some("E"), units: [Some("D"), Some("SD"), Some("S"), Some("M"), Some("K"), Some("T")] }, // LTH
    Row { lang: 1066, c: None, units: [None, Some("NTT"), Some("T"), Some("TH"), Some("Q"), Some("N")] }, // VIT
    Row { lang: 2055, c: Some("L"), units: [Some("T"), Some("WT"), Some("W"), Some("M"), Some("Q"), Some("J")] }, // DES
    Row { lang: 2058, c: Some("P"), units: [Some("D"), Some("DS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // ESM
    Row { lang: 2060, c: Some("F"), units: [Some("J"), Some("JS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // FRB
    Row { lang: 2064, c: Some("C"), units: [Some("G"), Some("GS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // ITS
    Row { lang: 2067, c: Some("L"), units: [Some("D"), Some("WD"), Some("W"), Some("M"), Some("K"), Some("J")] }, // NLB
    Row { lang: 2070, c: Some("C"), units: [Some("D"), Some("DS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // PTG
    Row { lang: 3079, c: Some("L"), units: [Some("T"), Some("WT"), Some("W"), Some("M"), Some("Q"), Some("J")] }, // DEA
    Row { lang: 3082, c: Some("P"), units: [Some("D"), Some("DS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // ESN
    Row { lang: 3084, c: Some("F"), units: [Some("J"), Some("JS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // FRC
    Row { lang: 4108, c: Some("F"), units: [Some("J"), Some("JS"), Some("S"), Some("M"), Some("T"), Some("A")] }, // FRS
    Row { lang: 9242, c: Some("T"), units: [Some("D"), Some("RD"), Some("N"), Some("M"), Some("K"), Some("G")] }, // SRM
];

/// Languages whose letters are the invariant ones (T4:P11all, and 2074 from T4:P11/P12), 210 ids.
pub static INVARIANT_LANGS: &[i32] = &[
    1025, 1027, 1028, 1033, 1037, 1041, 1042, 1047, 1052, 1054, 1056, 1057, 1059, 1064,
    1065, 1067, 1068, 1069, 1070, 1071, 1072, 1073, 1074, 1076, 1077, 1078, 1079, 1080,
    1081, 1082, 1083, 1086, 1087, 1088, 1089, 1090, 1091, 1092, 1093, 1094, 1095, 1096,
    1097, 1098, 1099, 1100, 1101, 1102, 1103, 1104, 1105, 1106, 1107, 1108, 1109, 1110,
    1111, 1113, 1114, 1115, 1116, 1117, 1118, 1119, 1121, 1122, 1123, 1124, 1125, 1127,
    1128, 1130, 1131, 1132, 1133, 1134, 1135, 1136, 1138, 1139, 1140, 1141, 1143, 1144,
    1146, 1148, 1150, 1152, 1153, 1154, 1155, 1156, 1157, 1158, 1159, 1160, 1169, 1170,
    2049, 2051, 2052, 2057, 2068, 2072, 2073, 2077, 2080, 2092, 2094, 2098, 2107, 2108,
    2110, 2115, 2117, 2118, 2121, 2128, 2137, 2141, 2143, 2145, 2151, 2155, 2163, 3073,
    3076, 3081, 3131, 3152, 3179, 4097, 4100, 4103, 4105, 4106, 4122, 4155, 4191, 5121,
    5124, 5127, 5129, 5130, 5132, 5146, 5179, 6145, 6153, 6154, 6156, 6170, 6203, 7169,
    7177, 7178, 7194, 7227, 8193, 8201, 8202, 8204, 8218, 8251, 9217, 9225, 9226, 9228,
    9275, 10241, 10249, 10250, 10252, 10266, 11265, 11273, 11274, 11276, 11290, 12289, 12297, 12298,
    12300, 12314, 13313, 13321, 13322, 13324, 14337, 14345, 14346, 14348, 15361, 15369, 15370, 15372,
    16385, 16393, 16394, 17417, 17418, 18441, 18442, 19466, 20490, 21514, 22538, 23562, 31770,
    // not in T4:P11all's list, but read and formatted with the invariant letters (T4:P11, P12)
    2074,
];

/// The row of session language `lang`; `None` for an id that is no Windows language BC knows
/// (T4:P11all lists 247).
pub fn row(lang: i32) -> Option<&'static Row> {
    if lang == ENGLISH || INVARIANT_LANGS.contains(&lang) {
        return Some(&INVARIANT);
    }
    ROWS.iter().find(|r| r.lang == lang)
}

/// What a run of letters means in a row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Spelling {
    Unit(Unit),
    Current(Unit),
    /// starts with the current prefix, but the rest is no unit: E2 (T4:P04e `C`, `CC`, `CX`, `CMM`)
    CurrentNoUnit,
    /// no spelling: E1 (R26)
    Unknown,
    /// no evidenced spelling in an incomplete row: it may be an unevidenced one
    Open,
}

/// Whether the letters `run` (any case) spell `word` (upper case).
fn spells(run: &str, word: &str) -> bool {
    let mut a = run.chars().map(upper);
    let mut b = word.chars();
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x == y => {}
            _ => return false,
        }
    }
}

/// The rest of `run` after `prefix` (any case), if `run` starts with it.
fn strip<'a>(run: &'a str, prefix: &str) -> Option<&'a str> {
    let mut it = run.char_indices();
    for p in prefix.chars() {
        match it.next() {
            Some((_, c)) if upper(c) == p => {}
            _ => return None,
        }
    }
    Some(it.next().map_or("", |(i, _)| &run[i..]))
}

/// The current prefix is matched first: a run that starts with it is a current term or nothing
/// (T4:P11back, Greek: C is `Τ`, Q is `ΤΡ`; `ΤΡ2` raises E2 and `1ΤΡ` E1).
fn classify(run: &str, row: &Row) -> Spelling {
    if let Some(rest) = row.c.and_then(|c| strip(run, c)) {
        for u in Unit::ALL {
            if row.unit(u).is_some_and(|w| spells(rest, w)) {
                return Spelling::Current(u);
            }
        }
        return if row.complete() { Spelling::CurrentNoUnit } else { Spelling::Open };
    }
    for u in Unit::ALL {
        if row.unit(u).is_some_and(|w| spells(run, w)) {
            return Spelling::Unit(u);
        }
    }
    if row.complete() {
        Spelling::Unknown
    } else {
        Spelling::Open
    }
}

// ------------------------------------------------------------------------------------ reader

/// Character classes of the reader (T4:P09, 621 code points in 15 places each).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Digit,
    Plus,
    Minus,
    /// dropped, and ends the number or letter run in progress (R11, R13, R14, R15; T4:P09):
    /// tab, LF, CR, space and the ASCII punctuation `! " # $ % & ' ( ) * , . / : ; < = > ? @`
    Sep,
    /// a token of its own that is no term (T4:P09: U+0008, U+000B, U+000C, U+000E..U+001F)
    Junk,
    /// every other character joins a run of letters (T4:P09: ASCII letters, `[ \ ] ^ _ \` { | } ~`,
    /// U+007F..U+024F, Greek, Cyrillic, the Unicode spaces and dashes probed, fullwidth forms)
    Letter,
    /// not settled (T4:P09: inputs with U+0001..U+0007 or with decimal digits of other scripts; U+0000
    /// was not probed)
    Open,
}

fn class(c: char) -> Class {
    match c {
        '0'..='9' => Class::Digit,
        '+' => Class::Plus,
        '-' => Class::Minus,
        '\t' | '\n' | '\r' | ' '..='*' | ',' | '.' | '/' | ':'..='@' => Class::Sep,
        '\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' => Class::Junk,
        '\u{0}'..='\u{7}' => Class::Open,
        c if !c.is_ascii() && c.is_numeric() => Class::Open,
        _ => Class::Letter,
    }
}

/// Count numbers above this raise E4 0 - 9999; position numbers above it raise E2 (T4:P01, P02).
pub const NUM_MAX: u32 = 9999;
/// Numbers are read saturating here: no input overflows (Q8).
const NUM_SATURATE: u32 = 100_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok {
    End,
    Sign(Sign),
    /// a number: its value (saturated) and digit count (saturated)
    Num { v: u32, digits: u8 },
    /// a run of letters, as a byte range of the input
    Letters { a: usize, b: usize },
    Junk,
}

struct Lexer<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Lexer<'a> {
    fn peek_char(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn next(&mut self) -> Result<Tok, DfError> {
        while let Some(c) = self.peek_char().filter(|c| class(*c) == Class::Sep) {
            self.i += c.len_utf8();
        }
        let Some(c) = self.peek_char() else { return Ok(Tok::End) };
        match class(c) {
            Class::Plus => {
                self.i += 1;
                Ok(Tok::Sign(Sign::Plus))
            }
            Class::Minus => {
                self.i += 1;
                Ok(Tok::Sign(Sign::Minus))
            }
            Class::Digit => {
                let (mut v, mut digits) = (0u32, 0u8);
                while let Some(d) = self.peek_char().and_then(|c| c.to_digit(10).filter(|_| c.is_ascii_digit())) {
                    v = (v * 10 + d).min(NUM_SATURATE);
                    digits = digits.saturating_add(1);
                    self.i += 1;
                }
                Ok(Tok::Num { v, digits })
            }
            Class::Letter => {
                let a = self.i;
                while let Some(c) = self.peek_char().filter(|c| class(*c) == Class::Letter) {
                    self.i += c.len_utf8();
                }
                Ok(Tok::Letters { a, b: self.i })
            }
            Class::Junk => {
                self.i += c.len_utf8();
                Ok(Tok::Junk)
            }
            Class::Open | Class::Sep => Err(DfError::Unsettled(Probe::P09)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    AfterSign,
    AfterTerm,
}

/// The accepted numbers of a position term (R24; T4:P02).
fn position_range(u: Unit) -> (u16, u16) {
    match u {
        Unit::D => (1, 31),
        Unit::WD => (1, 7),
        Unit::W => (1, 53),
        Unit::M => (1, 12),
        Unit::Q => (1, 4),
        Unit::Y => (0, 99),
    }
}

/// Read `input` with the letters of `row`, left to right; the first problem met is reported
/// (R30). `open`: the probe for a letter run that an incomplete row cannot place.
fn read_terms(input: &str, row: &Row, open: Probe) -> Result<Formula, DfError> {
    let mut lx = Lexer { s: input, i: 0 };
    let mut f = Formula::EMPTY;
    let mut sign = Sign::None;
    let mut state = State::Start;
    loop {
        let tok = lx.next()?;
        if state == State::AfterTerm {
            match tok {
                Tok::End => break,
                Tok::Sign(s) => {
                    sign = s;
                    state = State::AfterSign;
                }
                // R28, R30: a later term without a sign
                Tok::Num { .. } | Tok::Letters { .. } | Tok::Junk => return Err(read_err(input, ReadKind::NoSign)),
            }
            continue;
        }
        let term = match tok {
            // R31: nothing at all is the empty formula
            Tok::End if state == State::Start => break,
            // R29: a sign must be followed by a term
            Tok::End => return Err(read_err(input, ReadKind::NoNumber)),
            Tok::Sign(s) if state == State::Start => {
                sign = s;
                state = State::AfterSign;
                continue;
            }
            Tok::Sign(_) => return Err(read_err(input, ReadKind::NoNumber)),
            Tok::Num { v, digits } => {
                // R23, T4:P01: range-checked when read
                if v > NUM_MAX {
                    return Err(read_err(input, ReadKind::TooLarge { min: 0, max: NUM_MAX as u16 }));
                }
                match lx.next()? {
                    Tok::Letters { a, b } => match classify(&input[a..b], row) {
                        Spelling::Unit(u) => Term { sign, unit: u, kind: Kind::Count(Num { value: v as u16, digits }) },
                        Spelling::Open => return Err(DfError::Unsettled(open)),
                        // R17, R21: the letters after a number are no count unit
                        _ => return Err(read_err(input, ReadKind::NoUnit)),
                    },
                    // R22: a number with no letters after it
                    _ => return Err(read_err(input, ReadKind::NoUnit)),
                }
            }
            Tok::Letters { a, b } => match classify(&input[a..b], row) {
                // R27; T4:P04: CWD, -CD and +CD are current terms too
                Spelling::Current(u) => Term { sign, unit: u, kind: Kind::Current },
                Spelling::Unit(u) => match lx.next()? {
                    Tok::Num { v, digits } => {
                        // T4:P02: a position number above 9999 reads as no number
                        if v > NUM_MAX {
                            return Err(read_err(input, ReadKind::NoNumber));
                        }
                        let (min, max) = position_range(u);
                        if v < u32::from(min) || v > u32::from(max) {
                            return Err(read_err(input, ReadKind::TooLarge { min, max }));
                        }
                        Term { sign, unit: u, kind: Kind::Position(Num { value: v as u16, digits }) }
                    }
                    // R25: a unit with no number
                    _ => return Err(read_err(input, ReadKind::NoNumber)),
                },
                // R14, T4:P04e: the current prefix without a unit
                Spelling::CurrentNoUnit => return Err(read_err(input, ReadKind::NoNumber)),
                // R17, R26: letters that are no unit
                Spelling::Unknown => return Err(read_err(input, ReadKind::NoUnit)),
                Spelling::Open => return Err(DfError::Unsettled(open)),
            },
            // T4:P09: a junk character where a term begins
            Tok::Junk => return Err(read_err(input, ReadKind::NoUnit)),
        };
        f.push(term);
        sign = Sign::None;
        state = State::AfterTerm;
    }
    Ok(f)
}

/// Read `input` (T4:P10: at most 32 characters; R30 for the rest).
fn read(input: &str, row: &Row, open: Probe) -> Result<Formula, DfError> {
    // a character of an unsettled class makes the whole text open, wherever it stands (T4:P09)
    if input.chars().any(|c| class(c) == Class::Open) {
        return Err(DfError::Unsettled(Probe::P09));
    }
    if input.encode_utf16().count() > MAX_INPUT_LEN {
        // T4:P10 measured texts that are formulas apart from their length; which error a longer
        // text that also fails to read raises is open
        return Err(match read_terms(input, row, open) {
            Ok(_) if input.chars().all(|c| upper(c) == c) => DfError::Read { kind: ReadKind::TooLong, quoted: Ok(input.into()) },
            Ok(_) => DfError::Read { kind: ReadKind::TooLong, quoted: Err(Probe::P10) },
            Err(e) if e.is_refusal() => DfError::Refused(Probe::P10),
            Err(e) => e,
        });
    }
    read_terms(input, row, open)
}

/// Whether a text is bracketed for the choice of letters (R35, T4:P12): it starts with `<` and
/// ends with `>`, outer spaces included (`'  <1d 1w>  '` reads with the session's letters).
fn bracketed(text: &str) -> bool {
    text.len() >= 2 && text.starts_with('<') && text.ends_with('>')
}

/// Read a stored value, a field value from code, an InitValue or a filter value: invariant
/// letters, whatever the session language (R35, R80, Q4, Q13).
pub fn parse_invariant(text: &str) -> Result<Formula, DfError> {
    read(text, &INVARIANT, Probe::P12)
}

/// Which input a read serves: the probe for letters a localized row cannot place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    Evaluate,
    CalcDate,
}

/// Read user text in session language `lang` (R35, R36, T4:P12, P17): bracketed text with the
/// invariant letters, other text (one bracket included) with the letters of `lang` only.
pub fn parse(text: &str, lang: i32, purpose: Purpose) -> Result<Formula, DfError> {
    let open = match purpose {
        Purpose::Evaluate => Probe::P12,
        Purpose::CalcDate => Probe::P17,
    };
    if bracketed(text) {
        return read(text, &INVARIANT, open);
    }
    match row(lang) {
        Some(r) => read(text, r, open),
        None => Err(DfError::Unsettled(open)),
    }
}

/// Evaluate(DateFormula, Text [, Number]) in session language `lang` (R40..R44): the stored
/// text. Number 9 reads the invariant letters; any other number reads as without one (T4:P15eval).
pub fn evaluate(text: &str, lang: i32, number: Option<i64>) -> Result<String, DfError> {
    let f = if number == Some(9) { parse_invariant(text)? } else { parse(text, lang, Purpose::Evaluate)? };
    Ok(f.stored())
}

// ------------------------------------------------------------------------------------ format

/// Render `f` with the letters of `row` into `out`; `Err(P11)` when the row lacks a letter.
fn render(f: &Formula, row: &Row, out: &mut String) -> Result<(), DfError> {
    use std::fmt::Write;
    fn num(out: &mut String, n: Num) {
        for _ in digits(n.value)..usize::from(n.digits) {
            out.push('0');
        }
        let _ = write!(out, "{}", n.value);
    }
    for t in f.terms() {
        match t.sign {
            Sign::None => {}
            Sign::Plus => out.push('+'),
            Sign::Minus => out.push('-'),
        }
        let u = row.unit(t.unit).ok_or(DfError::Unsettled(Probe::P11))?;
        match t.kind {
            Kind::Count(n) => {
                num(out, n);
                out.push_str(u);
            }
            Kind::Position(n) => {
                out.push_str(u);
                num(out, n);
            }
            Kind::Current => {
                out.push_str(row.c.ok_or(DfError::Unsettled(Probe::P11))?);
                out.push_str(u);
            }
        }
    }
    Ok(())
}

fn digits(n: u16) -> usize {
    match n {
        0..=9 => 1,
        10..=99 => 2,
        100..=999 => 3,
        _ => 4,
    }
}

/// `Format(DF, 0, std)` of a stored value in session language `lang` (R50..R55, T4:P11, P15fmt).
pub fn format(stored: &str, std: i64, lang: i32) -> Result<String, DfError> {
    match std {
        // R50, R51, R55: the session language's letters, no brackets
        0 | 1 => {
            if stored.is_empty() {
                return Ok(String::new());
            }
            if lang == ENGLISH {
                return Ok(stored.to_string());
            }
            parse_invariant(stored)?.localized(lang)
        }
        // R52: brackets around the invariant text, also for the empty formula
        2 => Ok(format!("<{stored}>")),
        // R53, R55: the invariant text
        9 => Ok(stored.to_string()),
        // R54, T4:P15fmt: every other number, negative ones included
        _ => Err(DfError::InvalidStandardFormat(std)),
    }
}

/// The longest padding Format(DF, Length) builds.
const MAX_PAD: i64 = 1_000_000;

/// `Format(DF, Length [, std])` (T4:P15len, measured with format 0): a positive length pads on
/// the left to that length or cuts to it; a negative one cuts to its size. A length with another
/// format number is open (P15).
pub fn format_with(stored: &str, length: i64, std: i64, lang: i32) -> Result<String, DfError> {
    let s = format(stored, std, lang)?;
    if length == 0 {
        return Ok(s);
    }
    if std != 0 {
        return Err(DfError::Unsettled(Probe::P15));
    }
    let n = s.chars().count();
    let want = usize::try_from(length.unsigned_abs()).unwrap_or(usize::MAX);
    // T4:P15len measured lengths up to 1000; far longer padding is not measured (and must not
    // exhaust memory, Q8)
    if length > MAX_PAD {
        return Err(DfError::Unsettled(Probe::P15));
    }
    if n >= want || length < 0 {
        return Ok(s.chars().take(want).collect());
    }
    let mut out = " ".repeat(want - n);
    out.push_str(&s);
    Ok(out)
}

/// DateFormula text outside Format (StrSubstNo, Error, a Variant's Format) in session language
/// `lang` (R85, Q10): format 0 (T4:P16).
pub fn text(stored: &str, lang: i32) -> Result<String, DfError> {
    format(stored, 0, lang)
}

// ------------------------------------------------------------------------------------ comparing

/// `=` between two DateFormula values (R5, Q6): equality of the stored text (T4:P14eq: `+1D`,
/// `01D` and `1W+1D` all differ from `1D` and `1D+1W`). Never reads them again (Q15).
pub fn equal(a: &str, b: &str) -> Result<bool, DfError> {
    Ok(a == b)
}

/// The order of two DateFormula values (R82): only equal ones are settled; BC's order differs
/// between SQL and temporary tables (T4:P14order) by a form this project does not store.
pub fn order(a: &str, b: &str) -> Result<Ordering, DfError> {
    if a == b {
        Ok(Ordering::Equal)
    } else {
        Err(DfError::Unsettled(Probe::P14))
    }
}

// ------------------------------------------------------------------------------------ dates

/// Day numbers: days from the common era, 0001-01-01 = 1 (the runtime's Date value is this
/// times 2, plus 1 for a closing date).
pub const MIN_DAY: i64 = 1;
/// The first day a CalcDate step may reach: 0001-01-01 is refused (T4:P06 `<-1D>` on 0001-01-02).
pub const MIN_RESULT: i64 = 2;
/// 9999-12-31
pub const MAX_DAY: i64 = 3_652_059;

/// The day number of a date of 0001-01-01..9999-12-31; `None` for any other year, month or day.
pub fn date_to_day(y: i32, m: u32, d: u32) -> Option<i64> {
    let (y, m, d) = (i64::from(y), i64::from(m), i64::from(d));
    ((1..=9999).contains(&y) && (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)).then(|| day_number(y, m, d))
}

/// (year, month, day) of a day number of 0001-01-01..9999-12-31; `None` outside.
pub fn day_to_date(n: i64) -> Option<(i32, u32, u32)> {
    (MIN_DAY..=MAX_DAY).contains(&n).then(|| {
        let (y, m, d) = civil(n);
        (y as i32, m as u32, d as u32)
    })
}

/// Day number of a proleptic Gregorian date (H. Hinnant's days_from_civil, shifted). Unchecked
/// i64 arithmetic: callers keep the year within a few times 10,000 (Formula::apply checks every step).
pub(crate) fn day_number(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 + 719_163
}

/// (year, month, day) of a day number (unchecked, as day_number).
pub(crate) fn civil(n: i64) -> (i64, i64, i64) {
    let z = n - 719_163 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn leap(y: i64) -> bool {
    y.rem_euclid(4) == 0 && (y.rem_euclid(100) != 0 || y.rem_euclid(400) == 0)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Monday = 1 .. Sunday = 7 (Learn CalcDate: "Monday is weekday 1"); 0001-01-01 was a Monday.
pub(crate) fn weekday(n: i64) -> i64 {
    (n - 1).rem_euclid(7) + 1
}

/// (ISO week-year, ISO week) of a day.
fn iso_week(n: i64) -> (i64, i64) {
    let thu = n - weekday(n) + 4;
    let (y, _, _) = civil(thu);
    (y, (thu - day_number(y, 1, 1)) / 7 + 1)
}

fn iso_week1_monday(y: i64) -> i64 {
    let j4 = day_number(y, 1, 4);
    j4 - (weekday(j4) - 1)
}

fn iso_weeks(y: i64) -> i64 {
    iso_week(day_number(y, 12, 28)).1
}

/// Add `n` months, the day clamped to the target month (R73..R75).
fn add_months(d: i64, n: i64) -> i64 {
    let (y, m, dd) = civil(d);
    let t = y * 12 + (m - 1) + n;
    let (ny, nm) = (t.div_euclid(12), t.rem_euclid(12) + 1);
    day_number(ny, nm, dd.min(days_in_month(ny, nm)))
}

impl Term {
    /// This term applied to day number `d` (R70..R78; T4:P03, P04, P05). Arithmetic on i64;
    /// `Err(())` for a `Wn` step into ISO week-year 10000.
    pub(crate) fn apply(&self, d: i64) -> Result<i64, ()> {
        let back = self.sign == Sign::Minus;
        Ok(match self.kind {
            Kind::Count(n) => {
                let n = if back { -i64::from(n.value) } else { i64::from(n.value) };
                match self.unit {
                    // R76, T4:P05: WD counts calendar days, every n
                    Unit::D | Unit::WD => d + n,
                    Unit::W => d + 7 * n,
                    Unit::M => add_months(d, n),
                    Unit::Q => add_months(d, 3 * n),
                    Unit::Y => add_months(d, 12 * n),
                }
            }
            // R77: to the last day of the current period, with `-` to the first; CD and CWD keep
            // the day whatever the sign (T4:P04)
            Kind::Current => match self.unit {
                Unit::D | Unit::WD => d,
                Unit::W => {
                    let wd = weekday(d);
                    if back {
                        d - (wd - 1)
                    } else {
                        d + (7 - wd)
                    }
                }
                Unit::M => {
                    let (y, m, _) = civil(d);
                    if back {
                        day_number(y, m, 1)
                    } else {
                        day_number(y, m, days_in_month(y, m))
                    }
                }
                Unit::Q => {
                    let (y, m, _) = civil(d);
                    let first = (m - 1) / 3 * 3 + 1;
                    if back {
                        day_number(y, first, 1)
                    } else {
                        day_number(y, first + 2, days_in_month(y, first + 2))
                    }
                }
                Unit::Y => {
                    let (y, _, _) = civil(d);
                    if back {
                        day_number(y, 1, 1)
                    } else {
                        day_number(y, 12, 31)
                    }
                }
            },
            // R78, T4:P03: the start of period number n, forward (backward with `-`)
            Kind::Position(n) => {
                let n = i64::from(n.value);
                // forward: the base's own period if its index is below n; backward: if above
                let own = |k: i64| if back { k > n } else { k < n };
                let step = if back { -1 } else { 1 };
                match self.unit {
                    Unit::D => {
                        let (y, m, k) = civil(d);
                        let t = y * 12 + (m - 1) + if own(k) { 0 } else { step };
                        let (yy, mm) = (t.div_euclid(12), t.rem_euclid(12) + 1);
                        day_number(yy, mm, n.min(days_in_month(yy, mm)))
                    }
                    Unit::WD => {
                        let k = weekday(d);
                        let monday = d - (k - 1);
                        monday + if own(k) { 0 } else { 7 * step } + (n - 1)
                    }
                    // weeks of the ISO week-year, the number cut to the weeks of that year
                    Unit::W => {
                        let (iy, k) = iso_week(d);
                        let yy = if own(k) { iy } else { iy + step };
                        if yy > 9999 {
                            return Err(());
                        }
                        iso_week1_monday(yy) + 7 * (n.min(iso_weeks(yy)) - 1)
                    }
                    Unit::M => {
                        let (y, k, _) = civil(d);
                        day_number(if own(k) { y } else { y + step }, n, 1)
                    }
                    // forward by the quarter's index; backward by the month: the base's own year if
                    // its month is past the quarter's first month (`-Q1` in February stays)
                    Unit::Q => {
                        let (y, m, _) = civil(d);
                        let first = 3 * (n - 1) + 1;
                        let this_year = if back { m > first } else { (m - 1) / 3 + 1 < n };
                        day_number(if this_year { y } else { y + step }, first, 1)
                    }
                    // the year of the century; the sign changes nothing (`-Y2` as `Y2`)
                    Unit::Y => {
                        let (y, _, _) = civil(d);
                        let k = y.rem_euclid(100);
                        let century = y - k;
                        day_number(if k < n { century + n } else { century + 100 + n }, 1, 1)
                    }
                }
            }
        })
    }
}

impl Formula {
    /// CalcDate on a day number (R62, R64, R70; T4:P06): every step must stay inside
    /// 0001-01-02..9999-12-31, else E6. No allocation.
    pub fn apply(&self, day: i64) -> Result<i64, DfError> {
        if !(MIN_DAY..=MAX_DAY).contains(&day) {
            return Err(DfError::DateInvalid);
        }
        let mut d = day;
        for t in self.terms() {
            d = t.apply(d).map_err(|()| DfError::WeekBeyondRange { formula: self.stored().into() })?;
            if !(MIN_RESULT..=MAX_DAY).contains(&d) {
                return Err(DfError::DateInvalid);
            }
        }
        Ok(d)
    }

    /// CalcDate on the runtime's Date value (`days * 2`, plus 1 for a closing date) in session
    /// language `lang` (the language of E5's Formula part).
    pub fn calc(&self, date: i32, lang: i32) -> Result<i32, DfError> {
        // R63, T4:P07: 0D raises E5 for every formula; its Formula part is format 0
        if date == 0 {
            return Err(DfError::UndefinedDate { formula: self.localized(lang)?.into() });
        }
        let r = self.apply(i64::from(date >> 1))?;
        // R65, T4:P08: computed from the normal date; a closing base gives a closing date
        Ok((r * 2) as i32 + (date & 1))
    }
}

/// CalcDate(Text, Date) in session language `lang` (R60, Q4). A reading error wins over E5
/// (T4:P07).
pub fn calc_text(text: &str, lang: i32, date: i32) -> Result<i32, DfError> {
    parse(text, lang, Purpose::CalcDate)?.calc(date, lang)
}

// ------------------------------------------------------------------------------------ cache

/// FNV-1a: short keys, no hash flooding concern in a bounded per-session cache.
#[derive(Default)]
pub struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf2_9ce4_8422_2325 } else { self.0 };
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = h;
    }
}

type FastMap<V> = HashMap<Box<str>, V, BuildHasherDefault<Fnv>>;

/// Entries per map before the cache starts over.
pub const CACHE_CAP: usize = 1024;
/// The key language of a text read with the invariant letters (bracketed).
const ANY_LANG: i32 = i32::MIN;

/// The session's parse cache (Q7): stored text, and formula text per language, to the parsed
/// formula. Only successful reads are kept. Owned by a session, never shared.
#[derive(Default)]
pub struct Cache {
    stored: FastMap<Formula>,
    text: FastMap<(i32, Formula)>,
}

impl Cache {
    pub fn new() -> Cache {
        Cache::default()
    }

    /// The parsed form of a stored value (invariant letters).
    pub fn stored(&mut self, stored: &str) -> Result<Formula, DfError> {
        if let Some(f) = self.stored.get(stored) {
            return Ok(*f);
        }
        let f = parse_invariant(stored)?;
        if self.stored.len() >= CACHE_CAP {
            self.stored.clear();
        }
        self.stored.insert(stored.into(), f);
        Ok(f)
    }

    /// The parsed form of user text read in `lang` (CalcDate with a Text formula).
    pub fn text(&mut self, text: &str, lang: i32) -> Result<Formula, DfError> {
        if let Some((l, f)) = self.text.get(text) {
            if *l == ANY_LANG || *l == lang {
                return Ok(*f);
            }
        }
        let f = parse(text, lang, Purpose::CalcDate)?;
        let key_lang = if bracketed(text) { ANY_LANG } else { lang };
        if self.text.len() >= CACHE_CAP {
            self.text.clear();
        }
        self.text.insert(text.into(), (key_lang, f));
        Ok(f)
    }

    /// CalcDate(DateFormula, Date): the stored value, invariant letters (R66, Q4).
    pub fn calc_stored(&mut self, stored: &str, lang: i32, date: i32) -> Result<i32, DfError> {
        self.stored(stored)?.calc(date, lang)
    }

    /// CalcDate(Text, Date) in session language `lang` (Q4).
    pub fn calc_text(&mut self, text: &str, lang: i32, date: i32) -> Result<i32, DfError> {
        self.text(text, lang)?.calc(date, lang)
    }

    pub fn len(&self) -> usize {
        self.stored.len() + self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_numbers_match_chrono() {
        use chrono::Datelike;
        for (y, m, d) in [(1, 1, 1), (1970, 1, 1), (2000, 2, 29), (2024, 1, 15), (9999, 12, 31), (1600, 3, 1)] {
            let n = chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
            assert_eq!(day_number(y as i64, m as i64, d as i64), i64::from(n.num_days_from_ce()));
            assert_eq!(civil(i64::from(n.num_days_from_ce())), (y as i64, m as i64, d as i64));
            assert_eq!(weekday(i64::from(n.num_days_from_ce())), i64::from(n.weekday().number_from_monday()));
            let w = n.iso_week();
            assert_eq!(iso_week(i64::from(n.num_days_from_ce())), (i64::from(w.year()), i64::from(w.week())));
        }
        assert_eq!(day_number(9999, 12, 31), MAX_DAY);
        assert_eq!(day_number(1, 1, 1), MIN_DAY);
    }

    /// Every name [note_assumed] can record is a row of `assume::REGISTRY`, and every DateFormula
    /// row there is a probe.
    #[test]
    fn every_probe_is_registered() {
        for p in Probe::ALL {
            assert!(crate::assume::lookup(p.assume_name()).is_some(), "{} is not in assume::REGISTRY", p.assume_name());
        }
        let rows: Vec<&str> = crate::assume::REGISTRY.iter().map(|a| a.name).filter(|n| n.starts_with("DateFormula.")).collect();
        assert_eq!(rows.len(), Probe::ALL.len(), "{rows:?}");
        assert!(rows.iter().all(|n| Probe::ALL.iter().any(|p| p.assume_name() == *n)), "{rows:?}");
    }

    #[test]
    fn rows_are_unique_and_known() {
        for r in ROWS {
            assert!(!INVARIANT_LANGS.contains(&r.lang), "{}", r.lang);
            assert_eq!(ROWS.iter().filter(|x| x.lang == r.lang).count(), 1, "{}", r.lang);
        }
        assert_eq!(ROWS.len() + INVARIANT_LANGS.len(), 248);
    }

    #[test]
    fn eleven_terms_fill_32_characters() {
        let t = "1D+1D+1D+1D+1D+1D+1D+1D+1D+1D+1D";
        assert_eq!(t.len(), 32);
        assert_eq!(parse_invariant(t).unwrap().terms().len(), CAP);
    }
}
