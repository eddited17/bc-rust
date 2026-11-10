//! Refuse, don't guess: the registry of builtin and method arms that return a guessed default
//! instead of doing the work, and the record of what a test relied on.
//!
//! Every arm that answers with a neutral value (true, false, 0, '', nothing) where BC would do
//! real work calls [hit] with its registered name. A refusal ("Not implemented in bc-rust: ...")
//! is an ordinary AL error, so `asserterror`, a [TryFunction] or `if Codeunit.Run()` can swallow
//! it; every such catch goes through `Session::note_error`, which calls [note_caught].
//!
//! al-test reads both after each test ([take]) and classifies a passing test as a *flagged pass*
//! (it touched a registered default) or *not trusted* (a refusal was swallowed on its path).
//! The records are per thread (one test runs on one thread) and bounded: hit counts per
//! registered name, and at most [MAX_SWALLOWED] distinct refusal texts. docs/assumptions.md
//! lists every entry; the tests below keep the registry, the call sites and the doc in step.

use std::cell::RefCell;
use std::collections::BTreeMap;

/// One registered default: its name (`Area.Method`), what bc-rust assumes instead of doing the
/// work, and where the arm is.
pub struct Assumption {
    pub name: &'static str,
    pub assumed: &'static str,
    pub site: &'static str,
}

macro_rules! reg {
    ($($name:literal, $site:literal, $what:literal;)*) => {
        &[$(Assumption { name: $name, assumed: $what, site: $site }),*]
    };
}

