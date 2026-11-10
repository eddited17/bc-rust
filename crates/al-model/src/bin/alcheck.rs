//! Parse every .al file under the given directories and report failures.
use rayon::prelude::*;
use std::collections::HashSet;
use std::time::Instant;

fn main() {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    let mut files = Vec::new();
    for d in &dirs {
        al_model::loader::collect_al_files(std::path::Path::new(d), &mut files);
    }
    let t0 = Instant::now();
    // BC_AL_SYMBOLS=A,B: preprocessor symbols, as for Catalog::load
    let syms: HashSet<String> = std::env::var("BC_AL_SYMBOLS").map(|v| v.split(',').map(|x| x.trim().to_ascii_uppercase()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    let results: Vec<_> = files
        .par_iter()
        .map(|f| {
            let src = std::fs::read_to_string(f).unwrap_or_default();
            let lines = src.lines().count();
            (f, lines, al_syntax::parse_file(&src, &syms))
        })
        .collect();
    let el = t0.elapsed();
    let mut ok = 0;
    let mut lines = 0;
    let mut objects = 0;
    let mut fails = Vec::new();
    for (f, l, r) in &results {
        lines += l;
        match r {
            Ok(sf) => {
                ok += 1;
                objects += sf.objects.len();
            }
            Err(e) => fails.push(format!("{}:{}", f.display(), e)),
        }
    }
    fails.sort();
    let max: usize = std::env::var("ALCHECK_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    for f in fails.iter().take(max) {
        println!("FAIL {f}");
    }
    println!(
        "files={} ok={} failed={} objects={} lines={} time={:.2?}",
        results.len(),
        ok,
        fails.len(),
        objects,
        lines,
        el
    );
}
