use std::path::{Path, PathBuf};

/// Recursively collect `.al` files (sorted for determinism).
pub fn collect_al_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
        Err(_) => return,
    };
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_al_files(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("al")) {
            out.push(p);
        }
    }
}