/// The registry. Names are unique; [hit] with a name not listed here is a bug (debug assert).
pub const REGISTRY: &[Assumption] = reg![
    // ---- system functions (builtins.rs Session::builtin)
    "System.SerialNumber", "builtins.rs builtin \"serialnumber\" | \"applicationpath\"", "SerialNumber() and ApplicationPath() return ''.";
    "System.ExportObjects", "builtins.rs builtin \"exportobjects\" | \"debugmode\"", "ExportObjects/DebugMode do nothing and return true.";
    "System.HasCollation", "builtins.rs builtin \"hascollation\"", "HasCollation returns false (no collation model).";
    "System.CodeCoverage", "builtins.rs builtin \"codecoverageinclude\" | \"codecoveragelog\" | \"codecoveragerefresh\"", "Code coverage is not recorded; the calls do nothing.";
    // ---- Database / Session / NavApp statics (builtins.rs Session::static_call)
    "Database.AlterKey", "builtins.rs static_call (\"database\", \"alterkey\")", "Database.AlterKey does not enable or disable the key; returns true.";
    "Database.RowVersion", "builtins.rs static_call (\"database\", \"lastusedrowversion\" | \"minimumactiverowversion\")", "LastUsedRowVersion/MinimumActiveRowVersion return 0 (no row versions): delta queries on SystemRowVersion see nothing new.";
    "Session.StartSession", "builtins.rs static_call (\"session\", \"startsession\")", "Without bc serve's background pool (al-test, bc-nst) StartSession returns false and runs nothing.";
    "Session.IsSessionActive", "builtins.rs static_call (\"session\", \"issessionactive\")", "Without the background pool IsSessionActive returns false.";
    "NavApp.IsEntitled", "builtins.rs static_call (\"navapp\", \"isentitled\" | \"isunlicensed\")", "Every app is entitled and licensed: IsEntitled true, IsUnlicensed false.";
    "RequestPage.Static", "builtins.rs static_call (\"requestpage\", _)", "RequestPage.* statics evaluate their arguments and return false.";
    "Xmlport.Run", "builtins.rs static_call (\"xmlport\", \"run\") and xmlport.rs \"run\"", "XMLport.Run (needs a file dialog) does nothing and returns LookupCancel.";
    // ---- values (builtins.rs Session::value_method)
    "RecordRef.Unopened", "builtins.rs value_method unopened RecordRef \"close\" | \"isdirty\" | \"number\"", "An unopened RecordRef answers Close/IsDirty false and Number 0 instead of the BC error.";
    "DotNet.Neutral", "builtins.rs value_method Val::Opaque(\"DotNet\"); dotnet/mod.rs net_call Kind::Net unknown member, net_set_prop", "An unimplemented .NET member returns nothing (or an opaque DotNet value) and property writes are dropped. Recorded per member as DotNet.Neutral:<Type>.<Member> (assume::hit_member).";
    "PlatformObject.Neutral", "builtins.rs value_method Val::Opaque(t) fallback", "A method of a platform object without an implementation returns nothing. Recorded per member as PlatformObject.Neutral:<object>.<Member> (assume::hit_member).";
    // ---- records (recops.rs Session::record_method)
    "Record.IsDirty", "recops.rs record_method \"isdirty\"", "Rec.IsDirty is always false (no per-field dirty tracking).";
    "Record.SecurityFiltering", "recops.rs record_method \"securityfiltering\" | \"setpermissionfilter\"", "SecurityFiltering/SetPermissionFilter are accepted and ignored (no security filters).";
    "Record.Consistent", "recops.rs record_method \"consistent\"", "Rec.Consistent(false) is ignored: Commit does not check transaction consistency.";
    // ---- reports (report.rs)
    "Report.HasLayout", "report.rs report_static \"rdlclayout\" | \"wordlayout\" | \"excellayout\"", "Report.RdlcLayout/WordLayout/ExcelLayout return false (no layout stream handed out).";
    "Report.ValidateAndPrepareLayout", "report.rs report_static \"validateandpreparelayout\"", "Report.ValidateAndPrepareLayout does nothing.";
    "Report.DefaultLayout", "report.rs report_static/report_method \"defaultlayout\"", "Report.DefaultLayout returns RDLC whatever the report declares.";
    "Report.TargetFormat", "report.rs report_method/curr_report \"targetformat\"", "TargetFormat returns option 0 whatever the requested output.";
    "Report.PrintOnlyIfDetail", "report.rs report_method \"printonlyifdetail\" | \"preview\" | \"istraced\"", "Report variable PrintOnlyIfDetail/Preview/IsTraced return false.";
    "Report.OutputDiscarded", "report.rs report_run_finish (Default/Preview/Print output, rendering skipped)", "A report's printed or previewed output is not rendered: only the dataset is processed and the document is discarded.";
    "CurrReport.IsReadOnly", "report.rs curr_report \"isreadonly\" | \"printonlyifdetail\"", "CurrReport.IsReadOnly/PrintOnlyIfDetail return false.";
    "CurrReport.ShowOutput", "report.rs curr_report \"showoutput\"", "CurrReport.ShowOutput returns true (sections always shown).";
    "CurrReport.PageNo", "report.rs curr_report \"pageno\"", "CurrReport.PageNo returns 1 (no pagination).";
    "CurrReport.TotalsCausedBy", "report.rs curr_report \"totalscausedby\"", "CurrReport.TotalsCausedBy returns 0.";
    "CurrReport.SetTableView", "report.rs curr_report \"settableview\" | \"setselectionfilter\"", "CurrReport.SetTableView/SetSelectionFilter are ignored.";
    "CurrReport.Unknown", "report.rs curr_report fallback", "An unknown CurrReport method returns nothing.";
    // ---- report layouts saved as Excel (rdlc_numfmt.rs, rdlc_xlsx.rs): open points of the
    // Format conversion until probes on a real BC settle them
    "U2.CultureData", "rdlc_numfmt.rs culture", "A named culture's number format data (decimal digits, negative sign, currency and percent patterns) is taken to be the invariant culture's (O-3).";
    "U2.NegativeForm", "rdlc_numfmt.rs record (NEG_FORM)", "For a format class probe P1 does not cover, a negative whose display is the negative sign before the positive display is written as an explicit second section, pos;-pos (O-19).";
    "U2.OpenFormat", "rdlc_numfmt.rs record (Open)", "A Format string the converter does not write a code for gives the cell the General format: one the public sources leave open (padded strings, a single non-ASCII letter, bad quotes, empty or extra sections, several %, per mille, odd commas or exponents; BC writes a code for most of them), one where BC writes a #VALUE! cell (X on a Decimal, B, invalid letters), a value-dependent code (G with a precision) or a hex text cell (X on an Integer), or one over 1,024 characters; the member names the item (O-1, O-7, O-9 to O-13, length).";
    "U2.UserLanguage", "rdlc_xlsx.rs cell_format", "A textbox without any Language (none on its TextRun, Textbox or Report) is formatted in the report's FormatRegion (O-2).";
    // ---- DateFormula (datefml.rs): points still open after the U1 probes (T4)
    "DateFormula.P09", "datefml.rs Probe::P09 (note_assumed)", "A date formula holding U+0000..U+0007 or a decimal digit of another script (T4:P09: those inputs are not settled): U1 unsettled: P09.";
    "DateFormula.P10", "datefml.rs Probe::P10 (note_assumed)", "A date formula text longer than 32 characters that also fails to read, or that holds lower case (the case of %1 is not measured): refused, message U1 unsettled: P10.";
    "DateFormula.P11", "datefml.rs Probe::P11 (note_assumed)", "Format(DateFormula) with format 0 or 1 in Vietnamese (C and D letters not measured) or a language id outside BC's list: U1 unsettled: P11.";
    "DateFormula.P12", "datefml.rs Probe::P12 (note_assumed)", "Evaluate of unbracketed date formula text in Vietnamese or a language id outside BC's list: U1 unsettled: P12.";
    "DateFormula.P13", "datefml.rs Probe::P13 (note_assumed)", "Date formula error texts outside English (the probes saw English in one German session, German in another): U1 unsettled: P13; Evaluate's return form still gives false.";
    "DateFormula.P14", "datefml.rs Probe::P14 (note_assumed)", "The order of two different DateFormula values (BC orders SQL and temporary tables differently): U1 unsettled: P14 for < > in AL; value::compare orders by stored text (flagged).";
    "DateFormula.P15", "datefml.rs Probe::P15 (note_assumed)", "Format(DateFormula, Length, n) with a length and a format number other than 0, or with a format string: U1 unsettled: P15.";
    "DateFormula.P17", "datefml.rs Probe::P17 (note_assumed)", "CalcDate(Text) with unbracketed text in Vietnamese or a language id outside BC's list: U1 unsettled: P17.";
    // ---- SessionInformation (dotnet/mod.rs net_static)
    "SessionInformation.Counters", "dotnet/mod.rs net_static SessionInformation \"sqlrowsread\" | \"sqlstatementsexecuted\" | \"aitokensused\" and others", "SessionInformation counters return 0; other members return nothing.";
];

