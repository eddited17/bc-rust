//! The W1 demo company of Microsoft's legacy demo tool, the company BCApps' CI runs the Base App
//! tests on.
//!
//! BCApps runs every Tests-* app under Layers/W1/Tests as test type "Legacy"
//! (build/scripts/TestConfiguration.json, buckets LegacyTests-Bucket1/2). For that test type
//! build/scripts/ImportTestDataInBcContainer.ps1 (Invoke-DemoDataGeneration, Legacy branch)
//! installs the Base, TestFramework and LocalBaseExtensions groups plus the DemoTool app, creates
//! the company "CRONUS International Ltd." (not an evaluation company), runs codeunit 2
//! "Company-Initialize", stages the tool's resources (src/DemoTool: Pictures, o365 and
//! DemoDataConfig.xml with DataType = Extended) into the session's temp folder, and runs page
//! 101900's action "Create Demo Data from Config", i.e. codeunit 101899. Only then are the other
//! apps installed. Contoso (codeunit 5193) is used for the other test types (UnitTest: no data,
//! IntegrationTest: setup data, Uncategorized: all data in an evaluation company).
//!
//! This module does the same through the interpreter: the resources go into the session's
//! in-memory server files, in its user folder (al_runtime::files::USER_DIR, where CI copies them),
//! then codeunit 101899 runs unmodified. "Path to Picture Folder" stays blank, so the tool opens
//! relative names (resolved against the user folder) and `TemporaryPath() + '..\X'` (the user
//! folder's X, as TemporaryPath() is its TEMP subfolder).

use super::{al_err, table_counts, Counter, CountingConn, DemoReport, NullConn, Phase, COMPANY_INITIALIZE};
use al_model::Catalog;
use al_runtime::Session;
use al_syntax::ObjectKind;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

/// Codeunit 101899 "Create Demo Data from Config" (DemoTool app): reads DemoDataConfig.xml into
/// "Demo Data Setup" and runs codeunit 101900 "Create Demonstration Data".
pub const CREATE_DEMO_DATA_FROM_CONFIG: i64 = 101899;

/// The data type BCApps' CI generates for the Legacy test company (DemoDataSetup.DataType).
pub const DATA_TYPE: &str = "Extended";

/// App directories for a run that creates the legacy W1 demo company: the default apps plus the
/// DemoTool app.
pub fn w1_demo_app_dirs(root: &Path) -> Vec<PathBuf> {
    al_model::app_dirs_with(root, &al_model::w1_demo_tool_dirs(root))
}

/// The DemoTool's resource folder for a catalog that loaded it: src/DemoTool, three levels above
/// the app (src/Layers/W1/DemoTool).
pub fn resources_dir(cat: &Catalog) -> Option<PathBuf> {
    let app = cat.app_dirs.iter().find(|d| d.ends_with("Layers/W1/DemoTool"))?;
    let dir = app.parent()?.parent()?.parent()?.join("DemoTool");
    dir.join("DemoDataConfig.xml").exists().then_some(dir)
}

/// The files BCApps' Initialize-DemoToolResources stages for W1, as (path the AL opens, content):
/// Pictures/* at the top level, the o365 folder as `o365\`, and DemoDataConfig.xml with the given
/// DataType and DataLanguageID 1033.
pub fn resource_files(dir: &Path, data_type: &str) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    fn walk(base: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(base)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name } else { format!("{prefix}\\{name}") };
            if e.file_type()?.is_dir() {
                walk(&e.path(), &rel, out)?;
            } else {
                out.push((rel, std::fs::read(e.path())?));
            }
        }
        Ok(())
    }
    let mut out = vec![];
    walk(&dir.join("Pictures"), "", &mut out)?;
    if dir.join("o365").is_dir() {
        walk(&dir.join("o365"), "o365", &mut out)?;
    }
    let config = std::fs::read_to_string(dir.join("DemoDataConfig.xml"))?;
    out.push(("DemoDataConfig.xml".into(), set_xml_element(&set_xml_element(&config, "DataType", data_type), "DataLanguageID", "1033").into_bytes()));
    Ok(out)
}

/// Replace the text of `<name>...</name>` (or an empty `<name/>`) in a flat XML document.
fn set_xml_element(xml: &str, name: &str, value: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    if let (Some(a), Some(b)) = (xml.find(&open), xml.find(&close)) {
        if a < b {
            return format!("{}{open}{value}{}", &xml[..a], &xml[b..]);
        }
    }
    let empty = format!("<{name}/>");
    if xml.contains(&empty) {
        return xml.replacen(&empty, &format!("{open}{value}{close}"), 1);
    }
    xml.to_string()
}

