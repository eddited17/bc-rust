//! DateFormula (crate::datefml): one table-driven test per part of section 3 of the U1 spec, with
//! the evidence of each row beside it, the `Unsettled` points of section 4, and the requirement
//! tests (Q4, Q5, Q6, Q8, Q9, Q13). Evidence ids: `T1:` BCApps, `T2:` Learn, `T3:` AL
//! Language.Tests (file::test), as in the spec. Base date 2024-01-15 (a Monday) unless a row says
//! otherwise.

use al_runtime::datefml::{self, date_to_day, day_to_date, DfError, Formula, Probe, Purpose, ReadKind, ENGLISH};

const B: &str = "2024-01-15";

fn day(iso: &str) -> i64 {
    let p: Vec<i64> = iso.split('-').map(|x| x.parse().unwrap()).collect();
    date_to_day(p[0] as i32, p[1] as u32, p[2] as u32).expect(iso)
}

fn iso(n: i64) -> String {
    let (y, m, d) = day_to_date(n).expect("a date in range");
    format!("{y:04}-{m:02}-{d:02}")
}

/// CalcDate(Text) in English, and Evaluate + CalcDate(DateFormula) give the same (spec 2.2).
fn calc(f: &str, base: &str) -> Result<String, DfError> {
    let a = datefml::parse(f, ENGLISH, Purpose::CalcDate)?;
    let b = datefml::parse(f, ENGLISH, Purpose::Evaluate)?;
    assert_eq!(a, b, "{f}: CalcDate(Text) and Evaluate read differently");
    let stored = datefml::parse_invariant(&a.stored()).expect("stored text reads back");
    assert_eq!(stored, a, "{f}: stored text does not read back");
    a.apply(day(base)).map(iso)
}