/// Entries that are recorded but do not make a pass a flagged pass: they skip a check rather
/// than guess a value, and nearly every posting touches them (W1 quick slice, 2026-11-09:
/// SecurityFiltering in 6,860 and Consistent in 3,720 of 21,979 passing tests), so as flags they
/// would hide the guesses that matter. Security filters only act with permissions enforced; a
/// Consistent(false) left set at Commit would be a posting bug that the ledger assertions see.
pub const NOT_FLAGGING: &[&str] = &["Record.SecurityFiltering", "Record.Consistent"];

/// Whether touching `name` makes a pass a flagged pass. A member entry (`Name:Member`, see
/// [hit_member]) never flags by itself: its registered name is recorded with it.
pub fn flags(name: &str) -> bool {
    !NOT_FLAGGING.contains(&name) && !name.contains(':')
}

/// At most this many distinct swallowed refusal texts are kept per test.
pub const MAX_SWALLOWED: usize = 8;

/// The start of a message that means bc-rust could not run the code (a refusal or an
/// interpreter gap), as opposed to an error the AL code raised. Same family as al-test's
/// RUNTIME_GAPS, without the limits (step limit, call depth) and BC errors it also lists.
const REFUSALS: &[&str] = &[
    "Not implemented in bc-rust",
    "Unknown variable",
    "Method '",
    "Cannot call expression",
    "Cannot index",
    "Cannot resolve",
    "Record method",
    "'in' requires",
    "Expected a field reference",
    // an unsettled DateFormula point (crate::datefml, spec section 4)
    "U1 unsettled",
];

#[derive(Default)]
struct Touched {
    hits: BTreeMap<&'static str, u32>,
    swallowed: Vec<String>,
    swallowed_n: u32,
}

thread_local! {
    static TOUCHED: RefCell<Touched> = RefCell::new(Touched::default());
}

/// The registry entry of `name`.
pub fn lookup(name: &str) -> Option<&'static Assumption> {
    REGISTRY.iter().find(|a| a.name == name)
}

/// A registered default answered instead of doing the work.
pub fn hit(name: &'static str) {
    debug_assert!(lookup(name).is_some(), "assume::hit: {name} is not in assume::REGISTRY");
    TOUCHED.with(|t| *t.borrow_mut().hits.entry(name).or_default() += 1);
}

/// A registered default answered for one member of a family (`DotNet.Neutral` for
/// `NavUserAccountHelper.IsWindowsAuthentication`): records `name` as [hit] does, plus the entry
/// `name:member`, so al-test's `assumed` field and summary split the family by member. Member
/// entries are interned (one leaked string per distinct entry, process-wide).
pub fn hit_member(name: &'static str, member: &str) {
    hit(name);
    let entry = intern_member(name, member);
    TOUCHED.with(|t| *t.borrow_mut().hits.entry(entry).or_default() += 1);
}

fn intern_member(name: &str, member: &str) -> &'static str {
    static NAMES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let key = format!("{name}:{member}");
    let mut v = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = v.iter().find(|s| **s == key) {
        return s;
    }
    let s: &'static str = Box::leak(key.into_boxed_str());
    v.push(s);
    s
}

/// Whether an error message is a refusal (bc-rust could not run the code).
pub fn is_refusal(msg: &str) -> bool {
    REFUSALS.iter().any(|p| msg.starts_with(p))
}