/// Create the legacy W1 demo company in the session's company the way BCApps' CI does for its
/// Legacy tests: platform rows, codeunit 2 "Company-Initialize" (committed), the tool's resources
/// in the session's server files, then codeunit 101899 "Create Demo Data from Config" (DataType
/// Extended). Commits at the end. The catalog must include `al_model::w1_demo_tool_dirs`.
pub fn create_w1_demo_company(s: &mut Session) -> anyhow::Result<DemoReport> {
    if !s.cat.code_by_key.contains_key(&(ObjectKind::Codeunit, CREATE_DEMO_DATA_FROM_CONFIG)) {
        anyhow::bail!("the DemoTool app is not loaded (load the catalog with demo::w1::w1_demo_app_dirs)");
    }
    let dir = resources_dir(s.cat).ok_or_else(|| anyhow::anyhow!("the DemoTool resources (vendor/bcapps/src/DemoTool) were not found"))?;
    let counter = Rc::new(RefCell::new(Counter::default()));
    let inner = std::mem::replace(&mut s.db, Box::new(NullConn));
    s.db = Box::new(CountingConn { inner, c: counter.clone() });
    let res = run_tool(s, &dir, &counter);
    let (init_ms, tool_ms, warnings) = res?;
    counter.borrow_mut().close();
    let phases: Vec<Phase> = std::mem::take(&mut counter.borrow_mut().phases);
    let tables = table_counts(s.rt, &s.company);
    let total_rows = tables.iter().map(|t| t.rows).sum();
    Ok(DemoReport {
        company: s.company.clone(),
        backend: s.rt.db.describe(),
        init_ms,
        contoso_ms: tool_ms,
        steps: s.steps,
        phases,
        tables,
        total_rows,
        missing: std::mem::take(&mut s.missing),
        warnings,
    })
}

fn run_tool(s: &mut Session, dir: &Path, counter: &Rc<RefCell<Counter>>) -> anyhow::Result<(f64, f64, Vec<String>)> {
    let t0 = Instant::now();
    let mut warnings = vec![];
    s.init_platform_data().map_err(|e| al_err("platform data", e))?;
    s.commit()?;
    counter.borrow_mut().phase = Some(("Company-Initialize".into(), String::new(), Instant::now()));
    match s.run_codeunit(COMPANY_INITIALIZE, None) {
        Ok(()) => s.commit()?,
        Err(e) => {
            // 101899 runs Company-Initialize itself when General Ledger Setup is missing
            s.rollback();
            let msg = al_err("Company-Initialize failed (rolled back, continuing)", e).to_string();
            eprintln!("warning: {msg}");
            warnings.push(msg);
            counter.borrow_mut().rows = 0;
        }
    }
    counter.borrow_mut().close();
    let init_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = Instant::now();
    for (path, bytes) in resource_files(dir, DATA_TYPE)? {
        s.files.write(&path, bytes);
    }
    counter.borrow_mut().phase = Some(("DemoTool".into(), DATA_TYPE.into(), Instant::now()));
    // the tool posts the VAT settlements through Report 20.SaveAsPdf and never reads the PDF
    al_runtime::report::set_skip_rendering(true);
    let r = s.run_codeunit(CREATE_DEMO_DATA_FROM_CONFIG, None);
    al_runtime::report::set_skip_rendering(false);
    r.map_err(|e| al_err("Create Demo Data from Config", e))?;
    counter.borrow_mut().close();
    s.commit()?;
    Ok((init_ms, t1.elapsed().as_secs_f64() * 1000.0, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_element() {
        let x = "<a>\n<DataType>Standard</DataType>\n<AdditionalCurrency/>\n</a>";
        assert_eq!(set_xml_element(x, "DataType", "Extended"), "<a>\n<DataType>Extended</DataType>\n<AdditionalCurrency/>\n</a>");
        assert_eq!(set_xml_element(x, "AdditionalCurrency", "EUR"), "<a>\n<DataType>Standard</DataType>\n<AdditionalCurrency>EUR</AdditionalCurrency>\n</a>");
        assert_eq!(set_xml_element(x, "Missing", "1"), x);
    }
}