fn check_dates(rows: &[(&str, &str, &str, &str)]) {
    let mut bad = vec![];
    for (f, base, want, ev) in rows {
        match calc(f, base) {
            Ok(got) if got == *want => {}
            other => bad.push(format!("{f} @{base}: want {want}, got {other:?} [{ev}]")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

fn e(kind: ReadKind, quoted: &str) -> DfError {
    DfError::Read { kind, quoted: Ok(quoted.into()) }
}

const E1: ReadKind = ReadKind::NoUnit;
const E2: ReadKind = ReadKind::NoNumber;
const E3: ReadKind = ReadKind::NoSign;

// ---------------------------------------------------------------------------------- 3.1

#[test]
fn s3_1_count_terms() {
    const DAYS: &str = "T3:types/date-time/CalcDate.al::Days";
    const WEEKS: &str = "T3:types/date-time/CalcDate.al::Weeks";
    const MONTHS: &str = "T3:types/date-time/CalcDate.al::Months";
    const QUARTERS: &str = "T3:types/date-time/CalcDate.al::Quarters";
    const YEARS: &str = "T3:types/date-time/CalcDate.al::Years";
    const WDP: &str = "T3:types/date-time/DateFormulaSignedWd.al::SignedWD_Plus";
    const WDM: &str = "T3:types/date-time/DateFormulaSignedWd.al::SignedWD_Minus";
    const WDU: &str = "T3:types/date-time/DateFormulaEdges.al::WeekAndWeekdayUnits";
    check_dates(&[
        ("<1D>", B, "2024-01-16", DAYS),
        ("<-1D>", B, "2024-01-14", DAYS),
        ("<+1D>", B, "2024-01-16", DAYS),
        ("<0D>", B, "2024-01-15", DAYS),
        ("<30D>", B, "2024-02-14", DAYS),
        ("<365D>", B, "2025-01-14", DAYS),
        ("<366D>", B, "2025-01-15", DAYS),
        ("<-365D>", B, "2023-01-15", DAYS),
        ("<1D>", "2024-02-28", "2024-02-29", DAYS),
        ("<2D>", "2024-02-28", "2024-03-01", DAYS),
        ("<1D>", "2023-02-28", "2023-03-01", DAYS),
        ("<1D>", "2023-12-31", "2024-01-01", DAYS),
        ("<-1D>", "2024-01-01", "2023-12-31", DAYS),
        ("1D", B, "2024-01-16", DAYS),
        ("<1d>", B, "2024-01-16", DAYS),
        ("<1W>", B, "2024-01-22", WEEKS),
        ("<-1W>", B, "2024-01-08", WEEKS),
        ("<2W>", B, "2024-01-29", WEEKS),
        ("<0W>", B, "2024-01-15", WEEKS),
        ("<52W>", B, "2025-01-13", WEEKS),
        ("<1W+1D>", B, "2024-01-23", WEEKS),
        ("<1W-1D>", B, "2024-01-21", WEEKS),
        ("<1M>", B, "2024-02-15", MONTHS),
        ("<-1M>", B, "2023-12-15", MONTHS),
        ("<13M>", B, "2025-02-15", MONTHS),
        ("<0M>", B, "2024-01-15", MONTHS),
        ("<1M>", "2024-01-31", "2024-02-29", MONTHS),
        ("<1M>", "2023-01-31", "2023-02-28", MONTHS),
        ("<1M>", "2024-03-31", "2024-04-30", MONTHS),
        ("<-1M>", "2024-03-31", "2024-02-29", MONTHS),
        ("<-1M>", "2024-03-30", "2024-02-29", MONTHS),
        ("<12M>", "2024-02-29", "2025-02-28", MONTHS),
        ("<-12M>", "2024-02-29", "2023-02-28", MONTHS),
        ("<1M>", "2023-12-15", "2024-01-15", MONTHS),
        ("<1Q>", B, "2024-04-15", QUARTERS),
        ("<-1Q>", B, "2023-10-15", QUARTERS),
        ("<4Q>", B, "2025-01-15", QUARTERS),
        ("<1Q>", "2023-11-30", "2024-02-29", QUARTERS),
        ("<1Q>", "2023-08-31", "2023-11-30", QUARTERS),
        ("<1Y>", B, "2025-01-15", YEARS),
        ("<-1Y>", B, "2023-01-15", YEARS),
        ("<100Y>", B, "2124-01-15", YEARS),
        ("<0Y>", B, "2024-01-15", YEARS),
        ("<1Y>", "2024-02-29", "2025-02-28", YEARS),
        ("<-1Y>", "2024-02-29", "2023-02-28", YEARS),
        ("<4Y>", "2024-02-29", "2028-02-29", YEARS),
        ("<-4Y>", "2024-02-29", "2020-02-29", YEARS),
        ("<+1WD>", "2024-01-12", "2024-01-13", WDP),
        ("<+1WD>", "2024-01-13", "2024-01-14", WDP),
        ("<+1WD>", "2024-01-14", "2024-01-15", WDP),
        ("<+1WD>", "2024-01-15", "2024-01-16", WDP),
        ("<+1WD>", "2024-03-31", "2024-04-01", WDP),
        ("<+1WD>", "2024-12-31", "2025-01-01", WDP),
        ("<+1WD>", "2026-05-18", "2026-05-19", WDP),
        ("<-1WD>", "2024-01-12", "2024-01-11", WDM),
        ("<-1WD>", "2024-01-15", "2024-01-14", WDM),
        ("<-1WD>", "2024-03-31", "2024-03-30", WDM),
        ("<-1WD>", "2024-12-31", "2024-12-30", WDM),
        ("<1WD>", B, "2024-01-16", WDU),
        ("<2WD>", B, "2024-01-17", WDU),
        ("<1WD+1D>", B, "2024-01-17", WDU),
    ]);
}

// ---------------------------------------------------------------------------------- 3.2

#[test]
fn s3_2_current_terms() {
    const C: &str = "T3:types/date-time/CalcDate.al::Combined_And_Syntax/CD";
    const W: &str = "T3:types/date-time/CalcDate.al::Weeks";
    const M: &str = "T3:types/date-time/CalcDate.al::Months";
    const Q: &str = "T3:types/date-time/CalcDate.al::Quarters";
    const Y: &str = "T3:types/date-time/CalcDate.al::Years";
    check_dates(&[
        ("<CD>", B, "2024-01-15", C),
        ("<CW>", "2024-01-15", "2024-01-21", W),
        ("<CW>", "2024-01-17", "2024-01-21", W),
        ("<CW>", "2024-01-20", "2024-01-21", W),
        ("<CW>", "2024-01-21", "2024-01-21", W),
        ("<-CW>", "2024-01-17", "2024-01-15", W),
        ("<+CW>", "2024-01-17", "2024-01-21", W),
        ("<CM>", B, "2024-01-31", M),
        ("<-CM>", B, "2024-01-01", M),
        ("<CM>", "2024-02-10", "2024-02-29", M),
        ("<CM>", "2023-02-10", "2023-02-28", M),
        ("<CQ>", B, "2024-03-31", Q),
        ("<CQ>", "2024-02-15", "2024-03-31", Q),
        ("<CQ>", "2024-03-31", "2024-03-31", Q),
        ("<CQ>", "2024-04-01", "2024-06-30", Q),
        ("<CQ>", "2024-06-30", "2024-06-30", Q),
        ("<CQ>", "2024-07-01", "2024-09-30", Q),
        ("<CQ>", "2024-10-15", "2024-12-31", Q),
        ("<CQ>", "2024-12-31", "2024-12-31", Q),
        ("<-CQ>", B, "2024-01-01", Q),
        ("<-CQ>", "2024-05-15", "2024-04-01", Q),
        ("<-CQ>", "2024-11-15", "2024-10-01", Q),
        ("<CY>", "2024-06-15", "2024-12-31", Y),
        ("<CY>", "2024-01-01", "2024-12-31", Y),
        ("<CY>", "2024-12-31", "2024-12-31", Y),
        ("<-CY>", "2024-06-15", "2024-01-01", Y),
        ("<-CY>", "2024-01-01", "2024-01-01", Y),
    ]);
}

// ---------------------------------------------------------------------------------- 3.3

#[test]
fn s3_3_position_terms() {
    const WDN: &str = "T3:types/date-time/CalcDate.al::WD_Numbers";
    const WDT: &str = "T3:text/TestProbeTextDateFormatContracts.al::Dates_CalcDate_Formulas";
    const LRN: &str = "T2:L-CDT Example 2; T3:types/date-time/CalcDate.al::BT_LearnExample_1996";
    const D1: &str = "T3:types/date-time/DateFormulaEdges.al::SG_DayOfMonth_D1_D15";
    const DI: &str = "T3:types/date-time/DateFormulaInsidePeriod.al::Y7_CalcDate_D1_NotLastDay";
    const D29: &str = "T3:types/date-time/DateFormulaEdges.al::SG_DayOfMonth_D29_D30_D31";
    const DB: &str = "T3:types/date-time/DateFormulaEdges.al::SG_DayOfMonth_Backward";
    const DC: &str = "T3:types/date-time/DateFormulaEdges.al::SG_DayOfMonth_Combined";
    const M2: &str = "T3:types/date-time/DateFormulaEdges.al::TV_MonthNumber_M2";
    const NP4: &str = "T3:types/date-time/Np4DateFormulas.al::NP4_CalcDate_M1/M12/M3";
    const ME: &str = "T3:types/date-time/DateFormulaEdges.al::TV_MonthNumber_EdgesAndCombined";
    const QW: &str = "T3:types/date-time/DateFormulaEdges.al::TV_OtherUnitsWithNumber_W2_Q2_Y2";
    const Q3: &str = "T3:types/date-time/Np4DateFormulas.al::NP4_CalcDate_Q3; DateFormulaInsidePeriod.al::Y7_CalcDate_Q3_InsideQuarter";
    const W21: &str = "T3:types/date-time/DateFormulaInsidePeriod.al::Y7_CalcDate_W21";
    const W17: &str = "T3:types/date-time/Np4DateFormulas.al::NP4_CalcDate_W17; DateFormulaInsidePeriod.al::Y7_CalcDate_W17_InsideWeek";
    check_dates(&[
        ("<WD1>", "2024-01-17", "2024-01-22", WDN),
        ("<WD2>", "2024-01-17", "2024-01-23", WDN),
        ("<WD3>", "2024-01-17", "2024-01-24", WDN),
        ("<WD4>", "2024-01-17", "2024-01-18", WDN),
        ("<WD5>", "2024-01-17", "2024-01-19", WDN),
        ("<WD6>", "2024-01-17", "2024-01-20", WDN),
        ("<WD7>", "2024-01-17", "2024-01-21", WDN),
        ("<-WD1>", "2024-01-17", "2024-01-15", WDN),
        ("<-WD7>", "2024-01-17", "2024-01-14", WDN),
        ("<1W+WD1>", "2024-01-17", "2024-01-29", WDN),
        ("<WD1>", B, "2024-01-22", WDT),
        ("<-WD1>", B, "2024-01-08", WDT),
        ("<WD7>", B, "2024-01-21", WDT),
        ("<-WD2>", "1996-05-21", "1996-05-14", LRN),
        ("<D15>", "2024-01-10", "2024-01-15", D1),
        ("<D15>", "2024-01-15", "2024-02-15", D1),
        ("<D15>", "2024-01-31", "2024-02-15", D1),
        ("<D1>", "2024-01-01", "2024-02-01", DI),
        ("<D1>", "2024-12-31", "2025-01-01", D1),
        ("<D29>", "2023-02-28", "2023-02-28", D29),
        ("<D29>", "2024-01-31", "2024-02-29", D29),
        ("<D29>", "2024-02-29", "2024-03-29", D29),
        ("<D30>", "2024-02-29", "2024-02-29", D29),
        ("<D31>", "2024-04-30", "2024-04-30", D29),
        ("<D31>", "2024-01-31", "2024-02-29", D29),
        ("<D31>", "2023-06-30", "2023-06-30", "T3:types/date-time/Np4DateFormulas.al::NP4_CalcDate_D31"),
        ("<-D1>", "2024-01-15", "2024-01-01", DI),
        ("<-D1>", "2024-01-01", "2023-12-01", DI),
        ("<-D15>", "2024-01-15", "2023-12-15", DB),
        ("<-D15>", "2024-01-31", "2024-01-15", DB),
        ("<-D31>", "2024-02-29", "2024-01-31", DB),
        ("<-D31>", "2024-12-31", "2024-11-30", DB),
        ("<CM+D5>", "2024-01-31", "2024-02-05", DC),
        ("<-CM+D5>", "2024-01-31", "2024-01-05", DC),
        ("<1M+D15>", "2024-01-15", "2024-03-15", DC),
        ("<D15+1M>", "2024-01-15", "2024-03-15", DC),
        ("<M2>", "2024-01-31", "2024-02-01", M2),
        ("<M2>", "2024-02-29", "2025-02-01", M2),
        ("<M2>", "2023-02-28", "2024-02-01", M2),
        ("<M1>", "2024-01-01", "2025-01-01", NP4),
        ("<M1>", "2023-12-31", "2024-01-01", NP4),
        ("<M12>", "2024-12-31", "2025-12-01", NP4),
        ("<M3>", "2024-03-31", "2025-03-01", NP4),
        ("<-M2>", "2024-01-15", "2023-02-01", ME),
        ("<-M2>", "2024-04-30", "2024-02-01", ME),
        ("<-M2>", "2024-02-29", "2023-02-01", ME),
        ("<CM+M2>", B, "2024-02-01", ME),
        ("<M2+D10>", B, "2024-02-10", ME),
        ("<1Y+M2>", B, "2025-02-01", ME),
        ("<Q2>", B, "2024-04-01", QW),
        ("<Q2>", "2024-05-20", "2025-04-01", QW),
        ("<Q3>", "2024-06-15", "2024-07-01", Q3),
        ("<Q3>", "2024-07-01", "2025-07-01", Q3),
        ("<Q3>", "2023-12-31", "2024-07-01", Q3),
        ("<W2>", B, "2025-01-06", QW),
        ("<W2>", "2024-12-31", "2025-01-06", QW),
        ("<W53>", B, "2024-12-23", QW),
        ("<W21>", "2024-05-19", "2024-05-20", W21),
        ("<W21>", "2024-05-20", "2025-05-19", W21),
        ("<W21>", "2020-05-18", "2021-05-24", W21),
        ("<W21>", "2026-10-30", "2027-05-24", W21),
        ("<W17>", "2023-06-30", "2024-04-22", W17),
        ("<W17>", "2024-04-24", "2025-04-21", W17),
        ("<W17>", "2024-12-31", "2025-04-21", W17),
    ]);
    // `<Y2>`: T3:types/date-time/DateFormulaEdges.al::TV_OtherUnitsWithNumber_W2_Q2_Y2, the year of the century (T4:P03)
    assert_eq!(datefml::evaluate("<Y2>", ENGLISH, None).as_deref(), Ok("Y2"));
    assert_eq!(calc("<Y2>", B).as_deref(), Ok("2102-01-01"));
}

// ---------------------------------------------------------------------------------- 3.4

#[test]
fn s3_4_chains() {
    const Y: &str = "T3:types/date-time/CalcDate.al::Years";
    const C: &str = "T3:types/date-time/CalcDate.al::Combined_And_Syntax";
    const L: &str = "T2:L-CDT Example 2; T3:types/date-time/CalcDate.al::BT_LearnExample_1996";
    const Y64: &str = "T3:types/date-time/DateFormulaEdges.al::Y64_BracketedTerms_OtherAnchorsAndWorkDate";
    check_dates(&[
        ("<1Y+1Q+1M+1W+1D>", B, "2025-05-23", Y),
        ("<CM+1D>", B, "2024-02-01", C),
        ("<1D+CM>", B, "2024-01-31", C),
        ("<CM-1D>", B, "2024-01-30", C),
        ("<CM-CM>", B, "2024-01-01", C),
        ("<-1M+CM+1D>", B, "2024-01-01", C),
        ("<1M+CM>", B, "2024-02-29", "T3:types/date-time/CalcDate.al::Months"),
        ("<-1M+CM>", B, "2023-12-31", "T3:types/date-time/CalcDate.al::Months"),
        ("<CY+CM>", "2024-06-15", "2024-12-31", C),
        ("<CQ+CM>", "2024-05-15", "2024-06-30", C),
        ("<CW+CM>", "2024-05-15", "2024-05-31", C),
        ("<1D+1D>", B, "2024-01-17", C),
        ("<1D-1D>", B, "2024-01-15", C),
        ("<1M+1D+1M>", B, "2024-03-16", C),
        ("<CQ+1M-10D>", "1996-05-21", "1996-07-20", L),
        ("<CM+30D>", "1996-05-21", "1996-06-30", L),
        ("<CY><-10Y>", "2024-12-31", "2014-12-31", Y64),
        ("<CY><-10Y>", "2024-02-29", "2014-12-31", Y64),
        ("<CY><-10Y>", "2023-01-01", "2013-12-31", Y64),
        ("<CM><-1Y>", "2024-02-29", "2023-02-28", Y64),
        ("<CM><+1M>", "2024-01-31", "2024-02-29", Y64),
    ]);
    // T1:System Application/Test/Retention Policy/src/RetentionPeriodTest.Codeunit.al:75-96 with
    // RetentionPeriodImpl.Codeunit.al:16,26: <+CY+n Y>, n = 9999 - year(base)
    for base in ["2026-11-10", "2024-01-15", "9999-06-01"] {
        let n = 9999 - day_to_date(day(base)).unwrap().0;
        assert_eq!(calc(&format!("<+CY+{n}Y>"), base).as_deref(), Ok("9999-12-31"), "{base}");
    }
}

// ---------------------------------------------------------------------------------- 3.5

#[test]
fn s3_5_stored_text() {
    const BL: &str = "T3:types/TestDateFormulaDurationFormatContracts.al::DateFormula_Evaluate_BlankLikeTexts_AndGarbage; P131CalcDate.al::P131_EvaluateDateFormula_OddBrackets";
    const F1: &str = "T3:text/TestProbeTextDateFormatContracts.al::DateFormula_EvaluateForms; TestDateFormulaEvaluateLeftoversContracts.al::Evaluate_Accepted";
    const EV: &str = "T3:types/date-time/Evaluate.al::DateFormula_Evaluate";
    const PC: &str = "T3:types/date-time/DateFormulaEdges.al::Placeholders_PercentN";
    const BR: &str = "T3:types/date-time/P131CalcDate.al; P129CalcDateAngles.al; DateFormulaEdges.al::Y43_G413_NumberBeforeBracket_Mixed";
    const NB: &str = "T3:types/date-time/DateFormulaEdges.al::Y43_G413_*";
    const Y64: &str = "T3:types/date-time/DateFormulaEdges.al::Y64_BracketedTerms_SignedLaterTerms, MoreForms";
    const SP: &str = "T3:types/date-time/DateFormulaSpaces.al::EvaluateDateFormula_Spaces";
    let rows: &[(&str, &str, &str)] = &[
        ("", "", BL),
        ("   ", "", BL),
        ("<>", "", BL),
        ("< >", "", BL),
        ("<", "", BL),
        (">", "", BL),
        ("1D", "1D", F1),
        ("<1D>", "1D", F1),
        ("1d", "1D", F1),
        ("<1D", "1D", F1),
        ("1D>", "1D", F1),
        ("<<1D>>", "1D", F1),
        (" 1D ", "1D", F1),
        ("1 D", "1D", F1),
        ("< 1D >", "1D", F1),
        ("+1D", "+1D", EV),
        ("-1D", "-1D", EV),
        ("0D", "0D", BL),
        ("<0D>", "0D", BL),
        ("cm", "CM", EV),
        ("<-1M+CM>", "-1M+CM", EV),
        ("<1Y+1Q+1M+1W+1D>", "1Y+1Q+1M+1W+1D", EV),
        ("WD1", "WD1", F1),
        ("<-WD1>", "-WD1", F1),
        ("D1", "D1", F1),
        ("1Y-1D", "1Y-1D", F1),
        ("CQ", "CQ", F1),
        ("1Q", "1Q", F1),
        ("<+%2D>", "+2D", PC),
        ("+%1D>", "+1D", PC),
        ("<%1D>", "1D", PC),
        ("<+%0D>", "+0D", PC),
        ("8D+<1D>", "8D+1D", BR),
        ("<8D>+<1D>", "8D+1D", BR),
        ("<8D+<1D>>", "8D+1D", BR),
        ("5D<+1D>", "5D+1D", BR),
        ("+<1D>", "+1D", BR),
        ("-<62D>", "-62D", BR),
        ("2<D>", "2D", NB),
        ("-62<D>", "-62D", NB),
        ("<1>D", "1D", NB),
        ("1<>D", "1D", NB),
        ("1<WD>", "1WD", NB),
        ("1<D>+1<D>", "1D+1D", NB),
        ("<CY><-10Y>", "CY-10Y", Y64),
        ("<CY> <-10Y>", "CY-10Y", Y64),
        ("<CY>-10Y", "CY-10Y", Y64),
        ("CY<-10Y>", "CY-10Y", Y64),
        ("<cy><-10y>", "CY-10Y", Y64),
        ("<CY><-10Y", "CY-10Y", Y64),
        ("<-1Y><+CY>", "-1Y+CY", Y64),
        ("<CY><>", "CY", Y64),
        ("<WD2><+1W>", "WD2+1W", Y64),
        ("<CM + 1D>", "CM+1D", SP),
        ("<CM - 1D>", "CM-1D", SP),
        ("CM + 1D", "CM+1D", SP),
        // R8: InitValue texts (T3:format-evaluate/PnC12ClearEvaluate.al::InitValue_DateFormula)
        ("1Y", "1Y", "T3:format-evaluate/PnC12ClearEvaluate.al::InitValue_DateFormula"),
        ("<1M+CM>", "1M+CM", "T3:format-evaluate/PnC12ClearEvaluate.al::InitValue_DateFormula"),
        // T1:System Application/Test/Retention Policy/src/RetentionPeriodTest.Codeunit.al:297
        ("<+CY-24M>", "+CY-24M", "T1:RetentionPeriodTest.Codeunit.al:297"),
    ];
    let mut bad = vec![];
    for (input, want, ev) in rows {
        match datefml::evaluate(input, ENGLISH, None) {
            Ok(s) if s == *want => {}
            other => bad.push(format!("{input:?}: want {want:?}, got {other:?} [{ev}]")),
        }
        // the invariant reader (field values, filters) reads them the same
        assert_eq!(datefml::parse_invariant(input).map(|f| f.stored()).as_deref(), Ok(*want), "{input:?}");
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    // R44: 0D is a formula, not the empty one; terms are not merged or reordered
    assert!(!datefml::parse_invariant("0D").unwrap().is_empty());
    assert_ne!(datefml::evaluate("1D+1M", ENGLISH, None), datefml::evaluate("1M+1D", ENGLISH, None));
}

// ---------------------------------------------------------------------------------- 3.6

#[test]
fn s3_6_rejected_texts() {
    const INV: &str = "T3:types/date-time/CalcDate.al::Invalid_Formula_Evaluate";
    const SD: &str = "T3:types/date-time/DateFormulaEdges.al::SignsAndDigits_ErrorTexts";
    const P128: &str = "T3:types/date-time/P128CalcDateForms.al::P128_CalcDate_DateTextIsNotAFormula";
    const Y43: &str = "T3:types/date-time/DateFormulaEdges.al::Y43_G413_NumberBeforeBracket_More/Mixed";
    const E2S: &str = "T3:types/date-time/DateFormulaEdges.al::SignsAndDigits_ErrorTexts, WeekAndWeekdayUnits, TextsFromTheTestLibraries, Placeholders_PercentN; DateFormulaSignedWd.al";
    const SIG: &str = "T3:types/date-time/P131CalcDate.al; P129CalcDateAngles.al; P132DateFormula.al::P132_CalcDate_InnerBrackets; DateFormulaEdges.al";
    const NS: &str = "T3:types/P132DateFormula.al::P132_CalcDate_MissingSignErrors; DateFormulaEdges.al; CalcDate.al; P128CalcDateForms.al";
    const RNG: &str = "T3:types/date-time/DateFormulaEdges.al::SG_DayOfMonth_EdgeNumbersAndEvaluate, TV_MonthNumber_EdgesAndCombined, TV_OtherUnitsWithNumber_W2_Q2_Y2; TestProbeTextDateFormatContracts.al::wd_with_number_8";
    const QT: &str = "spec 2.9 %1 examples (P131, P129, P132, PageSetValueFormulaDuration df17, W2ValueControls)";
    let rows: &[(&str, DfError, &str)] = &[
        ("abc", e(E1, "ABC"), INV),
        ("<1X>", e(E1, "1X"), INV),
        ("<1.5D>", e(E1, "1.5D"), INV),
        ("<1CM>", e(E1, "1CM"), INV),
        ("<1MONDAY>", e(E1, "1MONDAY"), INV),
        ("<5>", e(E1, "5"), SD),
        ("<+5>", e(E1, "+5"), SD),
        ("10/16/26", e(E1, "10/16/26"), P128),
        ("2026-10-16", e(E1, "2026-10-16"), P128),
        ("10-16-26", e(E1, "10-16-26"), P128),
        ("1/2/24", e(E1, "1/2/24"), P128),
        ("<10/16/26>", e(E1, "10/16/26"), P128),
        ("01/01/26", e(E1, "01/01/26"), "T3:types/date-time/DateFormulaEdges.al::TextsFromTheTestLibraries"),
        ("-1<CM>", e(E1, "-1<CM>"), Y43),
        ("1<0D>", e(E1, "1<0D>"), Y43),
        ("<D>", e(E2, "D"), E2S),
        ("<WD>", e(E2, "WD"), E2S),
        ("<W>", e(E2, "W"), E2S),
        ("<Y>", e(E2, "Y"), E2S),
        ("Y", e(E2, "Y"), E2S),
        ("<+D>", e(E2, "+D"), E2S),
        ("<-D>", e(E2, "-D"), E2S),
        ("C<M>", e(E2, "C<M>"), Y43),
        ("<+%D>", e(E2, "+%D"), E2S),
        ("%Q", e(E2, "%Q"), E2S),
        ("%Y", e(E2, "%Y"), E2S),
        ("<++1D>", e(E2, "++1D"), SIG),
        ("<+-1D>", e(E2, "+-1D"), SIG),
        ("+", e(E2, "+"), SIG),
        ("<+", e(E2, "<+"), SIG),
        ("<->", e(E2, "-"), SIG),
        ("<1D+>", e(E2, "1D+"), SIG),
        ("<1D>+", e(E2, "<1D>+"), SIG),
        ("1D+<", e(E2, "1D+<"), SIG),
        ("<CY>+<-10Y>", e(E2, "CY>+<-10Y"), SIG),
        ("<CM1>", e(E3, "CM1"), NS),
        ("<2D15>", e(E3, "2D15"), NS),
        ("<2M2>", e(E3, "2M2"), NS),
        ("1D1W", e(E3, "1D1W"), NS),
        ("<1D1W>", e(E3, "1D1W"), NS),
        ("CM1D", e(E3, "CM1D"), NS),
        ("1D2WD", e(E3, "1D2WD"), NS),
        ("8D<1D>", e(E3, "8D<1D>"), NS),
        ("<1D><1D>", e(E3, "1D><1D"), NS),
        ("<-1Y><CY>", e(E3, "-1Y><CY"), NS),
        ("1D/1D", e(E3, "1D/1D"), NS),
        ("<-1D1M>", e(E3, "-1D1M"), NS),
        ("1Y2M", e(E3, "1Y2M"), "T3:pages/controls/PageSetValueFormulaDuration.al::DateFormulaControl_Spellings/df10"),
        ("<8D<1D", e(E3, "<8D<1D"), QT),
        ("1D abc", e(E3, "1D ABC"), QT),
        ("1M junk", e(E3, "1M JUNK"), QT),
        ("<1M>junk", e(E3, "<1M>JUNK"), QT),
        ("<D0>", e(ReadKind::TooLarge { min: 1, max: 31 }, "D0"), RNG),
        ("<D32>", e(ReadKind::TooLarge { min: 1, max: 31 }, "D32"), RNG),
        ("<M0>", e(ReadKind::TooLarge { min: 1, max: 12 }, "M0"), RNG),
        ("<M13>", e(ReadKind::TooLarge { min: 1, max: 12 }, "M13"), RNG),
        ("<Q5>", e(ReadKind::TooLarge { min: 1, max: 4 }, "Q5"), RNG),
        ("<WD8>", e(ReadKind::TooLarge { min: 1, max: 7 }, "WD8"), RNG),
        ("161026", e(ReadKind::TooLarge { min: 0, max: 9999 }, "161026"), P128),
    ];
    let mut bad = vec![];
    for (input, want, ev) in rows {
        for purpose in [Purpose::Evaluate, Purpose::CalcDate] {
            let got = datefml::parse(input, ENGLISH, purpose);
            if got.as_ref() != Err(want) {
                bad.push(format!("{input:?} ({purpose:?}): want {want:?}, got {got:?} [{ev}]"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    // Evaluate returns false (R40): the refusal is certain, the text may be open
    for input in ["99999D", "1P", "1D1M1Y", "LW", "<foo>", "xyz", "<MON>", "<2CW>", "<2WD1>", "<WD0>", "<-WED>", "<+WED>", "<1M+FRI>", "<1MON>"] {
        // T3:text/TestProbeTextDateFormatContracts.al::DateFormula_EvaluateForms; TestDateFormulaEvaluateLeftoversContracts.al::Evaluate_Refused;
        // types/date-time/Evaluate.al::DateFormula_Evaluate; CalcDate.al::WeekdayNames, Weeks, WD_Numbers
        let r = datefml::evaluate(input, ENGLISH, None);
        assert!(r.as_ref().is_err_and(|e| e.is_refusal()), "{input}: {r:?}");
    }
}

/// Every message template once, verbatim with its code (Q11; `\` for each line break, deviations
/// D1 and D2 in docs/assumptions.md). tests/vectors/u1-dateformula.tsv carries only kind, range and
/// the variable part of each error, so these are the texts.
#[test]
fn message_templates() {
    let m = |input: &str| {
        let e = datefml::parse(input, ENGLISH, Purpose::Evaluate).unwrap_err();
        (e.code(), e.message(ENGLISH).unwrap())
    };
    let calc = |input: &str, base: i32| {
        let e = datefml::calc_text(input, ENGLISH, base).unwrap_err();
        (e.code(), e.message(ENGLISH).unwrap())
    };
    let at = |iso: &str| (day(iso) * 2) as i32;
    let rows: &[((&str, String), &str, &str)] = &[
        // E1: T3:types/date-time/CalcDate.al::Invalid_Formula_Evaluate/abc; T4:P13eval
        (m("abc"), "Evaluate", r"The date formula 'ABC' should include a time unit.\\Time units can be: D,WD,W,M,Q, or Y.\C, meaning current time unit based on date, can be specified as a prefix to the time unit."),
        // E2: T3:types/date-time/CalcDate.al::Invalid_Formula_Evaluate/D alone; T1:ERMAccountSchedule.Codeunit.al:32
        (m("<D>"), "Evaluate", "The date formula 'D' should include a number."),
        // E3: T3:types/date-time/CalcDate.al::Invalid_Formula_Evaluate/CM1
        (m("<CM1>"), "Evaluate", "The date formula 'CM1' must include plus (+) or minus (-)."),
        // E4: T3:types/date-time/DateFormulaEdges.al::SG_DayOfMonth_EdgeNumbersAndEvaluate; T3:P128CalcDateForms.al/161026
        (m("<D32>"), "Evaluate", r"The date formula 'D32' contains a number that is too large.\\The number must be in the range 1 - 31."),
        (m("161026"), "Evaluate", r"The date formula '161026' contains a number that is too large.\\The number must be in the range 0 - 9999."),
        // E5: T3:types/date-time/CalcDate.al::CalcDate_Undefined_Base; T4:P07 (the empty formula too)
        (calc("<1D>", 0), "Argument", r"You cannot base a date calculation on an undefined date.\\Date: 0D\Formula: 1D."),
        (calc("", 0), "Argument", r"You cannot base a date calculation on an undefined date.\\Date: 0D\Formula: ."),
        // E6: T3:types/date-time/CalcDate.al::CalcDate_Text_Overload_Errors; T4:P06
        (calc("<1D>", at("9999-12-31")), "DateInvalid", "The date is not valid."),
        // E7: T4:P10 (the text as written, brackets kept)
        (m("<1D+1D+1D+1D+1D+1D+1D+1D+1D+1D+1D>"), "Evaluate", "The date formula '<1D+1D+1D+1D+1D+1D+1D+1D+1D+1D+1D>' exceeds the maximum length of 32."),
        // E8: T4:P06 (<W1> on 9999-12-01)
        (calc("<W1>", at("9999-12-01")), "DateInvalid", r"The calculation of the date formula W1 resulted in an invalid date.\\Only dates between Jan. 1, 0000 - Dec. 31, 9999 may be used."),
    ];
    for ((code, got), want_code, want) in rows {
        assert_eq!((*code, got.as_str()), (*want_code, *want));
    }
    // InvalidStandardFormat: T3:format-evaluate/Y62DateFormulaFormat.al::DateFormulaFormat_OtherStandards_English,
    // T4:P15fmt (English and German); 'DateFormula' for BC's 'Duration' is deviation D1
    let e = datefml::format("1D", 3, ENGLISH).unwrap_err();
    assert_eq!(e.code(), "InvalidStandardFormat");
    assert_eq!(e.message(ENGLISH).unwrap(), "Standard format number 3 does not exist for the type 'DateFormula'.");
    assert_eq!(e.message(1031).unwrap(), "Die Standardformatnummer 3 ist für den Typ 'DateFormula' nicht vorhanden.");
    // T1:Layers/W1/Tests/ERM-Finance/ERMAccountSchedule.Codeunit.al:1315 (DateFormulaWithoutNumber)
    assert!(m("<Y>").1.contains("should include a number."));
}


// ---------------------------------------------------------------------------------- 3.7

#[test]
fn s3_7_format_and_languages() {
    const Y62: &str = "T3:format-evaluate/Y62DateFormulaFormat.al";
    let f = |input: &str, std: i64, lang: i32| datefml::format(&datefml::evaluate(input, ENGLISH, None).unwrap(), std, lang);
    let rows: &[(&str, i32, [&str; 4], &str)] = &[
        ("<-1W>", 1033, ["-1W", "-1W", "<-1W>", "-1W"], Y62),
        ("<-1W>", 1030, ["-1U", "-1U", "<-1W>", "-1W"], "T1:RetentionPeriodTest.Codeunit.al:148"),
        ("<-1M>", 1033, ["-1M", "-1M", "<-1M>", "-1M"], Y62),
        ("<-1M>", 1030, ["-1M", "-1M", "<-1M>", "-1M"], "T1:RetentionPeriodTest.Codeunit.al:169"),
        ("<-1Y>", 1033, ["-1Y", "-1Y", "<-1Y>", "-1Y"], Y62),
        ("<-1Y>", 1030, ["-1Å", "-1Å", "<-1Y>", "-1Y"], "T1:RetentionPeriodTest.Codeunit.al:232"),
        ("<CM+1D>", 1033, ["CM+1D", "CM+1D", "<CM+1D>", "CM+1D"], Y62),
        ("<CM+1D>", 1030, ["LM+1D", "LM+1D", "<CM+1D>", "CM+1D"], Y62),
        ("<+CY-24M>", 1033, ["+CY-24M", "+CY-24M", "<+CY-24M>", "+CY-24M"], Y62),
        ("<+CY-24M>", 1030, ["+LÅ-24M", "+LÅ-24M", "<+CY-24M>", "+CY-24M"], "T1:RetentionPeriodTest.Codeunit.al:297"),
        ("", 1033, ["", "", "<>", ""], Y62),
        ("", 1030, ["", "", "<>", ""], Y62),
    ];
    for (input, lang, want, ev) in rows {
        for (i, std) in [0, 1, 2, 9].into_iter().enumerate() {
            assert_eq!(f(input, std, *lang).as_deref(), Ok(want[i]), "{input} {lang} format {std} [{ev}]");
        }
    }
    // German: format 1 and 2 (T3:format-evaluate/Y62DateFormulaFormat.al::DateFormulaFormat_Std1_German)
    for (input, f1, f2) in [("<-1W>", "-1W", "<-1W>"), ("<-1M>", "-1M", "<-1M>"), ("<-1Y>", "-1J", "<-1Y>"), ("<CM+1D>", "LM+1T", "<CM+1D>")] {
        assert_eq!(f(input, 1, 1031).as_deref(), Ok(f1), "{input}");
        assert_eq!(f(input, 2, 1031).as_deref(), Ok(f2), "{input}");
    }
    // T1:Layers/W1/Tests/General Journal/ERMGeneralJournalUT.Codeunit.al:6291 (German session, Format())
    assert_eq!(f("1Y", 0, 1031).as_deref(), Ok("1J"));
    // T2:L-CDT remarks: 1W+1D shows as 1S+1J (French) and 1S+1D (Spanish)
    assert_eq!(f("<1W+1D>", 0, 1036).as_deref(), Ok("1S+1J"));
    assert_eq!(f("<1W+1D>", 0, 1034).as_deref(), Ok("1S+1D"));
    // R54: formats 3..8 (T3:format-evaluate/Y62DateFormulaFormat.al::DateFormulaFormat_OtherStandards_English), deviation D1
    for std in 3..=8 {
        let e = f("<1D>", std, 1033).unwrap_err();
        assert_eq!(e.code(), "InvalidStandardFormat");
        assert_eq!(e.message(1033).unwrap(), format!("Standard format number {std} does not exist for the type 'DateFormula'."));
    }
    // Evaluate per language (T3:format-evaluate/Y62DateFormulaFormat.al::DateFormulaFormat_Std1_Danish_EvaluateBack)
    assert_eq!(datefml::evaluate("-1U", 1030, None).as_deref(), Ok("-1W"));
    assert!(datefml::evaluate("-1U", 1033, None).is_err_and(|e| e.is_refusal()));
    assert_eq!(datefml::evaluate("-1M", 1030, None).as_deref(), Ok("-1M"));
    assert_eq!(datefml::evaluate("-1M", 1033, None).as_deref(), Ok("-1M"));
    // T3:types/W7EvaluateTests.al::EvaluateDateFormula_Format0AndFormat9_NonEnglishFormulas, _Format2_
    for input in ["1T", "1J", "1A", "GM", "<1T>"] {
        assert!(datefml::evaluate(input, 1033, None).is_err_and(|e| e.is_refusal()), "{input}");
    }
    // R35: bracketed text is invariant in any language (T1:RetentionPeriodImpl.Codeunit.al:28 under 1030)
    assert_eq!(datefml::evaluate("<-1W>", 1030, None).as_deref(), Ok("-1W"));
    assert_eq!(datefml::evaluate("<+CY-24M>", 1030, None).as_deref(), Ok("+CY-24M"));
    // R38: localized output reads back in the same language (CalendarManagement.ReverseDateFormula)
    for (lang, input) in [(1031, "<1Y+CM-1D>"), (1030, "<-1Y+CM-1W>")] {
        let stored = datefml::evaluate(input, ENGLISH, None).unwrap();
        let shown = datefml::format(&stored, 0, lang).unwrap();
        assert_eq!(datefml::evaluate(&shown, lang, None).as_deref(), Ok(stored.as_str()), "{lang} {shown}");
    }
}

// ---------------------------------------------------------------------------------- 3.8

#[test]
fn s3_8_limits_undefined_closing() {
    const LIM: &str = "T3:types/date-time/CalcDate.al::CalcDate_Text_Overload_Errors";
    for (f, base) in [("<1D>", "9999-12-31"), ("<1M>", "9999-12-31"), ("<1W>", "9999-12-31"), ("<1Y>", "9999-01-01"), ("<-1D>", "0001-01-01"), ("<-1Y>", "0001-06-01")] {
        let e = calc(f, base).unwrap_err();
        assert_eq!(e, DfError::DateInvalid, "{f} @{base} [{LIM}]");
        assert_eq!(e.code(), "DateInvalid");
        assert_eq!(e.message(ENGLISH).unwrap(), "The date is not valid.");
    }
    assert_eq!(calc("<CY>", "9999-12-31").as_deref(), Ok("9999-12-31"), "{LIM}");
    assert_eq!(calc("", B).as_deref(), Ok(B), "{LIM}");
    // E5 (T3:types/date-time/CalcDate.al::CalcDate_Undefined_Base; T1:ERMPurchDocReports.Codeunit.al:28)
    for (f, stored) in [("<1D>", "1D"), ("<CM>", "CM"), ("<0D>", "0D"), ("<-1D>", "-1D"), ("<CY>", "CY"), ("<1W>", "1W"), ("<1M>", "1M"), ("<1Y>", "1Y")] {
        let e = datefml::calc_text(f, ENGLISH, 0).unwrap_err();
        assert_eq!(e.code(), "Argument", "{f}");
        assert_eq!(e.message(ENGLISH).unwrap(), format!(r"You cannot base a date calculation on an undefined date.\\Date: 0D\Formula: {stored}."));
    }
    // closing date base (T3:text/TestProbeTextDateFormatContracts.al::Dates_CalcDate_Formulas/closing_date_base, _cm):
    // the date value from the normal date, and the result is a closing date (T4:P08, Q9)
    let closing = (day("2023-12-31") * 2 + 1) as i32;
    assert_eq!(calc("<1D>", "2023-12-31").as_deref(), Ok("2024-01-01"));
    assert_eq!(calc("<CM>", "2023-12-31").as_deref(), Ok("2023-12-31"));
    assert_eq!(datefml::calc_text("<1D>", ENGLISH, closing), Ok((day("2024-01-01") * 2 + 1) as i32));
    assert_eq!(datefml::calc_text("<CM>", ENGLISH, closing), Ok((day("2023-12-31") * 2 + 1) as i32));
    // the Date encoding of the runtime: days * 2
    assert_eq!(datefml::calc_text("<1D>", ENGLISH, (day(B) * 2) as i32), Ok((day("2024-01-16") * 2) as i32));
}

// ---------------------------------------------------------------------------------- 3.9

#[test]
fn s3_9_filter_values_read_invariant() {
    // R80, R81: invariant letters, case-insensitive; stored texts compared
    // (T3:record/filters/Y89DateFormulaFilter.al::Y89_DF_Single; T3:types/P136Filter.al::P136_SetFilter_DateFormula)
    let s = |t: &str| datefml::parse_invariant(t).map(|f| f.stored());
    assert_eq!(s("1M"), s("1m"));
    assert_eq!(s("1D").as_deref(), Ok("1D"));
    assert_ne!(s("+1M"), s("1M"));
    assert_ne!(s("1M+1D"), s("1D+1M"));
    assert_eq!(s("1M1D"), Err(e(E3, "1M1D")));
    assert_eq!(s("x"), Err(e(E1, "X")));
    assert_eq!(s("*M"), Err(e(E2, "*M")));
}

// ---------------------------------------------------------------------------------- section 4

/// Points the spec left OPEN that the U1 probes settled (T4:Pnn, BC 29.1); more rows of each in
/// tests/u1_vectors.rs.
#[test]
fn s4_settled_by_the_probes() {
    let ev = |t: &str| datefml::evaluate(t, ENGLISH, None);
    let ok = |t: &str| Ok::<String, DfError>(t.to_string());
    // T4:P01: leading zeros stay; every number above 9999 raises E4 0 - 9999
    assert_eq!(ev("<01D>"), ok("01D"));
    assert_eq!(ev("<D05>"), ok("D05"));
    assert_eq!(ev("99999D"), Err(e(ReadKind::TooLarge { min: 0, max: 9999 }, "99999D")));
    // T4:P02: W 1 - 53, Y 0 - 99, WD0 E4; a position number above 9999 reads as none (E2)
    assert_eq!(ev("<W1>"), ok("W1"));
    assert_eq!(ev("<W54>"), Err(e(ReadKind::TooLarge { min: 1, max: 53 }, "W54")));
    assert_eq!(ev("<Y0>"), ok("Y0"));
    assert_eq!(ev("<Y100>"), Err(e(ReadKind::TooLarge { min: 0, max: 99 }, "Y100")));
    assert_eq!(ev("<WD0>"), Err(e(ReadKind::TooLarge { min: 1, max: 7 }, "WD0")));
    assert_eq!(ev("<D10000>"), Err(e(E2, "D10000")));
    // T4:P03: Yn is the year of the century, its sign ignored; -Wn, -Qn; Wn from ISO week 53
    assert_eq!(calc("<-Y2>", B), calc("<Y2>", B));
    assert_eq!(calc("<-W2>", B).as_deref(), Ok("2024-01-08"));
    assert_eq!(calc("<-Q1>", "2024-02-28").as_deref(), Ok("2024-01-01"));
    assert_eq!(calc("<-Q1>", "2024-01-31").as_deref(), Ok("2023-01-01"));
    assert_eq!(calc("<W2>", "2026-12-31").as_deref(), Ok("2027-01-11"));
    // T4:P04: CWD, -CD and +CD keep the date
    for f in ["<CWD>", "<-CD>", "<+CD>", "<-CWD>"] {
        assert_eq!(calc(f, B).as_deref(), Ok(B), "{f}");
    }
    // T4:P05: nWD counts calendar days for every n
    assert_eq!(calc("<3WD>", B).as_deref(), Ok("2024-01-18"));
    assert_eq!(calc("<0WD>", B).as_deref(), Ok(B));
    // T4:P06: every step must stay inside 0001-01-02..9999-12-31
    assert_eq!(calc("<-1D+1D>", "0001-01-02"), Err(DfError::DateInvalid));
    assert_eq!(calc("<-1D>", "0001-01-02"), Err(DfError::DateInvalid));
    assert_eq!(calc("<W1>", "9999-12-01"), Err(DfError::WeekBeyondRange { formula: "W1".into() }));
    // T4:P07: the empty formula on 0D; a reading error wins over E5
    assert_eq!(
        datefml::calc_text("", ENGLISH, 0).unwrap_err().message(ENGLISH).as_deref(),
        Ok(r"You cannot base a date calculation on an undefined date.\\Date: 0D\Formula: .")
    );
    assert_eq!(datefml::calc_text("abc", ENGLISH, 0), Err(e(E1, "ABC")));
    // T4:P09: separators split runs and are dropped; junk characters; any other character is a letter
    assert_eq!(ev("1 0D"), Err(e(E1, "1 0D")));
    assert_eq!(ev("1%0D"), Err(e(E1, "1%0D")));
    assert_eq!(ev("W D1"), Err(e(E2, "W D1")));
    assert_eq!(ev("1D;"), ok("1D"));
    assert_eq!(ev("#"), ok(""));
    assert_eq!(ev("\u{8}1D"), Err(e(E1, "\u{8}1D")));
    assert_eq!(ev("Å"), Err(e(E1, "Å")));
    // T4:P10: 32 characters are read, 33 are too long
    let t32 = "1D+1D+1D+1D+1D+1D+1D+1D+1D+1D+1D";
    assert_eq!(ev(t32).map(|s| s.len()), Ok(32));
    assert_eq!(ev(&format!(" {t32}")), Err(DfError::Read { kind: ReadKind::TooLong, quoted: Ok(format!(" {t32}").into()) }));
    // T4:P11: letters of other languages
    assert_eq!(datefml::format("1D", 0, 1049).as_deref(), Ok("1Д"));
    assert_eq!(datefml::format("CM", 0, 1032).as_deref(), Ok("ΤΜ"));
    // T4:P12: one bracket reads the session's letters; English letters are refused in German
    assert!(datefml::evaluate("<1W", 1030, None).is_err_and(|e| e.is_refusal()));
    assert!(datefml::evaluate("1D", 1031, None).is_err_and(|e| e.is_refusal()));
    assert_eq!(datefml::evaluate("1T", 1031, None).as_deref(), Ok("1D"));
    // T4:P13: %1 keeps outer spaces
    assert_eq!(ev(" abc "), Err(e(E1, " ABC ")));
    // T4:P14eq: equality of the stored text
    assert_eq!(datefml::equal("+1D", "1D"), Ok(false));
    assert_eq!(datefml::equal("1D+1W", "1W+1D"), Ok(false));
    // T4:P15: formats other than 0, 1, 2, 9; a length; Evaluate with number 9 reads invariant letters
    assert_eq!(datefml::format("1D", 10, ENGLISH), Err(DfError::InvalidStandardFormat(10)));
    assert_eq!(datefml::format("1D", -1, ENGLISH), Err(DfError::InvalidStandardFormat(-1)));
    assert_eq!(datefml::format_with("-CM+1D", 7, 0, ENGLISH).as_deref(), Ok(" -CM+1D"));
    assert_eq!(datefml::format_with("-CM+1D", 5, 0, ENGLISH).as_deref(), Ok("-CM+1"));
    assert_eq!(datefml::format_with("-CM+1D", -2, 0, ENGLISH).as_deref(), Ok("-C"));
    assert_eq!(datefml::evaluate("1D", 1031, Some(9)).as_deref(), Ok("1D"));
    assert!(datefml::evaluate("1T", 1031, Some(9)).is_err());
    assert_eq!(datefml::evaluate("1T", 1031, Some(2)).as_deref(), Ok("1D"));
    // T4:P16: text outside Format is format 0
    assert_eq!(datefml::text("1D", 1031).as_deref(), Ok("1T"));
    // T4:P17: CalcDate(Text) reads the session's letters
    assert_eq!(datefml::calc_text("1T", 1031, (day(B) * 2) as i32), Ok((day("2024-01-16") * 2) as i32));
}

/// Points still open after the probes: each returns `Unsettled` (or `Refused`) with its probe.
#[test]
fn s4_still_open() {
    use Probe::*;
    let u = |p: Probe| Err::<String, DfError>(DfError::Unsettled(p));
    let ev = |t: &str| datefml::evaluate(t, ENGLISH, None);
    // P09: U+0000..U+0007 and digits of other scripts (T4:P09: those inputs are not settled)
    assert_eq!(ev("\u{1}1D"), u(P09));
    assert_eq!(ev("1\u{660}D"), u(P09));
    // P10: a text longer than 32 characters that also fails to read; the case of the quoted text
    assert_eq!(ev(&"abc ".repeat(9)), Err(DfError::Refused(P10)));
    let lower = "1d+1d+1d+1d+1d+1d+1d+1d+1d+1d+1d+1d";
    assert_eq!(ev(lower).unwrap_err().message(ENGLISH), Err(P10));
    // P11, P12, P17: the C and D letters of Vietnamese; language ids BC does not list
    assert_eq!(datefml::format("1D", 0, 1066), u(P11));
    assert_eq!(datefml::format("1D", 0, 4242), u(P11));
    assert_eq!(datefml::evaluate("1X", 1066, None), u(P12));
    assert_eq!(datefml::evaluate("1D", 4242, None), u(P12));
    assert_eq!(datefml::calc_text("1X", 1066, (day(B) * 2) as i32), Err(DfError::Unsettled(P17)));
    // P13: messages outside English (English in one German session, German in another)
    assert_eq!(datefml::evaluate("abc", 1031, None).unwrap_err().message(1031), Err(P13));
    // P14: the order of different values
    assert_eq!(datefml::order("1D", "1M"), Err(DfError::Unsettled(P14)));
    assert_eq!(datefml::parse_invariant("1D").unwrap().sort_key(), Err(DfError::Unsettled(P14)));
    // P15: a length with a format number other than 0
    assert_eq!(datefml::format_with("1D", 5, 2, ENGLISH), u(P15));
    assert!(datefml::unsettled_text(P13).starts_with("U1 unsettled: P13"));
}

// ---------------------------------------------------------------------------------- Q8

/// xorshift64*: a fixed seed, so a failure is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Q8: reading and applying never panic, for any text, date and language, and every formula
/// reads back from its stored text.
#[test]
fn q8_no_panic_and_round_trip() {
    const PIECES: &[&str] = &[
        "0", "1", "2", "9", "15", "31", "53", "9999", "10000", "0007", "99999999999999999999", "D", "WD", "W", "M", "Q", "Y", "C", "CM", "CW", "L", "T",
        "J", "U", "S", "Å", "å", "ä", "ß", "d", "wd", "x", "MON", "+", "-", "<", ">", " ", "%", "/", ".", "*", "\t", "\u{0}", "é", "日", "😀", "1D", "CY",
    ];
    const LANGS: &[i32] = &[1033, 1030, 1031, 1034, 1036, 1044, 0, -1, i32::MAX, i32::MIN];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ok = 0usize;
    for _ in 0..60_000 {
        let n = rng.below(12) as usize;
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(PIECES[rng.below(PIECES.len() as u64) as usize]);
        }
        let lang = LANGS[rng.below(LANGS.len() as u64) as usize];
        let date = match rng.below(4) {
            0 => 0,
            1 => rng.next() as i32,
            2 => (rng.below(datefml::MAX_DAY as u64) as i32 + 1) * 2 + (rng.below(2) as i32),
            _ => (datefml::MAX_DAY as i32 - rng.below(400) as i32) * 2,
        };
        for purpose in [Purpose::Evaluate, Purpose::CalcDate] {
            let r = datefml::parse(&s, lang, purpose);
            if let Err(e) = &r {
                let _ = e.message(lang);
                let _ = e.code();
            }
            if let Ok(f) = r {
                ok += 1;
                let stored = f.stored();
                assert_eq!(datefml::parse_invariant(&stored), Ok(f), "{s:?} -> {stored:?}");
                for l in [1033, 1030, 1031, 1036, 1044] {
                    for std in [0, 1, 2, 9, 3, 12] {
                        let _ = datefml::format(&stored, std, l);
                    }
                    for len in [i64::MIN, -1, 1, 7, 1000, 1_000_001, i64::MAX] {
                        let _ = datefml::format_with(&stored, len, 0, l);
                    }
                }
                if let Ok(r) = f.calc(date, lang) {
                    assert!(r >= 2 && r <= (datefml::MAX_DAY as i32) * 2 + 1, "{s:?} @{date}: {r}");
                }
                for _ in 0..3 {
                    if let Ok(d) = f.apply(rng.below(datefml::MAX_DAY as u64) as i64 + 1) {
                        assert!((datefml::MIN_DAY..=datefml::MAX_DAY).contains(&d));
                    }
                }
            }
        }
        let _ = datefml::calc_text(&s, lang, date);
        let _ = datefml::format(&s, rng.below(20) as i64 - 5, lang);
        let _ = datefml::equal(&s, "1D");
    }
    assert!(ok > 1000, "too few readable formulas generated: {ok}");
}

/// Q8: every formula the model can hold renders to a text that reads back to it.
#[test]
fn q8_render_parse_round_trip_exhaustive_shapes() {
    let mut texts = vec![];
    for u in ["D", "WD", "W", "M", "Q", "Y"] {
        for n in [0, 1, 7, 12, 99, 9999] {
            texts.push(format!("{n}{u}"));
        }
        texts.push(format!("C{u}"));
    }
    for t in ["D1", "D31", "WD1", "WD7", "M1", "M12", "Q1", "Q4", "W2", "W53", "Y2"] {
        texts.push(t.to_string());
    }
    let mut n = 0;
    for a in &texts {
        for sign in ["", "+", "-"] {
            for b in &texts {
                let t = format!("{sign}{a}-{b}");
                if let Ok(f) = datefml::parse_invariant(&t) {
                    n += 1;
                    assert_eq!(datefml::parse_invariant(&f.stored()), Ok(f), "{t}");
                    assert_eq!(f.stored(), t, "stored text keeps the written form (R44)");
                }
            }
        }
    }
    assert!(n > 1000);
}

// ---------------------------------------------------------------------------------- Q7 cache

#[test]
fn q7_cache_reads_once_and_by_language() {
    let mut c = datefml::Cache::new();
    let base = (day(B) * 2) as i32;
    assert_eq!(c.calc_stored("1M", ENGLISH, base), Ok((day("2024-02-15") * 2) as i32));
    assert_eq!(c.calc_stored("1M", ENGLISH, base), Ok((day("2024-02-15") * 2) as i32));
    assert_eq!(c.len(), 1);
    // unbracketed text depends on the language: -1U is a week in Danish, refused in English
    assert_eq!(c.calc_text("-1U", 1030, base), Ok((day("2024-01-08") * 2) as i32));
    assert!(c.calc_text("-1U", 1033, base).is_err());
    assert_eq!(c.calc_text("-1U", 1030, base), Ok((day("2024-01-08") * 2) as i32));
    // bracketed text reads the same in every language
    assert_eq!(c.calc_text("<1W>", 1030, base), c.calc_text("<1W>", 1031, base));
    // bounded
    for i in 0..(datefml::CACHE_CAP * 3) {
        let _ = c.calc_stored(&format!("{}D", i % 10000), ENGLISH, base);
    }
    assert!(c.len() <= datefml::CACHE_CAP * 2);
}

#[test]
fn q7_formula_is_plain_data() {
    // applying a parsed formula works on a Copy value: no heap, no shared state
    fn is_copy<T: Copy + Send + Sync>() {}
    is_copy::<Formula>();
    assert!(std::mem::size_of::<Formula>() <= 128);
}

// ---------------------------------------------------------------------------------- sessions (Q4, Q5, Q6, Q13, R41)

mod sessions {
    use al_model::{Catalog, ProcRef};
    use al_runtime::{Flow, Instance, Runtime, Session};
    use bc_store::{MemBackend, Store};
    use std::sync::Arc;

    const APP: &str = r#"
table 50960 "U1 DF Row"
{
    fields
    {
        field(1; "No."; Integer) { }
        // R8: InitValue texts
        field(2; F; DateFormula) { InitValue = '1Y'; }
        field(3; G; DateFormula) { InitValue = '<1M+CM>'; }
    }
    keys { key(PK; "No.") { } }
}

codeunit 50960 "U1 DF Tests"
{
    procedure Danish(): Text
    begin
        GlobalLanguage(1030);
        exit(Format(GlobalLanguage()));
    end;

    procedure German(): Text
    begin
        GlobalLanguage(1031);
        exit(Format(GlobalLanguage()));
    end;

    procedure FormatCurrent(): Text
    var
        DF: DateFormula;
    begin
        Evaluate(DF, '<CM+1D>');
        exit(Format(DF) + '|' + Format(DF, 0, 2) + '|' + Format(DF, 0, 9));
    end;

    // Q4: a stored value applies in any session language; a Text formula is read in it
    procedure StoredInGerman(): Text
    var
        DF: DateFormula;
        R: Text;
    begin
        Evaluate(DF, '1D');
        GlobalLanguage(1031);
        R := Format(CalcDate(DF, 20240115D), 0, 9);
        asserterror R += Format(CalcDate('1D', 20240115D), 0, 9);
        R += '|' + CopyStr(GetLastErrorText(), 1, 17);
        R += '|' + Format(CalcDate('<1D>', 20240115D), 0, 9);
        GlobalLanguage(1033);
        exit(R);
    end;

    // Q13, R8: field values from code (InitValue) read with the invariant letters
    procedure InitValuesInGerman(): Text
    var
        Row: Record "U1 DF Row";
        R: Text;
    begin
        GlobalLanguage(1031);
        Row.Init();
        R := Format(Row.F, 0, 9) + '|' + Format(Row.G, 0, 9) + '|' + Format(Row.F);
        GlobalLanguage(1033);
        exit(R);
    end;

    // R40, R41: return value false; statement form raises the reading error, code Evaluate
    procedure EvaluateForms(): Text
    var
        DF: DateFormula;
        Ok: Boolean;
        R: Text;
    begin
        Evaluate(DF, '<1D>');
        Ok := Evaluate(DF, 'MON');
        R := Format(Ok) + Format(DF);
        asserterror Evaluate(DF, 'abc');
        exit(R + '|' + GetLastErrorCode() + '|' + CopyStr(GetLastErrorText(), 1, 50) + '|' + Format(DF));
    end;

    // R63, Q2: E5 with code Argument
    procedure UndefinedBase(): Text
    var
        D: Date;
    begin
        asserterror D := CalcDate('<1D>', 0D);
        exit(GetLastErrorCode() + '|' + GetLastErrorText());
    end;

    // R5, Q6: = and <> by stored text
    procedure Compare(): Text
    var
        A: DateFormula;
        B: DateFormula;
        C: DateFormula;
    begin
        Evaluate(A, '<1D>');
        Evaluate(B, '1d');
        Evaluate(C, '<1M>');
        exit(Format(A = B) + Format(A <> C) + Format(A = C));
    end;

    // R54: formats 3..8
    procedure Format3(): Text
    var
        DF: DateFormula;
        T: Text;
    begin
        Evaluate(DF, '<1D>');
        asserterror T := Format(DF, 0, 3);
        exit(GetLastErrorCode() + '|' + GetLastErrorText());
    end;

    // T4:P16: DateFormula text in StrSubstNo is format 0 of the session
    procedure SubstGerman(): Text
    var
        DF: DateFormula;
        T: Text;
    begin
        Evaluate(DF, '<1D>');
        T := StrSubstNo('%1', DF);
        GlobalLanguage(1031);
        T += '|' + StrSubstNo('%1', DF);
        GlobalLanguage(1033);
        exit(T);
    end;
}
"#;

    fn catalog() -> &'static Catalog {
        let dir = std::env::temp_dir().join(format!("u1-df-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("App.al"), APP).unwrap();
        let cat = Catalog::load(&[dir.clone()]).expect("catalog");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
        Box::leak(Box::new(cat))
    }

    fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
        let ci = cat.code.iter().position(|c| c.name == "U1 DF Tests").unwrap() as u32;
        let p = cat.code[ci as usize].find(proc)[0];
        match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
            Ok(v) => v.to_text(),
            Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
            Err(_) => panic!("{proc}: unexpected flow"),
        }
    }

    #[test]
    fn q5_two_sessions_two_languages_one_thread() {
        let cat = catalog();
        let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
        let mut a = Session::new(&rt, "CRONUS");
        let mut b = Session::new(&rt, "CRONUS");
        assert_eq!(call(&mut a, cat, "Danish"), "1030");
        assert_eq!(call(&mut b, cat, "German"), "1031");
        // interleaved: each session keeps its own letters (T3:format-evaluate/Y62DateFormulaFormat.al)
        for _ in 0..2 {
            assert_eq!(call(&mut a, cat, "FormatCurrent"), "LM+1D|<CM+1D>|CM+1D");
            assert_eq!(call(&mut b, cat, "FormatCurrent"), "LM+1T|<CM+1D>|CM+1D");
        }
    }

    #[test]
    fn q4_q13_r41_r54_r63_sessions() {
        let cat = catalog();
        let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
        let mut s = Session::new(&rt, "CRONUS");
        // Q4: the stored 1D applies in German; unbracketed '1D' is refused in German (T4:P17, its text
        // outside English is P13); bracketed is invariant
        assert_eq!(call(&mut s, cat, "StoredInGerman"), "2024-01-16|U1 unsettled: P13|2024-01-16");
        // Q13, R8
        assert_eq!(call(&mut s, cat, "InitValuesInGerman"), "1Y|1M+CM|1J");
        // R40, R41
        assert_eq!(call(&mut s, cat, "EvaluateForms"), "No1D|Evaluate|The date formula 'ABC' should include a time unit.|1D");
        // R63, Q2
        assert_eq!(call(&mut s, cat, "UndefinedBase"), r"Argument|You cannot base a date calculation on an undefined date.\\Date: 0D\Formula: 1D.");
        // R5, Q6
        assert_eq!(call(&mut s, cat, "Compare"), "YesYesNo");
        // R54, deviation D1
        assert_eq!(call(&mut s, cat, "Format3"), "InvalidStandardFormat|Standard format number 3 does not exist for the type 'DateFormula'.");
        // T4:P16
        assert_eq!(call(&mut s, cat, "SubstGerman"), "1D|1T");
    }

    #[test]
    fn q6_value_ordering_does_not_read_again() {
        use al_runtime::value::{compare, equals};
        use al_runtime::Val;
        let a = Val::DateFormula("1D".into());
        let b = Val::DateFormula("1D".into());
        let c = Val::DateFormula("1M".into());
        assert!(equals(&a, &b));
        assert!(!equals(&a, &c));
        assert_eq!(compare(&a, &b), std::cmp::Ordering::Equal);
        // different values: an order is needed here, recorded as the P14 assumption
        al_runtime::assume::reset();
        assert_ne!(compare(&a, &c), std::cmp::Ordering::Equal);
        assert!(al_runtime::assume::take().assumed.iter().any(|(n, _)| *n == "DateFormula.P14"));
    }
}