/// An error was caught by AL code (asserterror, TryFunction, `if Codeunit.Run()`, ...): a
/// refusal caught this way is a swallowed refusal.
pub fn note_caught(msg: &str) {
    if !is_refusal(msg) {
        return;
    }
    let line = msg.lines().next().unwrap_or("").chars().take(160).collect::<String>();
    TOUCHED.with(|t| {
        let mut t = t.borrow_mut();
        t.swallowed_n += 1;
        if t.swallowed.len() < MAX_SWALLOWED && !t.swallowed.contains(&line) {
            t.swallowed.push(line);
        }
    });
}

/// What the current thread touched since the last [take]/[reset].
#[derive(Default, Debug, Clone)]
pub struct Record {
    /// Registered defaults touched, by name, with hit counts.
    pub assumed: Vec<(&'static str, u32)>,
    /// Distinct refusals swallowed (first line, at most [MAX_SWALLOWED]).
    pub swallowed: Vec<String>,
    /// All swallowed refusals, counted.
    pub swallowed_n: u32,
}

impl Record {
    /// Whether a registered default that flags a pass was touched ([flags]).
    pub fn flagging(&self) -> bool {
        self.assumed.iter().any(|(n, _)| flags(n))
    }
}

/// Take (and clear) what the current thread touched.
pub fn take() -> Record {
    TOUCHED.with(|t| {
        let t = std::mem::take(&mut *t.borrow_mut());
        Record { assumed: t.hits.into_iter().collect(), swallowed: t.swallowed, swallowed_n: t.swallowed_n }
    })
}

/// Forget what the current thread touched (before a test starts).
pub fn reset() {
    let _ = take();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                sources(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("assume.rs") {
                out.push((p.display().to_string(), std::fs::read_to_string(&p).unwrap()));
            }
        }
    }

    /// The direct string literals passed to `assume::hit("...")`.
    fn direct_hits(files: &[(String, String)]) -> Vec<(String, String)> {
        let mut out = vec![];
        for (path, text) in files {
            for part in text.split("assume::hit(\"").skip(1) {
                out.push((part[..part.find('"').unwrap_or(0)].to_string(), path.clone()));
            }
        }
        out
    }

    #[test]
    fn registry_names_unique_and_used() {
        let mut names: Vec<&str> = REGISTRY.iter().map(|a| a.name).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len(), "duplicate names in assume::REGISTRY");
        let mut files = vec![];
        sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        for (name, path) in direct_hits(&files) {
            assert!(lookup(&name).is_some(), "{path}: assume::hit(\"{name}\") is not registered in assume::REGISTRY");
        }
        // every entry has a call site (a `hit(if .. { "A" } else { "B" })` names it as a literal)
        for a in REGISTRY {
            let lit = format!("\"{}\"", a.name);
            assert!(files.iter().any(|(_, t)| t.contains(&lit) && t.contains("assume::hit(")), "assume::REGISTRY entry {} has no assume::hit call site", a.name);
        }
    }

    #[test]
    fn doc_lists_every_entry() {
        let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/assumptions.md");
        let Ok(text) = std::fs::read_to_string(&doc) else {
            panic!("{} is missing", doc.display());
        };
        for a in REGISTRY {
            assert!(text.contains(&format!("`{}`", a.name)), "docs/assumptions.md has no row for `{}`", a.name);
        }
    }

    #[test]
    fn take_counts_hits_and_swallowed_refusals() {
        reset();
        hit("Record.IsDirty");
        hit("Record.IsDirty");
        note_caught("The field must have a value."); // an AL error, not a refusal
        note_caught("Not implemented in bc-rust: Query.Foo\nmore");
        note_caught("Not implemented in bc-rust: Query.Foo");
        let r = take();
        assert_eq!(r.assumed, vec![("Record.IsDirty", 2)]);
        assert_eq!(r.swallowed, vec!["Not implemented in bc-rust: Query.Foo".to_string()]);
        assert_eq!(r.swallowed_n, 2);
        assert!(r.flagging());
        assert!(take().assumed.is_empty());
        hit("Record.Consistent");
        let r = take();
        assert_eq!(r.assumed, vec![("Record.Consistent", 1)]);
        assert!(!r.flagging());
        hit_member("DotNet.Neutral", "Uri.Foo");
        hit_member("DotNet.Neutral", "Uri.Foo");
        let r = take();
        assert_eq!(r.assumed, vec![("DotNet.Neutral", 2), ("DotNet.Neutral:Uri.Foo", 2)]);
        assert!(r.flagging());
        assert!(!flags("DotNet.Neutral:Uri.Foo"));
        assert!(NOT_FLAGGING.iter().all(|n| lookup(n).is_some()));
    }
}
