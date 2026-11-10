//! Puts the NST templates into OUT_DIR for `include_str!`.
//!
//! The templates (system command bars per page type, the Notes and Links system parts) come
//! from traffic captured from a real BC service tier, so they are not in the repository.
//! tools/nst-templates/mktemplates.py writes them into data/nst-templates/ (git ignores
//! data/); BC_NST_TEMPLATES points to another directory. A missing file becomes `{}`, and
//! bc-nst then builds its own command bars and system parts. See docs/nst-protocol-metadata.md.
//!
//! A clone without data/ doesn't watch for the files to appear: after generating them, run
//! `touch crates/bc-nst/build.rs` (or set BC_NST_TEMPLATES) so the next build picks them up.

use std::path::PathBuf;
use std::{env, fs};

const FILES: [&str; 3] = ["sysactions.json", "systemparts/notes.json", "systemparts/recordlinks.json"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BC_NST_TEMPLATES");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dir = env::var_os("BC_NST_TEMPLATES").map(PathBuf::from).unwrap_or_else(|| manifest.join("../../data/nst-templates"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    // Watch only paths that exist: Cargo reruns the script on every build for a missing one.
    if dir.is_dir() {
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    let mut missing = vec![];
    for f in FILES {
        let src = dir.join(f);
        let dst = out.join(f);
        fs::create_dir_all(dst.parent().unwrap()).expect("create OUT_DIR subdirectory");
        let text = match fs::read_to_string(&src) {
            Ok(s) => s,
            Err(_) => {
                missing.push(f);
                "{}\n".to_string()
            }
        };
        fs::write(&dst, text).expect("write template into OUT_DIR");
    }
    if !missing.is_empty() {
        println!(
            "cargo:warning=bc-nst: {} not found in {}; using bc-nst's own command bars and system parts (see tools/nst-templates/mktemplates.py)",
            missing.join(", "),
            dir.display()
        );
    }
}
