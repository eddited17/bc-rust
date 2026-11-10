//! Replays tests/vectors/u1-dateformula.tsv: a compact, deduplicated selection of the U1
//! probe runs on a real BC 29.1 (tools/probes, probe u1-dateformula; columns and ops are described
//! in the file's header and in `run` below). Every row must match, except rows of a point that is
//! still open, which must come back as `Unsettled` with the expected probe: [OPEN] lists them.
//! Rows carry an error's kind, range and variable part (`%1`, or the formula); the message
//! templates themselves are asserted once each in tests/u1_date_formula.rs (`message_templates`).

use al_runtime::datefml::{self, date_to_day, day_to_date, DfError, Probe, ENGLISH};

/// Rows the module answers with `Unsettled` on purpose, by the start of their `src` and input.
const OPEN: &[(&str, &str, Probe)] = &[
    // T4:P09: inputs with U+0001..U+0007 are not settled (the file holds none of their rows)
    ("P09 U+0001", "", Probe::P09),
    ("P09 U+0002", "", Probe::P09),
    ("P09 U+0003", "", Probe::P09),
    ("P09 U+0004", "", Probe::P09),
    ("P09 U+0005", "", Probe::P09),
    ("P09 U+0006", "", Probe::P09),
    ("P09 U+0007", "", Probe::P09),
    // T4:P09: inputs with decimal digits of other scripts are not settled
    ("P09 U+0660", "", Probe::P09),
    ("P09 U+FF10", "", Probe::P09),
    // the C and D letters of Vietnamese (only their sum `HTNG` was measured)
    ("P11all VIT", "", Probe::P11),
];

fn unesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('x') => {
                let h: String = it.by_ref().take(2).collect();
                out.push(char::from(u8::from_str_radix(&h, 16).unwrap()));
            }
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A date of the file: `yyyy-mm-dd`, `C` before a closing date, `0D`; the runtime's encoding.
fn date(s: &str) -> i32 {
    if s == "0D" {
        return 0;
    }
    let (closing, iso) = match s.strip_prefix('C') {
        Some(rest) => (1, rest),
        None => (0, s),
    };
    let p: Vec<u32> = iso.split('-').map(|x| x.parse().unwrap()).collect();
    (date_to_day(p[0] as i32, p[1], p[2]).expect(s) * 2) as i32 + closing
}

fn show(enc: i32) -> String {
    let (y, m, d) = day_to_date(i64::from(enc >> 1)).expect("a date in range");
    format!("{}{y:04}-{m:02}-{d:02}", if enc & 1 == 1 { "C" } else { "" })
}

fn kind(e: &DfError) -> String {
    use datefml::ReadKind::*;
    match e {
        DfError::Read { kind: NoUnit, .. } => "E1".into(),
        DfError::Read { kind: NoNumber, .. } => "E2".into(),
        DfError::Read { kind: NoSign, .. } => "E3".into(),
        DfError::Read { kind: TooLarge { min, max }, .. } => format!("E4[{min}-{max}]"),
        DfError::Read { kind: TooLong, .. } => "E7".into(),
        DfError::UndefinedDate { .. } => "E5".into(),
        DfError::DateInvalid => "E6".into(),
        DfError::WeekBeyondRange { .. } => "E8".into(),
        DfError::InvalidStandardFormat(_) => "IS".into(),
        DfError::Refused(p) | DfError::Unsettled(p) => format!("open {p}"),
    }
}

/// The variable part of an error's message: `%1`, or the formula of E5 and E8.
fn part(e: &DfError) -> Option<String> {
    match e {
        DfError::Read { quoted: Ok(q), .. } => Some(q.to_string()),
        DfError::Read { quoted: Err(_), .. } => None,
        DfError::UndefinedDate { formula } | DfError::WeekBeyondRange { formula } => Some(formula.to_string()),
        _ => Some(String::new()),
    }
}

