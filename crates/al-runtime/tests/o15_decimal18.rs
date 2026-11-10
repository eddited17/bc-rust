//! An AL Decimal arithmetic result keeps at most 18 significant digits of its coefficient, rounded, and a
//! rounded fraction drops its trailing zeros [src:A31.1]; ties away from zero are an open question
//! [src:A31.3]. The ledger oracle (I14/I15, tools/oracle) found it in the Currency
//! Factor columns of real BC 29.1: 100 / 85.68024 = 1.16713025080228533 and 100 / 73.301 = 1.36423786851475423
//! there, where bc-rust kept .NET's 28 digits (1.1671302508022853344014909389).
use al_model::Catalog;
use al_runtime::interp::dec18;
use al_runtime::{Runtime, Session, Val};
use al_syntax::ast::BinOp;
use bc_store::{MemBackend, Store};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[test]
fn dec18_rounds_to_18_significant_digits() {
    // what BC 29.1 stored for the oracle's EUR and USD factors (Cust. Ledger Entry, Adjusted Currency Factor)
    assert_eq!(dec18(d("1.1671302508022853344014909389")).to_string(), "1.16713025080228533");
    assert_eq!(dec18(d("1.364237868514754232547987067")).to_string(), "1.36423786851475423");
    // half away from zero, both signs; trailing zeros of a rounded fraction dropped
    assert_eq!(dec18(d("0.12345678901234567850")).to_string(), "0.123456789012345679");
    assert_eq!(dec18(d("-0.12345678901234567850")).to_string(), "-0.123456789012345679");
    assert_eq!(dec18(d("1.00000000000000000049")).to_string(), "1");
    // 18 digits or fewer: unchanged, scale kept
    assert_eq!(dec18(d("123.4500")).to_string(), "123.4500");
    assert_eq!(dec18(d("0.000123456789012345678")).to_string(), "0.000123456789012345678");
    assert_eq!(dec18(d("999999999999999999")).to_string(), "999999999999999999");
    // more than 18 integer digits: the coefficient is rounded, no fraction is left
    assert_eq!(dec18(d("12345678901234567891.5")).to_string(), "12345678901234567900");
    assert_eq!(dec18(d("-1234567890123456789")).to_string(), "-1234567890123456790");
}

const APP_JSON: &str = r#"{ "id": "0e15d18a-0f15-4d18-a015-0d18c0de0018", "name": "O15 Decimal18 Test", "publisher": "bc-rust", "version": "1.0.0.0" }"#;
const APP: &str = "codeunit 50918 \"O15 Decimal18\" { }\n";

#[test]
fn al_arithmetic_is_decimal18() {
    let dir = std::env::temp_dir().join(format!("o15-decimal18-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat: &'static Catalog = Box::leak(Box::new(Catalog::load(&[dir.clone()]).expect("catalog")));
    let _ = std::fs::remove_dir_all(&dir);
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let dec = |v: &str| Val::Dec(d(v));
    let ev = |s: &mut Session, op, a: Val, b: Val| match s.binop(op, a, b) {
        Ok(Val::Dec(x)) => x.to_string(),
        Ok(v) => format!("{v:?}"),
        Err(_) => "error".into(),
    };
    // CurrExchRate.ExchangeRate: "Exchange Rate Amount" / "Relational Exch. Rate Amount"
    assert_eq!(ev(&mut s, BinOp::Div, dec("100"), dec("85.68024")), "1.16713025080228533");
    assert_eq!(ev(&mut s, BinOp::Div, dec("100"), dec("73.301")), "1.36423786851475423");
    assert_eq!(ev(&mut s, BinOp::Div, Val::Int(1), Val::Int(3)), "0.333333333333333333");
    assert_eq!(ev(&mut s, BinOp::Mul, dec("1.16713025080228533"), dec("1.16713025080228533")), "1.36219302233780546");
    assert_eq!(ev(&mut s, BinOp::Add, dec("0.123456789012345678"), dec("1000")), "1000.12345678901235");
    // short results keep their exact value
    assert_eq!(ev(&mut s, BinOp::Mul, dec("125.10"), dec("0.85")), "106.3350");
}
