//! Q7 of the U1 spec: CalcDate cost per call, cold (read each time) and through the session
//! cache, for the formulas BaseApp posting uses most. std only (no criterion in the workspace):
//! `cargo bench -p al-runtime --bench datefml`.
use al_runtime::datefml::{self, Cache, ENGLISH};
use std::hint::black_box;
use std::time::Instant;

const FORMULAS: &[&str] = &["1M", "CM", "-CM", "1D", "-1D", "CY", "14D", "30D", "1M+CM", "0D"];
const N: u32 = 2_000_000;

fn per_call(f: impl Fn(u32) -> i32) -> f64 {
    // warm up, then the best of 3 runs
    for i in 0..N / 10 {
        black_box(f(i));
    }
    (0..3)
        .map(|_| {
            let t = Instant::now();
            for i in 0..N {
                black_box(f(i));
            }
            t.elapsed().as_nanos() as f64 / f64::from(N)
        })
        .fold(f64::MAX, f64::min)
}

fn main() {
    let base = datefml::date_to_day(2024, 1, 15).unwrap() as i32 * 2;
    println!("{:<8} {:>12} {:>12} {:>12} {:>12}", "formula", "cold ns", "cached ns", "text ns", "apply ns");
    for f in FORMULAS {
        let bracketed = format!("<{f}>");
        // cold: read the stored text, then apply
        let cold = per_call(|i| datefml::parse_invariant(black_box(f)).unwrap().calc(base + 2 * (i % 512) as i32, ENGLISH).unwrap());
        // the session cache: CalcDate(DateFormula, Date)
        let mut c = Cache::new();
        let c = std::cell::RefCell::new(&mut c);
        let cached = per_call(|i| c.borrow_mut().calc_stored(black_box(f), ENGLISH, base + 2 * (i % 512) as i32).unwrap());
        // CalcDate('<...>', Date) through the cache
        let mut c2 = Cache::new();
        let c2 = std::cell::RefCell::new(&mut c2);
        let text = per_call(|i| c2.borrow_mut().calc_text(black_box(&bracketed), ENGLISH, base + 2 * (i % 512) as i32).unwrap());
        // applying a parsed formula alone
        let parsed = datefml::parse_invariant(f).unwrap();
        let apply = per_call(|i| black_box(&parsed).calc(base + 2 * (i % 512) as i32, ENGLISH).unwrap());
        println!("{f:<8} {cold:>12.1} {cached:>12.1} {text:>12.1} {apply:>12.1}");
    }
}