/// Our answer in the file's form, or the error.
fn run(op: &str, lang: i32, input: &str, arg: &str) -> Result<String, DfError> {
    let base = date("2024-01-15");
    Ok(match op {
        "calc" => show(datefml::calc_text(input, lang, date(arg))?),
        "calcdf" => {
            let (eval_lang, at) = match arg.strip_prefix("E1033:") {
                Some(at) => (ENGLISH, at),
                None => (lang, arg),
            };
            let stored = datefml::evaluate(input, eval_lang, None)?;
            show(datefml::parse_invariant(&stored)?.calc(date(at), lang)?)
        }
        "eval" => {
            let stored = datefml::evaluate(input, lang, None)?;
            let f = datefml::parse_invariant(&stored)?;
            let calc = match f.calc(base, lang) {
                Ok(d) => show(d),
                Err(DfError::DateInvalid) => "E6".into(),
                Err(e) => return Err(e),
            };
            format!("{stored}|{}|{calc}", datefml::format(&stored, 0, lang)?)
        }
        "evaln" => datefml::evaluate(input, lang, Some(arg.parse().unwrap()))?,
        "fmt" => datefml::format(&datefml::evaluate(input, ENGLISH, None)?, arg.parse().unwrap(), lang)?,
        "len" => datefml::format_with(&datefml::evaluate(input, ENGLISH, None)?, arg.parse().unwrap(), 0, lang)?,
        "eq" => {
            let (a, b) = input.split_once('=').unwrap();
            let same = datefml::equal(&datefml::evaluate(a, ENGLISH, None)?, &datefml::evaluate(b, ENGLISH, None)?)?;
            (if same { "Yes" } else { "No" }).into()
        }
        "text" => datefml::text(&datefml::evaluate(input, ENGLISH, None)?, lang)?,
        _ => panic!("unknown op {op}"),
    })
}

/// Whether `got` matches the expected `ok:` value (`*` parts are not recorded).
fn ok_matches(want: &str, got: &str) -> bool {
    let w: Vec<&str> = want.split('|').collect();
    let g: Vec<&str> = got.split('|').collect();
    if w.len() == 1 {
        return w[0] == "*" || w[0] == got;
    }
    w.len() == g.len() && w.iter().zip(&g).all(|(a, b)| *a == "*" || a == b)
}

#[test]
fn probe_vectors() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/u1-dateformula.tsv");
    let text = std::fs::read_to_string(&path).expect("vector file");
    let (mut n, mut open_rows) = (0usize, 0usize);
    let mut bad = vec![];
    for line in text.lines().filter(|l| !l.starts_with('#')).skip(1) {
        let f: Vec<String> = line.split('\t').map(unesc).collect();
        let [src, op, lang, input, arg, expect] = &f[..] else { panic!("bad row {line}") };
        let lang: i32 = lang.parse().unwrap();
        n += 1;
        let got = run(op, lang, input, arg);
        // the rows of a point that is still open
        if let Some((_, _, p)) = OPEN.iter().find(|(s, i, _)| src.starts_with(s) && input.contains(i)) {
            open_rows += 1;
            if !matches!(&got, Err(DfError::Unsettled(q) | DfError::Refused(q)) if q == p) {
                bad.push(format!("{src} {op} {lang} {input:?} {arg}: want Unsettled({p}), got {got:?}"));
            }
            continue;
        }
        let good = match (expect.split_once(':'), &got) {
            (Some(("ok", want)), Ok(v)) => ok_matches(want, v),
            (Some(("err", rest)), Err(e)) => {
                let mut p = rest.splitn(3, ':');
                let (code, want_kind, want_part) = (p.next().unwrap(), p.next().unwrap(), p.next());
                let code_ok = e.code() == code || (code == "Evaluate" && e.is_refusal());
                let kind_ok = want_kind == "*" || want_kind == "?" || kind(e) == want_kind;
                // the part is recorded for English sessions only
                let part_ok = want_part.is_none_or(|w| part(e).as_deref() == Some(w));
                code_ok && kind_ok && part_ok
            }
            _ => false,
        };
        if !good {
            let shown = match &got {
                Ok(v) => format!("ok:{v}"),
                Err(e) => format!("err:{}:{}:{}", e.code(), kind(e), part(e).unwrap_or_else(|| "(open)".into())),
            };
            bad.push(format!("{src} {op} {lang} {input:?} {arg}: want {expect}, got {shown}"));
        }
    }
    assert!(n > 1000, "too few rows: {n}");
    assert!(bad.is_empty(), "{} of {n} rows differ ({open_rows} open rows):\n{}", bad.len(), bad.iter().take(80).cloned().collect::<Vec<_>>().join("\n"));
}
