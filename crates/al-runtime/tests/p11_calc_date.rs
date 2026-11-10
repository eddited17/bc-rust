//! CalcDate with a day-of-month / weekday number (`<D10>`, `<WD1>`). Only cases a U1 spec rule or
//! a U1 probe (T4) covers are kept: R24, R78 and E4 of the spec; T4:P01s-pos for `<WD0>` (E4 1 - 7)
//! and T4:P02 for `<D99999999999>` (a position number above 9999 reads as no number: E2). The E4
//! text has the spec's literal backslashes.
use al_runtime::builtins::calc_date;
use al_runtime::datefml::{DfError, ReadKind, ENGLISH};
use chrono::{Datelike, NaiveDate};

fn d(y: i32, m: u32, day: u32) -> i32 {
    NaiveDate::from_ymd_opt(y, m, day).unwrap().num_days_from_ce() * 2
}

#[test]
fn day_and_weekday_numbers_in_range() {
    // 2026-11-07 is a Saturday; R78 (D forward, WD forward, CM then D)
    let base = d(2026, 11, 7);
    assert_eq!(calc_date("<D10>", ENGLISH, base), Ok(d(2026, 11, 10)));
    assert_eq!(calc_date("<D7>", ENGLISH, base), Ok(d(2026, 12, 7)));
    assert_eq!(calc_date("<D30>", ENGLISH, base), Ok(d(2026, 11, 30)));
    assert_eq!(calc_date("<CM+D10>", ENGLISH, base), Ok(d(2026, 12, 10)));
    assert_eq!(calc_date("<WD1>", ENGLISH, base), Ok(d(2026, 11, 9)));
    assert_eq!(calc_date("<WD7>", ENGLISH, base), Ok(d(2026, 11, 8)));
}

#[test]
fn day_and_weekday_numbers_out_of_range_are_errors() {
    // R24 and E4
    let base = d(2026, 11, 7);
    for (f, min, max) in [("<D0>", 1, 31), ("<D32>", 1, 31), ("<WD0>", 1, 7), ("<WD8>", 1, 7)] {
        let e = calc_date(f, ENGLISH, base).expect_err(f);
        assert!(matches!(e, DfError::Read { kind: ReadKind::TooLarge { min: a, max: b }, .. } if a == min && b == max), "{f}: {e:?}");
        let m = e.message(ENGLISH).unwrap();
        assert!(m.starts_with("The date formula '"), "{f}: {m}");
        assert!(m.contains("contains a number that is too large."), "{f}: {m}");
    }
    assert_eq!(
        calc_date("<WD8>", ENGLISH, base).unwrap_err().message(ENGLISH).unwrap(),
        "The date formula 'WD8' contains a number that is too large.\\\\The number must be in the range 1 - 7."
    );
    assert!(calc_date("<D0>", ENGLISH, base).unwrap_err().message(ENGLISH).unwrap().ends_with("The number must be in the range 1 - 31."));
    assert!(matches!(calc_date("<D99999999999>", ENGLISH, base), Err(DfError::Read { kind: ReadKind::NoNumber, .. })));
}
