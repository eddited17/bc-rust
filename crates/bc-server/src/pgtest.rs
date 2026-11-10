//! `al-test --pg CONN`: run the tests on PostgreSQL, the backend production runs, instead of the
//! embedded in-memory store (docs/pg-vs-memory.md).
//!
//! The demo company is built (or loaded from the demo cache) in memory as usual, then copied once
//! into a template database `<prefix>_tpl`. Every worker gets its own database `<prefix>_w<N>`,
//! cloned from the template with `CREATE DATABASE ... TEMPLATE` (a file copy, about a second).
//! The per-test and per-codeunit isolation is the same as in memory: a test's Commit() is a
//! savepoint, and the codeunit (or test) ends with a ROLLBACK of the worker's one transaction;
//! number sequences, which no rollback undoes, are put back from the snapshot as in memory.
//!
//! `<prefix>` is the `dbname` of CONN. It must not be a live database (`bc`, `bc_nst`): every
//! database this module creates carries the comment `al-test scratch ...`, and it drops or
//! replaces only databases with that comment.

use crate::demo::{self, SeqState};
use al_model::Catalog;
use al_runtime::Runtime;
use bc_store::{Backend, MemBackend, PgBackend, ScanReq};
use postgres::{Client, NoTls};
use std::sync::{Arc, Mutex};

const MARK: &str = "al-test scratch";

/// The scratch databases of one `--pg` run.
pub struct PgScratch {
    /// CONN with `dbname=` replaced by `{db}`.
    pattern: String,
    prefix: String,
    /// Worker databases this run created (dropped by `finish`).
    created: Mutex<Vec<String>>,
}

fn replace_dbname(conn: &str, db: &str) -> String {
    let mut out: Vec<String> = conn.split_whitespace().filter(|p| !p.starts_with("dbname=")).map(str::to_string).collect();
    out.push(format!("dbname={db}"));
    out.join(" ")
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

impl PgScratch {
    /// Check CONN and connect to its server. The `dbname` of CONN is the name prefix.
    pub fn new(conn: &str) -> anyhow::Result<PgScratch> {
        let prefix = conn.split_whitespace().find_map(|p| p.strip_prefix("dbname=")).unwrap_or("").to_string();
        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            anyhow::bail!("--pg: give dbname=<prefix> (lower-case letters, digits, _): the scratch databases are <prefix>_tpl and <prefix>_w<N>");
        }
        if matches!(prefix.as_str(), "bc" | "bc_nst" | "postgres" | "template0" | "template1") {
            anyhow::bail!("--pg: dbname={prefix} is a live or system database; use a scratch prefix");
        }
        let s = PgScratch { pattern: conn.to_string(), prefix, created: Mutex::new(vec![]) };
        s.admin()?;
        Ok(s)
    }

    fn url(&self, db: &str) -> String {
        replace_dbname(&self.pattern, db)
    }

    /// A connection to the server's `postgres` database (CREATE / DROP DATABASE).
    fn admin(&self) -> anyhow::Result<Client> {
        Client::connect(&self.url("postgres"), NoTls).map_err(|e| anyhow::anyhow!("--pg: connect: {e}"))
    }

    pub fn template_name(&self) -> String {
        format!("{}_tpl", self.prefix)
    }

    /// The comment of database `db` (None: no such database).
    fn comment(a: &mut Client, db: &str) -> anyhow::Result<Option<String>> {
        let r = a.query_opt("SELECT coalesce(shobj_description(oid, 'pg_database'), '') FROM pg_database WHERE datname = $1", &[&db])?;
        Ok(r.map(|r| r.get::<_, String>(0)))
    }

    /// Drop `db` if it exists and is one of ours (refuse otherwise).
    fn drop_ours(a: &mut Client, db: &str) -> anyhow::Result<()> {
        match Self::comment(a, db)? {
            None => Ok(()),
            Some(c) if c.starts_with(MARK) => {
                a.batch_execute(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", quote_ident(db)))?;
                Ok(())
            }
            Some(_) => anyhow::bail!("--pg: database {db} exists and was not made by al-test; refusing to replace it"),
        }
    }

    /// `CREATE DATABASE db [TEMPLATE tpl]`, retried while the template still has connections
    /// (they close asynchronously).
    fn create(a: &mut Client, db: &str, tpl: Option<&str>, comment: &str) -> anyhow::Result<()> {
        let sql = match tpl {
            Some(t) => format!("CREATE DATABASE {} TEMPLATE {} STRATEGY FILE_COPY", quote_ident(db), quote_ident(t)),
            None => format!("CREATE DATABASE {}", quote_ident(db)),
        };
        let mut tries = 0;
        loop {
            match a.batch_execute(&sql) {
                Ok(()) => break,
                Err(e) if tries < 50 && e.as_db_error().is_some_and(|d| d.message().contains("is being accessed by other users")) => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => anyhow::bail!("--pg: {sql}: {}", e.as_db_error().map(|d| d.message().to_string()).unwrap_or(e.to_string())),
            }
        }
        a.batch_execute(&format!("COMMENT ON DATABASE {} IS '{}'", quote_ident(db), comment.replace('\'', "''")))?;
        Ok(())
    }

    /// The template database with the demo company `master` (company CRONUS plus the shared
    /// tables) and its number sequences. `key` identifies the build and the company (the demo
    /// cache key): a template with the same key is reused, any other is replaced. Returns a line
    /// for the summary.
    pub fn ensure_template(&self, cat: &'static Catalog, master: &MemBackend, seqs: &[SeqState], key: &str) -> anyhow::Result<String> {
        let t0 = std::time::Instant::now();
        let tpl = self.template_name();
        let want = format!("{MARK} template {key}");
        let mut a = self.admin()?;
        if Self::comment(&mut a, &tpl)?.as_deref() == Some(want.as_str()) {
            return Ok(format!("PostgreSQL template {tpl} reused"));
        }
        Self::drop_ours(&mut a, &tpl)?;
        // built under a temporary comment: a run that dies half-way leaves no reusable template
        Self::create(&mut a, &tpl, None, &format!("{MARK} template (incomplete)"))?;
        let rows = {
            let be = PgBackend::connect_url(&self.url(&tpl)).map_err(|e| anyhow::anyhow!("--pg: {e}"))?;
            let rt = Runtime::new(cat, Arc::new(be));
            // every stored table up front, so workers never run DDL during a test
            let schemas: Vec<_> = rt
                .schemas
                .iter()
                .filter(|(id, _)| !al_runtime::virt::is_catalog_table(**id) && !cat.tables[*id].temporary_only)
                .map(|(_, s)| s.clone())
                .collect();
            rt.db.sync_schema("CRONUS", &schemas).map_err(|e| anyhow::anyhow!("--pg: schema: {e}"))?;
            let mut from = master.connect().map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut to = rt.db.connect().map_err(|e| anyhow::anyhow!("--pg: {e}"))?;
            let req = ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
            // also the stored rows of the platform's computed tables (the Feature Key states the
            // company build set: al-runtime feature_key_rows)
            let computed: Vec<_> = rt
                .schemas
                .iter()
                .filter(|(id, _)| al_runtime::virt::is_catalog_table(**id) && !cat.tables[*id].temporary_only)
                .map(|(_, s)| s.clone())
                .collect();
            let mut n = 0usize;
            for s in schemas.iter().chain(&computed) {
                for row in from.scan("CRONUS", s, &req).map_err(|e| anyhow::anyhow!("{e}"))? {
                    to.insert("CRONUS", s, &row).map_err(|e| anyhow::anyhow!("--pg: copy {} ({}): {e}", s.name, s.id))?;
                    n += 1;
                }
            }
            to.commit().map_err(|e| anyhow::anyhow!("--pg: commit: {e}"))?;
            demo::restore_sequences(to.as_mut(), seqs)?;
            to.commit().map_err(|e| anyhow::anyhow!("--pg: commit: {e}"))?;
            n
        };
        a.batch_execute(&format!("COMMENT ON DATABASE {} IS '{}'", quote_ident(&tpl), want.replace('\'', "''")))?;
        Ok(format!("PostgreSQL template {tpl} ({rows} rows copied in {:.1} s)", t0.elapsed().as_secs_f64()))
    }

    /// Worker `me`'s database, a fresh clone of the template; returns its backend.
    pub fn worker_backend(&self, me: usize) -> anyhow::Result<PgBackend> {
        let db = format!("{}_w{me}", self.prefix);
        let mut a = self.admin()?;
        Self::drop_ours(&mut a, &db)?;
        Self::create(&mut a, &db, Some(&self.template_name()), &format!("{MARK} worker"))?;
        self.created.lock().unwrap().push(db.clone());
        PgBackend::connect_url(&self.url(&db)).map_err(|e| anyhow::anyhow!("--pg: {db}: {e}"))
    }

    /// Drop the worker databases this run created (the template stays for the next run of the
    /// same build). `keep`: leave them for a look (`--pg-keep`).
    pub fn finish(&self, keep: bool) {
        let dbs = std::mem::take(&mut *self.created.lock().unwrap());
        if keep || dbs.is_empty() {
            return;
        }
        if let Ok(mut a) = self.admin() {
            for db in dbs {
                if let Err(e) = Self::drop_ours(&mut a, &db) {
                    eprintln!("--pg: could not drop {db}: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbname_is_replaced() {
        assert_eq!(replace_dbname("host=/s port=5432 user=bc dbname=s3a password=x", "s3a_w0"), "host=/s port=5432 user=bc password=x dbname=s3a_w0");
        assert_eq!(replace_dbname("host=h", "p"), "host=h dbname=p");
    }

    #[test]
    fn live_databases_are_refused() {
        for c in ["host=/nowhere dbname=bc", "host=/nowhere dbname=bc_nst", "host=/nowhere", "host=/nowhere dbname=Bad-Name"] {
            assert!(PgScratch::new(c).is_err(), "{c}");
        }
    }
}
