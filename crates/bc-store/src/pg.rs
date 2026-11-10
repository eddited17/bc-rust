//! PostgreSQL backend: the system of record.
//!
//! Layout: one schema per company (`c_<name>`, plus `shared` for tables with
//! DataPerCompany = false), one table per BC table (`t<id>`), one column per
//! field (`f<id>`). Text and Code columns use a case-insensitive ICU collation
//! (`ci`), like BC's SQL collation. Dates follow BC's SQL convention: closing
//! dates are 23:59:59, 0D/0DT are 1753-01-01.

use crate::backend::*;
use crate::filter::{FNode, FieldFilter};
use crate::table::{StoreError, TableSchema};
use crate::value::*;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use parking_lot::Mutex;
use postgres::types::ToSql;
use postgres::{Client, NoTls, Statement};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

type P = Box<dyn ToSql + Sync + Send>;

struct Shared {
    url: String,
    /// Idle connections with their prepared statements (kept across sessions:
    /// preparing again costs a round trip plus fresh custom planning per statement).
    pool: Mutex<Vec<(Client, FastMap<String, Statement>)>>,
    admin: Mutex<Client>,
    ready: Mutex<HashSet<(String, i64)>>,
    queries: std::sync::atomic::AtomicU64,
    /// Tables some session has seen with rows. Only a hint for `probe_empty`
    /// (skip the emptiness check for tables that are unlikely to be empty);
    /// never used to answer a read.
    nonempty: Mutex<HashSet<(String, i64)>>,
    /// Commit counters per table (hashed into `GEN_BUCKETS`; a collision only drops more): a
    /// transaction that wrote a table bumps it when it commits, and every session's cached reads
    /// of the table taken before that are dropped (`PgConn::cache`).
    gens: Vec<std::sync::atomic::AtomicU64>,
}

const GEN_BUCKETS: usize = 4_096;

fn gen_bucket(ck: &TKey) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = FastHash::default();
    ck.hash(&mut h);
    (h.finish() % GEN_BUCKETS as u64) as usize
}

impl Shared {
    /// A connection was found dead: the idle ones were opened before the same server restart, so
    /// they are dropped instead of failing one request each (I14: 7 failed sign-ins on live
    /// after a PostgreSQL restart).
    fn flush_pool(&self) {
        let idle = std::mem::take(&mut *self.pool.lock());
        if !idle.is_empty() {
            eprintln!("PostgreSQL connection lost: dropping {} idle pooled connections", idle.len());
        }
        drop(idle);
    }
    /// Run `f` on the admin connection; when the connection is gone, reconnect it and run `f`
    /// once more (admin statements are catalog reads and DDL in their own transactions).
    fn admin_retry<T>(&self, admin: &mut Client, mut f: impl FnMut(&mut Client) -> Result<T, postgres::Error>) -> Result<T, StoreError> {
        match f(admin) {
            Err(e) if conn_lost(&e) => {
                self.flush_pool();
                *admin = Client::connect(&self.url, NoTls).map_err(db)?;
                eprintln!("PostgreSQL admin connection lost ({e}); reconnected");
                f(admin).map_err(db)
            }
            r => r.map_err(db),
        }
    }
    fn gen(&self, ck: &TKey) -> u64 {
        self.gens[gen_bucket(ck)].load(std::sync::atomic::Ordering::Acquire)
    }
    fn bump(&self, ck: &TKey) {
        self.gens[gen_bucket(ck)].fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

pub struct PgBackend {
    sh: Arc<Shared>,
}

fn db(e: postgres::Error) -> StoreError {
    // a lock wait past lock_timeout and a deadlock: errors for the table this thread used last
    // (docs/job-queue.md), in bc-rust's own words [src:X22]
    if let Some(d) = e.as_db_error() {
        if *d.code() == postgres::error::SqlState::LOCK_NOT_AVAILABLE {
            let t = LAST_TABLE.with(|t| t.borrow().clone());
            return StoreError::Other(format!("Could not save: a record in table '{t}' is locked by another session's transaction. Try again when that transaction has finished."));
        }
        if *d.code() == postgres::error::SqlState::T_R_DEADLOCK_DETECTED {
            let t = LAST_TABLE.with(|t| t.borrow().clone());
            return StoreError::Other(format!("This transaction deadlocked with another session on table {t} and was rolled back. Try again."));
        }
    }
    StoreError::Other(format!("database: {}", e.as_db_error().map(|d| d.message().to_string()).unwrap_or_else(|| e.to_string())))
}

/// Whether `e` means the connection itself is gone (PostgreSQL restarted, the backend was
/// terminated, the socket broke), not that a statement failed: the client can't be used again.
fn conn_lost(e: &postgres::Error) -> bool {
    if e.is_closed() {
        return true;
    }
    if let Some(c) = e.code() {
        // 08: connection exception; 57P01-57P03: admin/crash shutdown, cannot connect now
        return c.code().starts_with("08") || c.code().starts_with("57P");
    }
    e.as_db_error().is_none() && std::error::Error::source(e).is_some_and(|s| s.is::<std::io::Error>())
}

thread_local! {
    /// The table of the last statement on this thread (BC's lock-timeout and deadlock texts).
    static LAST_TABLE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn note_table(schema: &TableSchema) {
    LAST_TABLE.with(|t| {
        if *t.borrow() != schema.name {
            *t.borrow_mut() = schema.name.clone();
        }
    });
}

/// lock_timeout of every session connection in ms (BC's LockTimeoutDuration default, 10 s);
/// BC_PG_LOCK_TIMEOUT_MS overrides, 0 waits without limit.
fn lock_timeout_ms() -> u64 {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MS.get_or_init(|| std::env::var("BC_PG_LOCK_TIMEOUT_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(10_000))
}

/// LockTimeoutDuration() of a session that hasn't set it: the server's lock timeout in seconds, as BC's
/// NavSqlConnection.GetDefaultLockTimeout (the database's LockTimeout setting), -1 when it is off.
pub fn default_lock_timeout_secs() -> i64 {
    match lock_timeout_ms() {
        0 => -1,
        ms => ms.div_ceil(1000) as i64,
    }
}

/// PostgreSQL lock_timeout (ms) for a session lock timeout in seconds; -1 (no limit) is 0.
fn lock_timeout_ms_of(secs: i64) -> i64 {
    if secs < 0 {
        0
    } else {
        secs * 1000
    }
}

fn schema_name(company: &str, per_company: bool) -> String {
    if !per_company {
        return "shared".into();
    }
    let s: String = company.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    format!("c_{s}")
}

fn col_sql(t: ColType) -> (&'static str, &'static str) {
    match t {
        ColType::Int | ColType::Option => ("integer", "0"),
        ColType::BigInt | ColType::Duration => ("bigint", "0"),
        ColType::Dec => ("numeric", "0"),
        ColType::Bool => ("boolean", "false"),
        ColType::Code | ColType::Text => ("text COLLATE ci", "''"),
        ColType::Date | ColType::Time | ColType::DateTime => ("timestamp", "'1753-01-01 00:00:00'"),
        ColType::Guid => ("uuid", "'00000000-0000-0000-0000-000000000000'"),
        ColType::Blob => ("bytea", "''::bytea"),
    }
}

fn undefined_ts() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(1753, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

fn to_param(t: ColType, v: &Value) -> P {
    match (t, v) {
        (_, Value::Int(i)) | (_, Value::Option(i)) if !matches!(t, ColType::Date | ColType::Time) => Box::new(*i),
        (_, Value::BigInt(i)) | (_, Value::Duration(i)) => Box::new(*i),
        (_, Value::Dec(d)) => Box::new(*d),
        (_, Value::Bool(b)) => Box::new(*b),
        (_, Value::Text(s)) => Box::new(s.clone()),
        (_, Value::Date(d)) => Box::new(date_to_ts(*d)),
        (_, Value::Time(t)) => Box::new(time_to_ts(*t)),
        (_, Value::DateTime(ms)) => Box::new(dt_to_ts(*ms)),
        (_, Value::Guid(g)) => Box::new(uuid::Uuid::from_bytes(*g)),
        (_, Value::Blob(b)) => Box::new(b.clone()),
        (_, Value::Int(i)) | (_, Value::Option(i)) => Box::new(*i),
    }
}

fn date_to_ts(d: i32) -> NaiveDateTime {
    if d <= 0 {
        return undefined_ts();
    }
    let n = NaiveDate::from_num_days_from_ce_opt(d / 2).unwrap_or(NaiveDate::from_ymd_opt(1753, 1, 1).unwrap());
    if d % 2 == 1 {
        n.and_hms_opt(23, 59, 59).unwrap()
    } else {
        n.and_hms_opt(0, 0, 0).unwrap()
    }
}

fn ts_to_date(t: NaiveDateTime) -> i32 {
    if t == undefined_ts() {
        return 0;
    }
    let closing = t.time() == NaiveTime::from_hms_opt(23, 59, 59).unwrap();
    t.date().num_days_from_ce() * 2 + closing as i32
}

fn time_to_ts(t: i32) -> NaiveDateTime {
    if t <= 0 {
        return undefined_ts();
    }
    let base = NaiveDate::from_ymd_opt(1754, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
    base + chrono::Duration::milliseconds((t - 1) as i64)
}

fn ts_to_time(t: NaiveDateTime) -> i32 {
    if t == undefined_ts() {
        return 0;
    }
    (t.time().num_seconds_from_midnight() as i64 * 1000 + (t.time().nanosecond() / 1_000_000) as i64) as i32 + 1
}

fn dt_to_ts(ms: i64) -> NaiveDateTime {
    if ms == DATETIME_UNDEFINED {
        return undefined_ts();
    }
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.naive_utc()).unwrap_or(undefined_ts())
}

fn ts_to_dt(t: NaiveDateTime) -> i64 {
    if t == undefined_ts() {
        return DATETIME_UNDEFINED;
    }
    t.and_utc().timestamp_millis()
}

fn read_row(schema: &TableSchema, r: &postgres::Row) -> Result<Row, StoreError> {
    read_row_at(schema, r, 0)
}

/// Decode a row whose columns start at result column `off`.
fn read_row_at(schema: &TableSchema, r: &postgres::Row, off: usize) -> Result<Row, StoreError> {
    schema.columns.iter().enumerate().map(|(i, t)| read_value(*t, r, i + off)).collect()
}

fn read_value(t: ColType, r: &postgres::Row, i: usize) -> Result<Value, StoreError> {
    Ok(match t {
        ColType::Int => Value::Int(r.try_get(i).map_err(db)?),
        ColType::Option => Value::Option(r.try_get(i).map_err(db)?),
        ColType::BigInt => Value::BigInt(r.try_get(i).map_err(db)?),
        ColType::Duration => Value::Duration(r.try_get(i).map_err(db)?),
        ColType::Dec => Value::Dec(r.try_get::<_, Decimal>(i).map_err(db)?),
        ColType::Bool => Value::Bool(r.try_get(i).map_err(db)?),
        ColType::Code | ColType::Text => Value::Text(r.try_get(i).map_err(db)?),
        ColType::Date => Value::Date(ts_to_date(r.try_get(i).map_err(db)?)),
        ColType::Time => Value::Time(ts_to_time(r.try_get(i).map_err(db)?)),
        ColType::DateTime => Value::DateTime(ts_to_dt(r.try_get(i).map_err(db)?)),
        ColType::Guid => Value::Guid(*r.try_get::<_, uuid::Uuid>(i).map_err(db)?.as_bytes()),
        ColType::Blob => Value::Blob(r.try_get(i).map_err(db)?),
    })
}

/// Advisory lock key ("bcrustdd") that serializes bc-rust's schema DDL within one database.
/// `CREATE ... IF NOT EXISTS` is not safe against a concurrent creator: two processes setting up
/// a brand-new database at the same moment both insert the catalog row and one fails with a
/// duplicate key error (pg_collation, pg_namespace, pg_type, ...).
const DDL_LOCK: i64 = 0x6263_7275_7374_6464;

/// Run DDL in a transaction that holds the DDL advisory lock. `f` re-checks the catalog under the
/// lock (another process may have created the objects meanwhile) and creates what is missing.
/// The lock waits are bounded by `lock_timeout` and retried: the DDL can wait for a table lock
/// (ALTER TABLE, CREATE INDEX) held by another process's open transaction, whose own schema setup
/// may be waiting for this advisory lock; giving the lock up and retrying breaks that cycle.
fn with_ddl_lock(admin: &mut Client, f: impl FnMut(&mut postgres::Transaction<'_>) -> Result<(), postgres::Error>) -> Result<(), StoreError> {
    with_ddl_lock_raw(admin, f).map_err(db)
}

fn with_ddl_lock_raw(admin: &mut Client, mut f: impl FnMut(&mut postgres::Transaction<'_>) -> Result<(), postgres::Error>) -> Result<(), postgres::Error> {
    let mut attempt = 0;
    loop {
        let r = (|| {
            let mut tx = admin.transaction()?;
            tx.batch_execute(&format!("SET LOCAL lock_timeout = '3s'; SELECT pg_advisory_xact_lock({DDL_LOCK});"))?;
            f(&mut tx)?;
            tx.commit()
        })();
        match r {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 100 && e.code() == Some(&postgres::error::SqlState::LOCK_NOT_AVAILABLE) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The DDL that creates what is missing of a table: its schema, the table, columns added since
/// (new fields, extensions) and secondary-key indexes. Empty when everything exists, so tables in
/// use are not touched (CREATE INDEX IF NOT EXISTS would still wait for a SHARE lock).
fn table_ddl<C: postgres::GenericClient>(c: &mut C, sn: &str, qualified: &str, schema: &TableSchema) -> Result<String, postgres::Error> {
    let tn = format!("t{}", schema.id);
    let r = c.query_one(
        "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1), to_regclass($2) IS NOT NULL, \
         ARRAY(SELECT column_name::text FROM information_schema.columns WHERE table_schema = $1 AND table_name = $3), \
         ARRAY(SELECT indexname::text FROM pg_indexes WHERE schemaname = $1 AND tablename = $3)",
        &[&sn, &qualified, &tn],
    )?;
    let (has_schema, has_table): (bool, bool) = (r.get(0), r.get(1));
    let columns: HashSet<String> = r.get::<_, Vec<String>>(2).into_iter().collect();
    let indexes: HashSet<String> = r.get::<_, Vec<String>>(3).into_iter().collect();
    let mut ddl = String::new();
    if !has_schema {
        let _ = writeln!(ddl, "CREATE SCHEMA IF NOT EXISTS \"{sn}\";");
    }
    let pk: Vec<String> = schema.keys[0].iter().map(|&c| format!("f{}", schema.field_ids[c])).collect();
    if !has_table {
        let _ = write!(ddl, "CREATE TABLE IF NOT EXISTS {qualified} (");
        for (i, t) in schema.columns.iter().enumerate() {
            let (ty, def) = col_sql(*t);
            let _ = write!(ddl, "{}f{} {ty} NOT NULL DEFAULT {def}", if i > 0 { ", " } else { "" }, schema.field_ids[i]);
        }
        let _ = write!(ddl, ", PRIMARY KEY ({}));\n", pk.join(", "));
        let _ = writeln!(ddl, "COMMENT ON TABLE {qualified} IS '{}';", schema.name.replace('\'', "''"));
    } else {
        // schema evolution: columns added by new fields / extensions
        for (i, t) in schema.columns.iter().enumerate() {
            let c = format!("f{}", schema.field_ids[i]);
            if !columns.contains(&c) {
                let (ty, def) = col_sql(*t);
                let _ = writeln!(ddl, "ALTER TABLE {qualified} ADD COLUMN IF NOT EXISTS {c} {ty} NOT NULL DEFAULT {def};");
            }
        }
    }
    for (ki, k) in schema.keys.iter().enumerate().skip(1) {
        let name = format!("t{}_k{ki}", schema.id);
        if indexes.contains(&name) {
            continue;
        }
        let mut cols: Vec<String> = k.iter().map(|&c| format!("f{}", schema.field_ids[c])).collect();
        for p in &pk {
            if !cols.contains(p) {
                cols.push(p.clone());
            }
        }
        let _ = writeln!(ddl, "CREATE INDEX IF NOT EXISTS {name} ON {qualified} ({});", cols.join(", "));
    }
    Ok(ddl)
}

impl PgBackend {
    pub fn connect_url(url: &str) -> Result<PgBackend, StoreError> {
        let mut admin = Client::connect(url, NoTls).map_err(db)?;
        // the ci collation and the shared schema; created once per database, under the DDL lock
        const READY: &str = "SELECT EXISTS (SELECT 1 FROM pg_collation WHERE collname = 'ci') AND EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'shared')";
        if !admin.query_one(READY, &[]).map_err(db)?.get::<_, bool>(0) {
            with_ddl_lock(&mut admin, |tx| {
                tx.batch_execute(
                    "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_collation WHERE collname = 'ci') THEN \
                     CREATE COLLATION ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false); END IF; END $$;\n\
                     CREATE SCHEMA IF NOT EXISTS shared;",
                )
            })?;
        }
        // the PostgreSQL locking contract (docs/job-queue.md) is on unless BC_PG_TABLE_LOCKS=0
        if std::env::var("BC_PG_TABLE_LOCKS").map_or(true, |v| v.trim() != "0") {
            set_table_locks(true);
        }
        Ok(PgBackend { sh: Arc::new(Shared { url: url.into(), pool: Mutex::new(vec![]), admin: Mutex::new(admin), ready: Mutex::new(HashSet::new()), queries: Default::default(), nonempty: Mutex::new(HashSet::new()), gens: (0..GEN_BUCKETS).map(|_| Default::default()).collect() }) })
    }
}

/// Create schema/table/columns/indexes on first use (on the admin connection, outside session
/// transactions). Only missing objects are created, under the DDL advisory lock (see
/// [`with_ddl_lock`]), so processes setting up the same database at once do not collide.
fn ensure(sh: &Shared, company: &str, schema: &TableSchema) -> Result<String, StoreError> {
    let sn = schema_name(company, schema.per_company);
    let key = (sn.clone(), schema.id);
    let qualified = format!("\"{sn}\".t{}", schema.id);
    if sh.ready.lock().contains(&key) {
        return Ok(qualified);
    }
    let mut admin = sh.admin.lock();
    sh.admin_retry(&mut admin, |a| {
        if table_ddl(a, &sn, &qualified, schema)?.is_empty() {
            return Ok(());
        }
        with_ddl_lock_raw(a, |tx| {
            let ddl = table_ddl(tx, &sn, &qualified, schema)?;
            if !ddl.is_empty() {
                tx.batch_execute(&ddl)?;
            }
            Ok(())
        })
    })?;
    drop(admin);
    sh.ready.lock().insert(key);
    Ok(qualified)
}

/// Plan cache mode for session connections. PostgreSQL's default (`auto`) plans a prepared
/// statement again on each of its first five executions per connection before it settles on
/// the generic plan; for BC's wide tables a plan costs 0.2-0.4 ms, so a posting paid about a
/// third more on every new connection and for every rarely run statement. `force_generic_plan`
/// plans once and reuses the plan, as SQL Server does for BC's parameterized statements.
/// `BC_PG_PLAN_CACHE=auto` (or `force_custom_plan`) overrides it.
fn plan_cache_mode() -> Option<&'static str> {
    static MODE: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("BC_PG_PLAN_CACHE").ok().as_deref().map(str::trim) {
        None | Some("") | Some("force_generic_plan") => Some("force_generic_plan"),
        Some("auto") => None,
        Some("force_custom_plan") => Some("force_custom_plan"),
        Some(other) => {
            eprintln!("BC_PG_PLAN_CACHE={other:?} is not auto, force_generic_plan or force_custom_plan; using force_generic_plan");
            Some("force_generic_plan")
        }
    })
}

/// A new pooled connection, with the plan cache mode set (see `plan_cache_mode`).
fn connect_client(url: &str) -> Result<Client, StoreError> {
    let mut c = Client::connect(url, NoTls).map_err(db)?;
    if let Some(mode) = plan_cache_mode() {
        c.batch_execute(&format!("SET plan_cache_mode = {mode}")).map_err(db)?;
    }
    // a session waits at most BC's lock timeout for a lock, then fails with BC's error (see `db`)
    if lock_timeout_ms() > 0 {
        c.batch_execute(&format!("SET lock_timeout = {}", lock_timeout_ms())).map_err(db)?;
    }
    Ok(c)
}

impl PgBackend {
    /// An idle pooled connection that is not known to be closed, else a new one.
    fn checkout(&self) -> Result<(Client, FastMap<String, Statement>), StoreError> {
        loop {
            let pooled = self.sh.pool.lock().pop();
            match pooled {
                Some((c, stmts)) if !c.is_closed() => return Ok((c, stmts)),
                Some(_) => continue,
                None => return Ok((connect_client(&self.sh.url)?, FastMap::default())),
            }
        }
    }

    fn connect_pg(&self) -> Result<PgConn, StoreError> {
        let (c, stmts) = self.checkout()?;
        Ok(PgConn {
            sh: self.sh.clone(),
            client: Some(c),
            in_txn: false,
            sp: 0,
            sp_floor: 0,
            aborted: None,
            stmts,
            locked: HashSet::new(),
            tables: HashMap::new(),
            cache: HashMap::new(),
            tx_wrote: HashSet::new(),
            tx_rows: HashMap::new(),
            tx_rows_n: 0,
            tx_reads: HashMap::new(),
            tx_reads_n: 0,
            tx_empty: HashMap::new(),
            lock_dur: None,
        })
    }
}

impl Backend for PgBackend {
    fn connect(&self) -> Result<Box<dyn Conn>, StoreError> {
        Ok(Box::new(self.connect_pg()?))
    }
    fn describe(&self) -> String {
        let u = &self.sh.url;
        let safe: String = u.split_whitespace().filter(|p| !p.starts_with("password")).collect::<Vec<_>>().join(" ");
        format!("PostgreSQL ({safe})")
    }
    fn stats(&self) -> StoreStats {
        let mut a = self.sh.admin.lock();
        let r = self
            .sh
            .admin_retry(&mut a, |a| a.query_one("SELECT count(*)::bigint, coalesce(sum(n_live_tup),0)::bigint FROM pg_stat_user_tables WHERE schemaname = 'shared' OR schemaname LIKE 'c\\_%'", &[]))
            .ok();
        r.map(|r| StoreStats { tables: r.get::<_, i64>(0) as usize, rows: r.get::<_, i64>(1) as usize }).unwrap_or_default()
    }
    fn sync_schema(&self, company: &str, schemas: &[Arc<TableSchema>]) -> Result<usize, StoreError> {
        let mut n = 0;
        for s in schemas {
            ensure(&self.sh, company, s)?;
            n += 1;
        }
        Ok(n)
    }
    fn queries(&self) -> u64 {
        self.sh.queries.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// A session-level advisory lock (pg_try_advisory_lock) on a connection of its own: it
    /// survives the commits of the work it guards, never blocks that work's own row writes, and
    /// PostgreSQL releases it when the connection ends (a crashed or killed server).
    fn try_lock_key(&self, name: &str) -> Result<Option<Box<dyn KeyLock>>, StoreError> {
        let key = key_hash(name);
        let (mut c, mut stmts) = self.checkout()?;
        let got: bool = match c.query_one("SELECT pg_try_advisory_lock($1)", &[&key]) {
            Ok(r) => r.get(0),
            // nothing is held yet: a connection that died with the server is replaced once
            Err(e) if conn_lost(&e) => {
                self.sh.flush_pool();
                (c, stmts) = (connect_client(&self.sh.url)?, FastMap::default());
                c.query_one("SELECT pg_try_advisory_lock($1)", &[&key]).map_err(db)?.get(0)
            }
            Err(e) => return Err(db(e)),
        };
        if !got {
            let mut pool = self.sh.pool.lock();
            if pool.len() < 64 {
                pool.push((c, stmts));
            }
            return Ok(None);
        }
        Ok(Some(Box::new(PgKeyLock { sh: self.sh.clone(), client: Some((c, stmts)), key, name: name.to_string() })))
    }
    fn table_count(&self, company: &str, schema: &Arc<TableSchema>) -> usize {
        let Ok(q) = ensure(&self.sh, company, schema) else { return 0 };
        let mut a = self.sh.admin.lock();
        self.sh.admin_retry(&mut a, |a| a.query_one(&format!("SELECT count(*) FROM {q}"), &[])).map(|r| r.get::<_, i64>(0) as usize).unwrap_or(0)
    }
}

/// Locked reads from the end of a key range take an advisory lock on it (`PgConn::lock_range_end`),
/// ReadIsolation(UpdLock) reads lock rows and AutoIncrement comes from a sequence: on for every
/// PostgreSQL backend unless BC_PG_TABLE_LOCKS=0 (`PgBackend::connect_url`).
static TABLE_LOCKS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_table_locks(on: bool) {
    TABLE_LOCKS.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn table_locks() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let _ = &ENV;
    TABLE_LOCKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The 64-bit advisory-lock key of a lock name (FNV-1a).
fn key_hash(name: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    h as i64
}

/// A held advisory lock ([`Backend::try_lock_key`]) and the connection that holds it.
struct PgKeyLock {
    sh: Arc<Shared>,
    client: Option<(Client, FastMap<String, Statement>)>,
    key: i64,
    name: String,
}

static ATTEMPT_TABLE: std::sync::Once = std::sync::Once::new();

impl KeyLock for PgKeyLock {
    /// Start counts live in `shared.bc_task_attempt` (bc-rust's own table, not a BC table).
    fn attempt(&mut self) -> Result<i32, StoreError> {
        let c = &mut self.client.as_mut().expect("held").0;
        ATTEMPT_TABLE.call_once(|| {
            // under the DDL lock: two processes starting their pools on one database both
            // created it (duplicate key in pg_type / pg_class)
            let _ = with_ddl_lock(c, |tx| {
                tx.batch_execute("CREATE SCHEMA IF NOT EXISTS shared; CREATE TABLE IF NOT EXISTS shared.bc_task_attempt (name text PRIMARY KEY, attempts integer NOT NULL, started timestamptz NOT NULL DEFAULT now())")
            });
        });
        let r = c
            .query_one(
                "INSERT INTO shared.bc_task_attempt AS a (name, attempts) VALUES ($1, 1) ON CONFLICT (name) DO UPDATE SET attempts = a.attempts + 1, started = now() RETURNING a.attempts",
                &[&self.name],
            )
            .map_err(db)?;
        Ok(r.get(0))
    }

    fn finish(&mut self) {
        if let Some((c, _)) = self.client.as_mut() {
            let _ = c.execute("DELETE FROM shared.bc_task_attempt WHERE name = $1", &[&self.name]);
        }
    }
}

impl Drop for PgKeyLock {
    fn drop(&mut self) {
        if let Some((mut c, stmts)) = self.client.take() {
            // a connection that could not unlock is closed (dropped), which also unlocks
            if c.query_one("SELECT pg_advisory_unlock($1)", &[&self.key]).is_ok() {
                let mut pool = self.sh.pool.lock();
                if pool.len() < 64 {
                    pool.push((c, stmts));
                }
            }
        }
    }
}

/// (PostgreSQL schema, table id): the unit of locking and cache invalidation.
type TKey = (String, i64);

/// Per-table SQL fragments, built once per connection (wide tables have hundreds of columns).
struct TInfo {
    ck: TKey,
    /// Qualified table name.
    name: String,
    /// Column list in schema order: "f1, f2, ...".
    cols: String,
}

/// FxHash-style hasher for maps keyed by SQL text (long keys, no untrusted input).
#[derive(Default, Clone, Copy)]
struct FastHash(u64);

impl std::hash::Hasher for FastHash {
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(K);
        }
        for &b in chunks.remainder() {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(K);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type FastMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<FastHash>>;

fn tkey(company: &str, schema: &TableSchema) -> TKey {
    (schema_name(company, schema.per_company), schema.id)
}

/// A row this transaction has already seen. `held` = this transaction holds the
/// row lock (it wrote the row or read it FOR UPDATE), so no other session can
/// change it before commit and the entry also answers locked reads.
struct TxRow {
    row: Option<Row>,
    held: bool,
}

/// A cached result of an unlocked read.
#[derive(Clone)]
enum Cached {
    Rows(Vec<Row>),
    Count(usize),
    Agg(AggOut),
}

/// Prepared statements kept per pooled connection.
const STMTS_MAX: usize = 4_096;
/// Per-transaction cache limits; past them reads simply go to the database.
const TX_ROWS_MAX: usize = 20_000;
const TX_READS_MAX: usize = 4_096;
/// Only small results are kept (FindFirst/IsEmpty/short FindSet); big scans rarely repeat.
const TX_READ_ROWS_MAX: usize = 8;

pub struct PgConn {
    sh: Arc<Shared>,
    client: Option<Client>,
    in_txn: bool,
    sp: usize,
    /// `sp` when the current transaction began: savepoints up to it belong to transactions
    /// that have ended (committed), see `rollback_to`.
    sp_floor: usize,
    /// The first error of a statement in the current transaction: PostgreSQL has aborted the
    /// transaction then, until it is rolled back (to a savepoint taken before the error).
    aborted: Option<String>,
    stmts: FastMap<String, Statement>,
    locked: HashSet<TKey>,
    tables: HashMap<i64, Vec<(String, Arc<TInfo>)>>,
    /// Get cache of unlocked reads: (schema, table) -> (the table's commit counter when the
    /// entries were read, pk bytes -> row (None = known missing)). Writes to a table drop its
    /// entries; locked tables bypass the cache; rollback (to a savepoint) ends it. A commit of any
    /// session of this backend that wrote the table bumps its counter (`Shared::gens`), and the
    /// entries read before are dropped: BC's server data cache, invalidated by every session's
    /// writes (a per-session cache that ignored other sessions' commits served rows hours old).
    /// Other processes' commits are seen at the next request (bc-nst ends its read caches after
    /// every call; API requests and background tasks get a new session).
    cache: HashMap<TKey, (u64, HashMap<Box<[u8]>, Option<Row>>)>,
    /// Tables the current transaction wrote: their commit counters are bumped at commit.
    tx_wrote: HashSet<TKey>,
    /// Rows read or written in the current transaction, by primary key.
    /// Cleared at commit, rollback and rollback to a savepoint.
    tx_rows: HashMap<TKey, HashMap<Box<[u8]>, TxRow>>,
    tx_rows_n: usize,
    /// Results of unlocked reads in the current transaction, keyed by statement
    /// text plus exact parameter values. Any write to the table drops its entries;
    /// reads of locked tables bypass it. Cleared at commit, rollback and rollback
    /// to a savepoint.
    tx_reads: HashMap<TKey, FastMap<Vec<u8>, Cached>>,
    tx_reads_n: usize,
    /// Whether a table had no rows at all, learned in the current transaction
    /// (see `probe_empty`). Same validity rules as `tx_reads`.
    tx_empty: HashMap<TKey, bool>,
    /// The session's lock timeout in seconds when AL set it (LockTimeoutDuration, LockTimeout(false) = -1,
    /// no limit; None = the server's default): applied per transaction (`SET LOCAL lock_timeout` at
    /// BEGIN), so a pooled connection never keeps it.
    lock_dur: Option<i64>,
}

impl Drop for PgConn {
    fn drop(&mut self) {
        if let Some(mut c) = self.client.take() {
            if self.in_txn {
                let _ = c.batch_execute("ROLLBACK");
            }
            if c.is_closed() {
                return;
            }
            let mut stmts = std::mem::take(&mut self.stmts);
            if stmts.len() > STMTS_MAX {
                stmts.clear(); // closes them on the server
            }
            let mut pool = self.sh.pool.lock();
            if pool.len() < 64 {
                pool.push((c, stmts));
            }
        }
    }
}

fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BC_PG_TRACE").is_ok())
}

/// `BC_PG_TRACE=1`: one stderr line per round trip (kind, microseconds, SQL,
/// parameters, rows) and per Conn call, for profiling.
fn trace(kind: &str, sql: &str, t0: std::time::Instant) {
    if tracing() {
        eprintln!("PGTRACE\t{kind}\t{}\t{sql}", t0.elapsed().as_micros());
    }
}

fn trace_q(kind: &str, q: &Sql, n: u64, t0: std::time::Instant) {
    if tracing() {
        let ps: Vec<String> = q.params.iter().map(|p| format!("{p:?}")).collect();
        eprintln!("PGTRACE\t{kind}\t{}\t{}\x1f{}\x1f{n}", t0.elapsed().as_micros(), q.text, ps.join("|"));
    }
}

struct Span(&'static str, std::time::Instant);
impl Drop for Span {
    fn drop(&mut self) {
        if tracing() {
            eprintln!("PGTRACE\tconn\t{}\t{}", self.1.elapsed().as_micros(), self.0);
        }
    }
}
fn span(n: &'static str) -> Span {
    Span(n, std::time::Instant::now())
}

struct Sql {
    text: String,
    params: Vec<P>,
    /// Exact, type-tagged encoding of the parameter values (read cache key).
    key: Vec<u8>,
}

impl Sql {
    fn new() -> Sql {
        Sql { text: String::new(), params: Vec::new(), key: Vec::new() }
    }

    /// Bind a column value.
    fn v(&mut self, t: ColType, v: &Value) -> String {
        exact_key(v, &mut self.key);
        self.p(to_param(t, v))
    }

    /// Bind a text parameter (LIKE patterns).
    fn s(&mut self, s: String) -> String {
        self.key.push(0xFF);
        self.key.extend_from_slice(&(s.len() as u32).to_le_bytes());
        self.key.extend_from_slice(s.as_bytes());
        self.p(Box::new(s))
    }

    fn p(&mut self, v: P) -> String {
        self.params.push(v);
        format!("${}", self.params.len())
    }

    fn cache_key(&self) -> Vec<u8> {
        let mut k = Vec::with_capacity(self.text.len() + 1 + self.key.len());
        k.extend_from_slice(self.text.as_bytes());
        k.push(0);
        k.extend_from_slice(&self.key);
        k
    }
}

/// Injective encoding of a value (variant tag + exact bytes; text is case-sensitive).
fn exact_key(v: &Value, out: &mut Vec<u8>) {
    let bytes = |out: &mut Vec<u8>, tag: u8, b: &[u8]| {
        out.push(tag);
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    };
    match v {
        Value::Int(i) => bytes(out, 1, &i.to_le_bytes()),
        Value::BigInt(i) => bytes(out, 2, &i.to_le_bytes()),
        Value::Dec(d) => bytes(out, 3, &d.serialize()),
        Value::Bool(b) => bytes(out, 4, &[*b as u8]),
        Value::Text(s) => bytes(out, 5, s.as_bytes()),
        Value::Date(d) => bytes(out, 6, &d.to_le_bytes()),
        Value::Time(t) => bytes(out, 7, &t.to_le_bytes()),
        Value::DateTime(t) => bytes(out, 8, &t.to_le_bytes()),
        Value::Duration(t) => bytes(out, 9, &t.to_le_bytes()),
        Value::Guid(g) => bytes(out, 10, g),
        Value::Option(i) => bytes(out, 11, &i.to_le_bytes()),
        Value::Blob(b) => bytes(out, 12, b),
    }
}

/// The value a read returns after `v` was written to a column of type `t`
/// (the composition of `to_param` and `read_row`). None if that combination
/// is not one a successful write can produce.
fn canon_value(t: ColType, v: &Value) -> Option<Value> {
    let ts = || match v {
        Value::Date(d) => Some(date_to_ts(*d)),
        Value::Time(x) => Some(time_to_ts(*x)),
        Value::DateTime(ms) => Some(dt_to_ts(*ms)),
        _ => None,
    };
    Some(match (t, v) {
        (ColType::Int, Value::Int(i) | Value::Option(i)) => Value::Int(*i),
        (ColType::Option, Value::Int(i) | Value::Option(i)) => Value::Option(*i),
        (ColType::BigInt, Value::BigInt(i) | Value::Duration(i)) => Value::BigInt(*i),
        (ColType::Duration, Value::BigInt(i) | Value::Duration(i)) => Value::Duration(*i),
        // rust_decimal sends every zero (any sign or scale) as plain 0; other values keep their scale
        (ColType::Dec, Value::Dec(d)) => Value::Dec(if d.is_zero() { Decimal::ZERO } else { *d }),
        (ColType::Bool, Value::Bool(b)) => Value::Bool(*b),
        (ColType::Code | ColType::Text, Value::Text(s)) if !s.contains('\0') => Value::Text(s.clone()),
        (ColType::Date, _) => Value::Date(ts_to_date(ts()?)),
        (ColType::Time, _) => Value::Time(ts_to_time(ts()?)),
        (ColType::DateTime, _) => Value::DateTime(ts_to_dt(ts()?)),
        (ColType::Guid, Value::Guid(g)) => Value::Guid(*g),
        (ColType::Blob, Value::Blob(b)) => Value::Blob(b.clone()),
        _ => return None,
    })
}

fn canon_row(schema: &TableSchema, row: &Row) -> Option<Row> {
    if row.len() != schema.columns.len() {
        return None;
    }
    schema.columns.iter().zip(row).map(|(t, v)| canon_value(*t, v)).collect()
}

fn like_pattern(p: &[char]) -> String {
    let mut s = String::new();
    for &c in p {
        match c {
            '*' => s.push('%'),
            '?' => s.push('_'),
            '%' | '_' | '\\' => {
                s.push('\\');
                s.push(c);
            }
            c => s.push(c),
        }
    }
    s
}

fn node_sql(q: &mut Sql, col: &str, t: ColType, n: &FNode) -> String {
    match n {
        FNode::Or(xs) => format!("({})", xs.iter().map(|x| node_sql(q, col, t, x)).collect::<Vec<_>>().join(" OR ")),
        FNode::And(xs) => format!("({})", xs.iter().map(|x| node_sql(q, col, t, x)).collect::<Vec<_>>().join(" AND ")),
        FNode::Eq(v) => format!("{col} = {}", q.v(t, v)),
        FNode::Ne(v) => format!("{col} <> {}", q.v(t, v)),
        FNode::Lt(v) => format!("{col} < {}", q.v(t, v)),
        FNode::Le(v) => format!("{col} <= {}", q.v(t, v)),
        FNode::Gt(v) => format!("{col} > {}", q.v(t, v)),
        FNode::Ge(v) => format!("{col} >= {}", q.v(t, v)),
        FNode::Range(a, b) => {
            let mut parts = vec![];
            if let Some(a) = a {
                parts.push(format!("{col} >= {}", q.v(t, a)));
            }
            if let Some(b) = b {
                parts.push(format!("{col} <= {}", q.v(t, b)));
            }
            if parts.is_empty() {
                "TRUE".into()
            } else {
                format!("({})", parts.join(" AND "))
            }
        }
        FNode::Like(p) => {
            let pat = like_pattern(p);
            format!("lower({col} COLLATE \"default\") LIKE {} ESCAPE '\\'", q.s(pat))
        }
        FNode::True => "TRUE".into(),
        FNode::False => "FALSE".into(),
    }
}

fn where_sql(q: &mut Sql, schema: &TableSchema, filters: &[FieldFilter]) -> String {
    let mut conds = vec![];
    for f in filters {
        let col = format!("f{}", schema.field_ids[f.field]);
        conds.push(node_sql(q, &col, schema.columns[f.field], &f.node));
    }
    if conds.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conds.join(" AND "))
    }
}

fn select_list(schema: &TableSchema) -> String {
    schema.field_ids.iter().map(|id| format!("f{id}")).collect::<Vec<_>>().join(", ")
}

/// Ordering columns for key `k`: key fields then the remaining PK fields.
fn order_cols(schema: &TableSchema, key: usize) -> Vec<usize> {
    let mut cols: Vec<usize> = schema.keys[key].clone();
    for &p in &schema.keys[0] {
        if !cols.contains(&p) {
            cols.push(p);
        }
    }
    cols
}

/// SELECT for a range read in key order (keyset pagination after `req.after`).
fn scan_sql(ti: &TInfo, schema: &TableSchema, req: &ScanReq, limit: usize, fu: bool) -> Sql {
    let t = &ti.name;
    let mut q = Sql::new();
    let mut w = where_sql(&mut q, schema, req.filters);
    let key = req.key.min(schema.keys.len() - 1);
    let cols = order_cols(schema, key);
    if let Some(after) = req.after {
        let names: Vec<String> = cols.iter().map(|&c| format!("f{}", schema.field_ids[c])).collect();
        let vals: Vec<String> = cols.iter().map(|&c| q.v(schema.columns[c], &after[c])).collect();
        let op = if req.reverse { "<" } else { ">" };
        let cond = format!("({}) {op} ({})", names.join(", "), vals.join(", "));
        w = if w.is_empty() { format!(" WHERE {cond}") } else { format!("{w} AND {cond}") };
    }
    let dir = if req.reverse { " DESC" } else { "" };
    let order: Vec<String> = cols.iter().map(|&c| format!("f{}{dir}", schema.field_ids[c])).collect();
    let limit = if limit == usize::MAX { String::new() } else { format!(" LIMIT {limit}") };
    let lock = if fu { " FOR UPDATE" } else { "" };
    q.text = format!("SELECT {} FROM {t}{w} ORDER BY {}{limit}{lock}", ti.cols, order.join(", "));
    q
}

impl PgConn {
    fn c(&mut self) -> &mut Client {
        self.client.as_mut().expect("connection")
    }

    /// A failed statement inside a transaction aborts it (see `aborted`).
    fn note_abort<T>(&mut self, r: &Result<T, StoreError>) {
        if let (true, None, Err(e)) = (self.in_txn, &self.aborted, r) {
            self.aborted = Some(e.to_string());
        }
    }

    fn prepared(&mut self, sql: &str) -> Result<Statement, postgres::Error> {
        if let Some(s) = self.stmts.get(sql) {
            return Ok(s.clone());
        }
        let t0 = std::time::Instant::now();
        let st = self.client.as_mut().unwrap().prepare(sql)?;
        trace("prepare", sql, t0);
        self.stmts.insert(sql.to_string(), st.clone());
        Ok(st)
    }

    /// The connection died outside a transaction (PostgreSQL restarted since the session or
    /// the pool got it): replace it, so the caller can send the statement once more. Nothing
    /// of the session is lost then; inside a transaction the transaction is, and the error
    /// stands (the next statement after the rollback reconnects).
    fn revive(&mut self, e: &postgres::Error) -> bool {
        if self.in_txn || !conn_lost(e) {
            return false;
        }
        self.sh.flush_pool();
        match connect_client(&self.sh.url) {
            Ok(c) => {
                eprintln!("PostgreSQL connection lost ({e}); reconnected");
                self.client = Some(c);
                self.stmts = FastMap::default();
                self.cache.clear();
                true
            }
            Err(_) => false,
        }
    }

    /// Run `f`; when the connection turns out dead outside a transaction, once more on a new one.
    fn retry<T>(&mut self, mut f: impl FnMut(&mut PgConn) -> Result<T, postgres::Error>) -> Result<T, postgres::Error> {
        match f(self) {
            Err(e) if self.revive(&e) => f(self),
            r => r,
        }
    }

    fn query(&mut self, q: &Sql) -> Result<Vec<postgres::Row>, StoreError> {
        self.sh.queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let params: Vec<&(dyn ToSql + Sync)> = q.params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
        let t0 = std::time::Instant::now();
        let r = self
            .retry(|c| {
                let st = c.prepared(&q.text)?;
                c.c().query(&st, &params)
            })
            .map_err(db);
        trace_q("query", q, r.as_ref().map(|v| v.len() as u64).unwrap_or(0), t0);
        self.note_abort(&r);
        r
    }

    fn exec(&mut self, q: &Sql) -> Result<u64, StoreError> {
        self.sh.queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let params: Vec<&(dyn ToSql + Sync)> = q.params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
        let t0 = std::time::Instant::now();
        let r = self
            .retry(|c| {
                let st = c.prepared(&q.text)?;
                c.c().execute(&st, &params)
            })
            .map_err(db);
        trace_q("exec", q, *r.as_ref().unwrap_or(&0), t0);
        self.note_abort(&r);
        r
    }

    /// In a transaction already: the session's lock timeout from the next statement on, until it ends
    /// (later transactions get it at BEGIN).
    fn apply_lock_timeout(&mut self) {
        if self.in_txn {
            let ms = self.lock_dur.map_or(lock_timeout_ms() as i64, lock_timeout_ms_of);
            let r = self.c().batch_execute(&format!("SET LOCAL lock_timeout = {ms}")).map_err(db);
            self.note_abort(&r);
        }
    }

    fn begin(&mut self) -> Result<(), StoreError> {
        if !self.in_txn {
            let t0 = std::time::Instant::now();
            let sql = match self.lock_dur {
                None => "BEGIN".to_string(),
                Some(secs) => format!("BEGIN; SET LOCAL lock_timeout = {}", lock_timeout_ms_of(secs)),
            };
            self.retry(|c| c.c().batch_execute(&sql)).map_err(db)?;
            trace("ctl", "BEGIN", t0);
            self.in_txn = true;
            self.sp_floor = self.sp;
        }
        Ok(())
    }

    /// Storage for the table (created on first use) and its SQL fragments.
    fn table(&mut self, company: &str, schema: &TableSchema) -> Result<Arc<TInfo>, StoreError> {
        note_table(schema);
        let comp = if schema.per_company { company } else { "" };
        if let Some((_, ti)) = self.tables.get(&schema.id).and_then(|v| v.iter().find(|(c, _)| c == comp)) {
            return Ok(ti.clone());
        }
        let name = ensure(&self.sh, company, schema)?;
        let ti = Arc::new(TInfo { ck: tkey(company, schema), name, cols: select_list(schema) });
        self.tables.entry(schema.id).or_default().push((comp.to_string(), ti.clone()));
        Ok(ti)
    }

    /// Reads of this table take row locks (explicit request, or LockTable in this transaction).
    fn for_update(&self, ck: &TKey, explicit: bool) -> bool {
        explicit || (self.in_txn && self.locked.contains(ck))
    }

    /// Drop everything learned in the current transaction.
    fn forget_tx(&mut self) {
        self.tx_rows.clear();
        self.tx_rows_n = 0;
        self.tx_reads.clear();
        self.tx_reads_n = 0;
        self.tx_empty.clear();
    }

    fn note_row(&mut self, ck: &TKey, pk: Box<[u8]>, row: Option<Row>, held: bool) {
        let m = self.tx_rows.entry(ck.clone()).or_default();
        match m.get_mut(&pk) {
            // never downgrade a held entry with an unlocked read (same value anyway)
            Some(e) if held || !e.held => *e = TxRow { row, held },
            Some(_) => {}
            None if self.tx_rows_n < TX_ROWS_MAX => {
                m.insert(pk, TxRow { row, held });
                self.tx_rows_n += 1;
            }
            None => {}
        }
    }

    fn forget_row(&mut self, ck: &TKey, pk: &[u8]) {
        if let Some(m) = self.tx_rows.get_mut(ck) {
            if m.remove(pk).is_some() {
                self.tx_rows_n -= 1;
            }
        }
    }

    fn cached_read(&self, ck: &TKey, k: &[u8]) -> Option<&Cached> {
        self.tx_reads.get(ck).and_then(|m| m.get(k))
    }

    fn remember_read(&mut self, ck: &TKey, k: Vec<u8>, v: Cached) {
        if self.tx_reads_n >= TX_READS_MAX {
            return;
        }
        if self.tx_reads.entry(ck.clone()).or_default().insert(k, v).is_none() {
            self.tx_reads_n += 1;
        }
    }

    /// Before a write to a table: results of earlier reads may change.
    fn invalidate(&mut self, ck: &TKey) {
        if !self.tx_wrote.contains(ck) {
            self.tx_wrote.insert(ck.clone());
        }
        self.cache.remove(ck);
        self.tx_empty.remove(ck);
        if let Some(m) = self.tx_reads.remove(ck) {
            self.tx_reads_n -= m.len();
        }
    }

    /// Unlocked reads of this table may use what this transaction already learned.
    fn cacheable(&self, ck: &TKey) -> bool {
        !self.locked.contains(ck)
    }

    /// The table is known to have no rows (answers unlocked reads without a round trip).
    fn known_empty(&self, ck: &TKey) -> bool {
        self.cacheable(ck) && self.tx_empty.get(ck) == Some(&true)
    }

    /// Whether the next unlocked read of the table should also check if the
    /// table is empty. The check rides along in the same statement, so it costs
    /// no round trip; many setup and feature tables are empty, and posting
    /// probes some of them a dozen times per transaction.
    fn probe_empty(&self, ck: &TKey) -> bool {
        self.cacheable(ck) && !self.tx_empty.contains_key(ck) && !self.sh.nonempty.lock().contains(ck)
    }

    /// Record what this transaction learned about the table having rows.
    fn learn_empty(&mut self, ck: &TKey, empty: bool) {
        if !empty {
            self.sh.nonempty.lock().insert(ck.clone());
        }
        if self.cacheable(ck) {
            self.tx_empty.insert(ck.clone(), empty);
        }
    }

    /// Run a row-returning unlocked SELECT (`q.text` starts with "SELECT " and
    /// lists the table's columns), checking table emptiness along the way when
    /// it is not known yet. The check is a UNION ALL branch that yields one
    /// marker row only when the table has no rows at all, in which case the
    /// real branch is empty too.
    fn select_rows(&mut self, q: &mut Sql, t: &str, schema: &TableSchema, ck: &TKey) -> Result<Vec<Row>, StoreError> {
        if !self.probe_empty(ck) {
            let rows: Vec<Row> = self.query(q)?.iter().map(|r| read_row(schema, r)).collect::<Result<_, _>>()?;
            if !rows.is_empty() && !self.tx_empty.contains_key(ck) {
                self.learn_empty(ck, false);
            }
            return Ok(rows);
        }
        let nulls = vec!["NULL"; schema.columns.len()].join(", ");
        let inner = &q.text["SELECT ".len()..];
        q.text = format!("(SELECT false, {inner}) UNION ALL (SELECT true, {nulls} WHERE NOT EXISTS (SELECT 1 FROM {t}))");
        let res = self.query(q)?;
        if res.first().is_some_and(|r| r.get::<_, bool>(0)) {
            self.learn_empty(ck, true);
            return Ok(vec![]);
        }
        self.learn_empty(ck, false);
        res.iter().map(|r| read_row_at(schema, r, 1)).collect()
    }

    /// After a write statement: on success this transaction holds the row lock
    /// and knows the row's value (`known`: the stored row equals `row`); on
    /// failure forget the row; on a database error the transaction is aborted,
    /// so forget everything.
    fn after_write(&mut self, ck: &TKey, schema: &TableSchema, row: &Row, res: &Result<(), StoreError>, deleted: bool, known: bool) {
        let pk = schema.pk(row);
        match res {
            Ok(()) if deleted => self.note_row(ck, pk, None, true),
            Ok(()) => match canon_row(schema, row).filter(|_| known) {
                Some(r) => self.note_row(ck, pk, Some(r), true),
                None => self.forget_row(ck, &pk),
            },
            Err(StoreError::Other(_)) => {
                self.cache.clear();
                self.forget_tx();
            }
            Err(_) => {
                // a failed Insert of a row this transaction wrote is expected; keep that entry
                let ours = self.tx_rows.get(ck).and_then(|m| m.get(&pk)).is_some_and(|e| e.held && e.row.is_some());
                if !(ours && matches!(res, Err(StoreError::Duplicate))) {
                    self.forget_row(ck, &pk);
                }
            }
        }
    }
}

fn exact_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Dec(x), Value::Dec(y)) => x.serialize() == y.serialize(),
        _ => a == b,
    }
}

impl PgConn {
    /// A locked read from the end of a key range (FindLast / Find('+') after LockTable or
    /// ReadIsolation(UpdLock); docs/job-queue.md) first takes an advisory transaction lock on that
    /// range's end: table, key and the equality filters on its leading fields. Two sessions that
    /// append to one range ("FindLast; Insert(last + 1)", legacy Entry No. assignment) then run
    /// one after the other, and the second reads the first's committed row; FOR UPDATE alone
    /// waits for the old last row and returns it again. Appends to other ranges don't wait.
    fn lock_range_end(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<(), StoreError> {
        if !req.reverse || !table_locks() {
            return Ok(());
        }
        let ck = tkey(company, schema);
        if !self.for_update(&ck, req.lock) {
            return Ok(());
        }
        note_table(schema);
        let (prefix, _) = key_bounds(schema, req.key, req.filters);
        // The posting ledgers' ends share one lock: G/L Entry (17), G/L Register (45) and Item
        // Ledger Entry (32), whole table on the primary key. BC takes them in different orders:
        // journal posting locks G/L Register before G/L Entry (Gen. Jnl.-Post
        // Batch.FindNextGLRegisterNo), document posting G/L Entry first (legacy Sales/Purch.-Post
        // LockTables, Gen. Jnl.-Post Line.InitNextEntryNo) and G/L Register later (StartPosting),
        // and item and G/L posting meet on Item Ledger Entry and G/L Entry in both orders. On SQL
        // Server that is BC's deadlock error on G/L Entry; the I14 soak hit it within minutes. A transaction that locks one of these
        // ends posts and locks the others before it commits, so one lock serializes the postings
        // where they first meet (G/L Entry's lock serializes them anyway) and they can't deadlock.
        let table = if matches!(schema.id, 45 | 32) && req.key == 0 && prefix.is_empty() { 17 } else { ck.1 };
        let mut name = format!("range:{}.{}:{}:", ck.0, table, req.key);
        for b in &prefix {
            let _ = write!(name, "{b:02x}");
        }
        let t0 = std::time::Instant::now();
        let r = self.c().query_one("SELECT pg_advisory_xact_lock($1)::text", &[&key_hash(&name)]).map_err(db);
        self.note_abort(&r);
        r?;
        trace("ctl", "LOCKRANGE", t0);
        Ok(())
    }

    fn get_cached(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, lock: bool) -> Result<Option<Row>, StoreError> {
        let ck = tkey(company, schema);
        let fu = self.for_update(&ck, lock);
        let pkb = schema.pk(key);
        if let Some(e) = self.tx_rows.get(&ck).and_then(|m| m.get(&pkb)) {
            if e.held || !fu {
                return Ok(e.row.clone());
            }
        }
        if !fu {
            if let Some((g, m)) = self.cache.get(&ck) {
                if *g != self.sh.gen(&ck) {
                    // a transaction that wrote the table committed since these were read
                    self.cache.remove(&ck);
                } else if let Some(hit) = m.get(&pkb) {
                    return Ok(hit.clone());
                }
            }
            if self.known_empty(&ck) {
                return Ok(None);
            }
        }
        // the counter before the read: a commit while it runs drops what it returns
        let g0 = self.sh.gen(&ck);
        let r = self.get_db(company, schema, key, fu)?;
        if fu {
            if let (true, Some(row)) = (self.in_txn, &r) {
                self.note_row(&ck, pkb, Some(row.clone()), true);
            }
        } else {
            let e = self.cache.entry(ck).or_insert_with(|| (g0, HashMap::new()));
            if e.0 != g0 {
                *e = (g0, HashMap::new());
            }
            e.1.insert(pkb, r.clone());
        }
        Ok(r)
    }

    fn get_db(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, fu: bool) -> Result<Option<Row>, StoreError> {
        let ti = self.table(company, schema)?;
        let t = &ti.name;
        let mut q = Sql::new();
        let conds: Vec<String> = schema.keys[0]
            .iter()
            .map(|&c| {
                let ph = q.v(schema.columns[c], &key[c]);
                format!("f{} = {ph}", schema.field_ids[c])
            })
            .collect();
        let lock = if fu { " FOR UPDATE" } else { "" };
        q.text = format!("SELECT {} FROM {t} WHERE {}{lock}", ti.cols, conds.join(" AND "));
        if fu {
            let rows = self.query(&q)?;
            return rows.first().map(|r| read_row(schema, r)).transpose();
        }
        Ok(self.select_rows(&mut q, t, schema, &ti.ck)?.into_iter().next())
    }

    fn scan_db(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Row>, StoreError> {
        let ti = self.table(company, schema)?;
        let (t, ck) = (&ti.name, ti.ck.clone());
        let fu = self.for_update(&ck, req.lock);
        let cacheable = !fu && self.cacheable(&ck);
        // FindFirst/FindLast/IsEmpty read one row more, so that a following Next
        // is answered from what was read (see below). Locked reads don't: that
        // would lock a row the caller never asked for.
        let overfetch = cacheable && req.limit == 1 && req.after.is_none();
        let mut q = scan_sql(&ti, schema, req, if overfetch { 2 } else { req.limit }, fu);
        let ck_read = if cacheable { Some(q.cache_key()) } else { None };
        if cacheable && self.known_empty(&ck) {
            return Ok(vec![]);
        }
        if let Some(Cached::Rows(rows)) = ck_read.as_ref().and_then(|k| self.cached_read(&ck, k)) {
            return Ok(rows.iter().take(req.limit).cloned().collect());
        }
        let mut rows: Vec<Row> = if fu {
            self.query(&q)?.iter().map(|r| read_row(schema, r)).collect::<Result<_, _>>()?
        } else {
            self.select_rows(&mut q, t, schema, &ck)?
        };
        if fu {
            if self.in_txn {
                for r in &rows {
                    self.note_row(&ck, schema.pk(r), Some(r.clone()), true);
                }
            }
        } else if let Some(k) = ck_read.filter(|_| rows.len() <= TX_READ_ROWS_MAX) {
            for r in &rows {
                self.note_row(&ck, schema.pk(r), Some(r.clone()), false);
            }
            if overfetch && !rows.is_empty() {
                // Next() after this row asks for the rows following it: the one
                // extra row read here, or none if the set has just this row. A
                // shorter batch than asked for is fine (Next reads on when it runs
                // out); an empty one ends the loop, and is only stored when the
                // set really ended.
                let next = ScanReq { key: req.key, reverse: req.reverse, filters: req.filters, after: Some(&rows[0]), limit: self.batch_hint(), lock: false };
                let nk = scan_sql(&ti, schema, &next, next.limit, false).cache_key();
                self.remember_read(&ck, nk, Cached::Rows(rows[1..].to_vec()));
            }
            self.remember_read(&ck, k, Cached::Rows(rows.clone()));
        }
        rows.truncate(req.limit);
        Ok(rows)
    }
}

impl Conn for PgConn {
    fn get(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, lock: bool) -> Result<Option<Row>, StoreError> {
        let _sp = span("get");
        // A locked read starts the transaction, as in BC, so its row lock lasts until commit. In
        // autocommit the lock ended with the statement: the stale-record check's locked read
        // (recops rec_stored_for_write) of a request that hadn't written yet then let another
        // session's change in before the UPDATE, which wrote the whole stale row back.
        if lock && !self.in_txn {
            self.begin()?;
        }
        self.get_cached(company, schema, key, lock)
    }

    fn scan(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Row>, StoreError> {
        let _sp = span("scan");
        // a locked scan holds its row locks and its range-end lock until commit (see `get`)
        if req.lock && !self.in_txn {
            self.begin()?;
        }
        self.lock_range_end(company, schema, req)?;
        self.scan_db(company, schema, req)
    }

    fn count(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter]) -> Result<usize, StoreError> {
        let _sp = span("count");
        let ti = self.table(company, schema)?;
        let (t, ck) = (&ti.name, ti.ck.clone());
        let mut q = Sql::new();
        let w = where_sql(&mut q, schema, filters);
        q.text = format!("SELECT count(*) FROM {t}{w}");
        let k = if self.cacheable(&ck) { Some(q.cache_key()) } else { None };
        if self.known_empty(&ck) {
            return Ok(0);
        }
        if let Some(Cached::Count(n)) = k.as_ref().and_then(|k| self.cached_read(&ck, k)) {
            return Ok(*n);
        }
        let probe = self.probe_empty(&ck);
        if probe {
            q.text = format!("SELECT count(*), NOT EXISTS (SELECT 1 FROM {t}) FROM {t}{w}");
        }
        let rows = self.query(&q)?;
        let n = rows[0].get::<_, i64>(0) as usize;
        if probe {
            self.learn_empty(&ck, rows[0].get::<_, bool>(1));
        }
        if let Some(k) = k {
            self.remember_read(&ck, k, Cached::Count(n));
        }
        Ok(n)
    }

    fn aggregate(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter], col: usize, agg: Agg) -> Result<AggOut, StoreError> {
        let _sp = span("aggregate");
        let ti = self.table(company, schema)?;
        let (t, ck) = (&ti.name, ti.ck.clone());
        let mut q = Sql::new();
        let w = where_sql(&mut q, schema, filters);
        let c = format!("f{}", schema.field_ids[col]);
        q.text = match agg {
            Agg::Count => format!("SELECT count(*) FROM {t}{w}"),
            Agg::Exists => format!("SELECT EXISTS (SELECT 1 FROM {t}{w})"),
            Agg::Sum => format!("SELECT coalesce(sum({c}), 0)::numeric FROM {t}{w}"),
            Agg::Min => format!("SELECT {} FROM {t}{w} ORDER BY {c} LIMIT 1", ti.cols),
            Agg::Max => format!("SELECT {} FROM {t}{w} ORDER BY {c} DESC LIMIT 1", ti.cols),
        };
        // Min/Max return one column of the row; keep the column in the key
        q.key.extend_from_slice(&(col as u32).to_le_bytes());
        let k = if self.cacheable(&ck) { Some(q.cache_key()) } else { None };
        if self.known_empty(&ck) {
            return Ok(match agg {
                Agg::Count => AggOut::Count(0),
                Agg::Exists => AggOut::Exists(false),
                Agg::Sum => AggOut::Sum(Decimal::ZERO),
                Agg::Min | Agg::Max => AggOut::Value(None),
            });
        }
        if let Some(Cached::Agg(a)) = k.as_ref().and_then(|k| self.cached_read(&ck, k)) {
            return Ok(a.clone());
        }
        // one-row aggregates carry the emptiness check as an extra column
        let probe = self.probe_empty(&ck) && !matches!(agg, Agg::Min | Agg::Max);
        if probe {
            let e = format!("NOT EXISTS (SELECT 1 FROM {t})");
            q.text = match agg {
                Agg::Count => format!("SELECT count(*), {e} FROM {t}{w}"),
                Agg::Exists => format!("SELECT EXISTS (SELECT 1 FROM {t}{w}), {e}"),
                _ => format!("SELECT coalesce(sum({c}), 0)::numeric, {e} FROM {t}{w}"),
            };
        }
        let rows = self.query(&q)?;
        if probe {
            self.learn_empty(&ck, rows[0].get::<_, bool>(1));
        }
        let out = match agg {
            Agg::Count => AggOut::Count(rows[0].get::<_, i64>(0)),
            Agg::Exists => AggOut::Exists(rows[0].get::<_, bool>(0)),
            Agg::Sum => AggOut::Sum(rows[0].get::<_, Decimal>(0)),
            Agg::Min | Agg::Max => AggOut::Value(match rows.first() {
                Some(r) => Some(read_row(schema, r)?[col].clone()),
                None => None,
            }),
        };
        if let Some(k) = k {
            self.remember_read(&ck, k, Cached::Agg(out.clone()));
        }
        Ok(out)
    }

    fn insert(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        let _sp = span("insert");
        let row = &*schema.stored_row(row);
        let ti = self.table(company, schema)?;
        self.begin()?;
        let (t, ck) = (&ti.name, ti.ck.clone());
        self.invalidate(&ck);
        let mut q = Sql::new();
        let vals: Vec<String> = row.iter().enumerate().map(|(i, v)| q.v(schema.columns[i], v)).collect();
        q.text = format!("INSERT INTO {t} ({}) VALUES ({}) ON CONFLICT DO NOTHING", ti.cols, vals.join(", "));
        let res = match self.exec(&q) {
            Ok(0) => Err(StoreError::Duplicate),
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };
        self.after_write(&ck, schema, row, &res, false, true);
        res
    }

    fn modify(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        let _sp = span("modify");
        let row = &*schema.stored_row(row);
        let ti = self.table(company, schema)?;
        self.begin()?;
        let (t, ck) = (&ti.name, ti.ck.clone());
        self.invalidate(&ck);
        let mut q = Sql::new();
        let pk = &schema.keys[0];
        let sets: Vec<String> = (0..row.len())
            .filter(|i| !pk.contains(i))
            .map(|i| {
                let ph = q.v(schema.columns[i], &row[i]);
                format!("f{} = {ph}", schema.field_ids[i])
            })
            .collect();
        let conds: Vec<String> = pk
            .iter()
            .map(|&c| {
                let ph = q.v(schema.columns[c], &row[c]);
                format!("f{} = {ph}", schema.field_ids[c])
            })
            .collect();
        if sets.is_empty() {
            return Ok(());
        }
        // The key matches case-insensitively and the stored key stays as it is:
        // return it, so the row is only remembered when it is exactly `row`.
        let ret: Vec<String> = pk.iter().map(|&c| format!("f{}", schema.field_ids[c])).collect();
        q.text = format!("UPDATE {t} SET {} WHERE {} RETURNING {}", sets.join(", "), conds.join(" AND "), ret.join(", "));
        let mut known = false;
        let res = match self.query(&q) {
            Ok(r) if r.is_empty() => Err(StoreError::NotFound),
            Ok(r) => {
                known = pk.iter().enumerate().all(|(i, &c)| read_value(schema.columns[c], &r[0], i).is_ok_and(|v| exact_eq(&v, &row[c])));
                Ok(())
            }
            Err(e) => Err(e),
        };
        self.after_write(&ck, schema, row, &res, false, known);
        res
    }

    fn delete(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row) -> Result<(), StoreError> {
        let _sp = span("delete");
        let ti = self.table(company, schema)?;
        self.begin()?;
        let (t, ck) = (&ti.name, ti.ck.clone());
        self.invalidate(&ck);
        let mut q = Sql::new();
        let conds: Vec<String> = schema.keys[0]
            .iter()
            .map(|&c| {
                let ph = q.v(schema.columns[c], &key[c]);
                format!("f{} = {ph}", schema.field_ids[c])
            })
            .collect();
        q.text = format!("DELETE FROM {t} WHERE {}", conds.join(" AND "));
        let res = match self.exec(&q) {
            Ok(0) => Err(StoreError::NotFound),
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };
        self.after_write(&ck, schema, key, &res, true, true);
        res
    }

    fn lock_table(&mut self, company: &str, schema: &Arc<TableSchema>) -> Result<(), StoreError> {
        let ti = self.table(company, schema)?;
        self.begin()?;
        self.locked.insert(ti.ck.clone());
        Ok(())
    }

    fn is_locked(&self, company: &str, table: i64) -> bool {
        self.locked.iter().any(|(s, t)| *t == table && (s == "shared" || *s == schema_name(company, true)))
    }

    fn savepoint(&mut self) -> Result<usize, StoreError> {
        self.begin()?;
        self.sp += 1;
        let n = self.sp;
        let t0 = std::time::Instant::now();
        self.c().batch_execute(&format!("SAVEPOINT sp{n}")).map_err(db)?;
        trace("ctl", "SAVEPOINT", t0);
        Ok(n)
    }

    fn rollback_to(&mut self, sp: usize) -> Result<(), StoreError> {
        // undone writes, and row locks taken after the savepoint are released
        self.cache.clear();
        self.forget_tx();
        if self.in_txn && sp > self.sp_floor {
            self.c().batch_execute(&format!("ROLLBACK TO SAVEPOINT sp{sp}")).map_err(db)?;
            // the savepoint was taken before any failed statement (taking it would have failed)
            self.aborted = None;
        } else if self.in_txn && sp > 0 {
            // The savepoint's transaction was committed in between (`Commit(); if Codeunit.Run(..)`
            // with another Commit inside, as SendToPosting and Sales-Post do): everything in the
            // current transaction came after the savepoint, so it all goes.
            self.rollback();
        }
        Ok(())
    }

    /// RELEASE SAVEPOINT: a savepoint nests every later one, and a transaction that keeps
    /// thousands of them (al-test: a test codeunit's methods and Commit()s) makes each visibility
    /// check of its own rows walk the whole nesting.
    fn release(&mut self, sp: usize) -> Result<(), StoreError> {
        if self.in_txn && sp > self.sp_floor && self.aborted.is_none() {
            let t0 = std::time::Instant::now();
            self.c().batch_execute(&format!("RELEASE SAVEPOINT sp{sp}")).map_err(db)?;
            trace("ctl", "RELEASE SAVEPOINT", t0);
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), StoreError> {
        self.forget_tx();
        if let Some(first) = self.aborted.take().filter(|_| self.in_txn) {
            // PostgreSQL would answer COMMIT with ROLLBACK and no error: the caller would think
            // its changes were saved
            self.rollback();
            return Err(StoreError::Other(format!("The changes were not saved, because a database operation in the same transaction failed: {first}")));
        }
        if self.in_txn {
            self.in_txn = false;
            self.locked.clear();
            let t0 = std::time::Instant::now();
            let r = self.c().batch_execute("COMMIT").map_err(db);
            trace("ctl", "COMMIT", t0);
            // every session's cached reads of the tables this transaction wrote are stale now
            for ck in std::mem::take(&mut self.tx_wrote) {
                self.sh.bump(&ck);
            }
            if r.is_err() {
                self.cache.clear();
            }
            r?;
        }
        Ok(())
    }

    fn rollback(&mut self) {
        self.cache.clear();
        self.tx_wrote.clear();
        self.forget_tx();
        self.aborted = None;
        if self.in_txn {
            self.in_txn = false;
            self.locked.clear();
            let _ = self.c().batch_execute("ROLLBACK");
        }
    }

    fn in_write(&self) -> bool {
        self.in_txn
    }

    fn batch_hint(&self) -> usize {
        50
    }

    fn set_lock_timeout(&mut self, on: bool) {
        self.lock_dur = if on { None } else { Some(-1) };
        self.apply_lock_timeout();
    }

    fn lock_timeout(&self) -> bool {
        self.lock_timeout_duration() != -1
    }

    fn set_lock_timeout_duration(&mut self, secs: i64) {
        self.lock_dur = Some(if secs <= 0 { -1 } else { secs });
        self.apply_lock_timeout();
    }

    fn lock_timeout_duration(&self) -> i64 {
        self.lock_dur.unwrap_or_else(default_lock_timeout_secs)
    }

    fn flush_read_cache(&mut self, table: Option<i64>) {
        let hit = |k: &TKey| table.map_or(true, |t| k.1 == t);
        self.cache.retain(|k, _| !hit(k));
        self.tx_empty.retain(|k, _| !hit(k));
        let mut n = 0;
        self.tx_reads.retain(|k, m| {
            let keep = !hit(k);
            if keep {
                n += m.len();
            }
            keep
        });
        self.tx_reads_n = n;
        let mut rows = 0;
        for (k, m) in self.tx_rows.iter_mut() {
            if hit(k) {
                m.retain(|_, e| e.held);
            }
            rows += m.len();
        }
        self.tx_rows_n = rows;
    }

    fn seq(&mut self, op: SeqOp) -> Result<Option<i64>, StoreError> {
        fn name(n: &str) -> String {
            let s: String = n.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
            format!("shared.\"seq_{s}\"")
        }
        if let SeqOp::Exists(n) = op {
            let r = self.retry(|c| c.c().query_one("SELECT to_regclass($1) IS NOT NULL", &[&name(n)])).map_err(db);
            self.note_abort(&r);
            return Ok(Some(r?.get::<_, bool>(0) as i64));
        }
        if let SeqOp::Create(n, start, inc) = op {
            // Under a transaction lock on the sequence's name: two sessions creating the same
            // sequence in their transactions (the pool's workers inserting their first Job Queue
            // Log Entry, an AutoIncrement counter) collided on pg_class_relname_nsp_index, and the
            // loser's transaction was aborted. Now the second one waits for the first to end and
            // then finds the sequence. It stays in the session's transaction, so a Delete and
            // Insert of one sequence in one transaction (No. Series RecreateNoSeries) still work.
            let sql = format!(
                "SELECT pg_advisory_xact_lock({}); CREATE SEQUENCE IF NOT EXISTS {} START WITH {start} INCREMENT BY {inc} MINVALUE {}",
                key_hash(&format!("seq:{}", name(n))),
                name(n),
                start.min(0) - 1
            );
            let r = self.retry(|c| c.c().batch_execute(&sql)).map_err(db);
            self.note_abort(&r);
            r?;
            return Ok(None);
        }
        let c = self.c();
        Ok(match op {
            SeqOp::Exists(_) | SeqOp::Create(..) => unreachable!("handled above"),
            SeqOp::Next(n) => {
                // check first: a failing statement would abort the session's transaction
                let exists = c.query_one("SELECT to_regclass($1) IS NOT NULL", &[&name(n)]).map_err(db)?.get::<_, bool>(0);
                if !exists {
                    // own wording [src:X07]
                    return Err(StoreError::Other(format!("There is no number sequence named '{n}'.")));
                }
                Some(c.query_one(&format!("SELECT nextval('{}')", name(n).replace('\'', "''")), &[]).map_err(db)?.get::<_, i64>(0))
            }
            SeqOp::Current(n) => c
                .query_one(&format!("SELECT last_value FROM {}", name(n)), &[])
                .ok()
                .map(|r| r.get::<_, i64>(0)),
            SeqOp::Delete(n) => {
                c.batch_execute(&format!("DROP SEQUENCE IF EXISTS {}", name(n))).map_err(db)?;
                None
            }
            SeqOp::Restart(n, start) => {
                c.batch_execute(&format!("ALTER SEQUENCE {} RESTART WITH {start}", name(n))).map_err(db)?;
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    //! The session/transaction caches must never change what a read returns.
    //! Every read through the caches is checked against the same read sent
    //! straight to the database in the same transaction.
    //!
    //! Needs a scratch database: `BC_PG_TEST="host=... dbname=bc_bench ..."`.
    //! Never point it at a database with data you care about: the test creates
    //! and drops its own company schemas (`c_pgtest<pid><test>`).
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn pick(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn chance(&mut self, pct: u64) -> bool {
            self.next() % 100 < pct
        }
    }

    /// PK (code, int); name, qty, amount, status, date. Keys: PK, (status, name), (qty).
    fn schema(id: i64) -> Arc<TableSchema> {
        Arc::new(TableSchema {
            id,
            name: format!("Test {id}"),
            columns: vec![ColType::Code, ColType::Int, ColType::Text, ColType::Int, ColType::Dec, ColType::Option, ColType::Date],
            field_ids: vec![1, 2, 3, 4, 5, 6, 7],
            keys: vec![vec![0, 1], vec![5, 2], vec![3]],
            per_company: true,
        })
    }

    // "A"/"a" and "b"/"B" are the same key under the case-insensitive collation
    const CODES: [&str; 5] = ["A", "a", "B", "b", "C10"];
    const NAMES: [&str; 4] = ["x", "X", "y", ""];

    fn dec(r: &mut Rng) -> Decimal {
        let mut neg_zero = Decimal::new(0, 2);
        neg_zero.set_sign_negative(true);
        [
            Decimal::new(15, 1),
            Decimal::new(150, 2),
            Decimal::from(2),
            Decimal::ZERO,
            Decimal::new(-3, 0),
            Decimal::new(1, 6),
            Decimal::new(0, 2),
            Decimal::new(1, 20),
            Decimal::from_i128_with_scale(100_000_000_000_000_000_000, 0),
            Decimal::new(-123_456_789, 4),
        ][r.pick(10)]
        .max(if r.chance(5) { neg_zero } else { Decimal::MIN })
    }

    fn row(r: &mut Rng) -> Row {
        vec![
            Value::Text(CODES[r.pick(CODES.len())].into()),
            Value::Int(r.pick(3) as i32),
            Value::Text(NAMES[r.pick(NAMES.len())].into()),
            Value::Int(r.pick(4) as i32),
            Value::Dec(dec(r)),
            Value::Option(r.pick(3) as i32),
            Value::Date([0, 1_478_560, 1_478_561][r.pick(3)]),
        ]
    }

    fn filt(field: usize, node: FNode) -> FieldFilter {
        FieldFilter { field, node, text: String::new(), group: 0 }
    }

    fn filters(r: &mut Rng) -> Vec<FieldFilter> {
        let mut f = vec![];
        if r.chance(40) {
            f.push(filt(0, FNode::Eq(Value::Text(CODES[r.pick(CODES.len())].into()))));
        }
        if r.chance(30) {
            f.push(filt(3, FNode::Range(Some(Value::Int(r.pick(2) as i32)), Some(Value::Int(1 + r.pick(3) as i32)))));
        }
        if r.chance(25) {
            f.push(filt(5, FNode::Ne(Value::Option(r.pick(3) as i32))));
        }
        if r.chance(15) {
            f.push(filt(2, FNode::Eq(Value::Text(NAMES[r.pick(NAMES.len())].into()))));
        }
        if r.chance(10) {
            f.push(filt(2, FNode::Like("x*".chars().collect())));
        }
        f
    }

    fn same(a: &[Row], b: &[Row]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.len() == y.len() && x.iter().zip(y).all(|(u, v)| exact_eq(u, v)))
    }

    fn rows(c: &mut PgConn, s: &TableSchema, q: &Sql) -> Vec<Row> {
        c.query(q).unwrap().iter().map(|r| read_row(s, r).unwrap()).collect()
    }

    // ---- oracles: the same reads, straight to the database
    fn db_get(c: &mut PgConn, co: &str, s: &Arc<TableSchema>, key: &Row, lock: bool) -> Option<Row> {
        let ti = c.table(co, s).unwrap();
        let fu = c.for_update(&ti.ck, lock);
        let mut q = Sql::new();
        let conds: Vec<String> = s.keys[0].iter().map(|&k| format!("f{} = {}", s.field_ids[k], q.v(s.columns[k], &key[k]))).collect();
        q.text = format!("SELECT {} FROM {} WHERE {}{}", ti.cols, ti.name, conds.join(" AND "), if fu { " FOR UPDATE" } else { "" });
        rows(c, s, &q).into_iter().next()
    }

    fn db_scan(c: &mut PgConn, co: &str, s: &Arc<TableSchema>, req: &ScanReq) -> Vec<Row> {
        let ti = c.table(co, s).unwrap();
        let fu = c.for_update(&ti.ck, req.lock);
        let q = scan_sql(&ti, s, req, req.limit, fu);
        rows(c, s, &q)
    }

    fn db_count(c: &mut PgConn, co: &str, s: &Arc<TableSchema>, f: &[FieldFilter]) -> usize {
        let ti = c.table(co, s).unwrap();
        let mut q = Sql::new();
        let w = where_sql(&mut q, s, f);
        q.text = format!("SELECT count(*) FROM {}{w}", ti.name);
        c.query(&q).unwrap()[0].get::<_, i64>(0) as usize
    }

    fn agg_eq(a: &AggOut, b: &AggOut) -> bool {
        match (a, b) {
            (AggOut::Count(x), AggOut::Count(y)) => x == y,
            (AggOut::Exists(x), AggOut::Exists(y)) => x == y,
            (AggOut::Sum(x), AggOut::Sum(y)) => x.serialize() == y.serialize(),
            (AggOut::Value(x), AggOut::Value(y)) => match (x, y) {
                (Some(u), Some(v)) => exact_eq(u, v),
                (None, None) => true,
                _ => false,
            },
            _ => false,
        }
    }

    fn db_agg(c: &mut PgConn, co: &str, s: &Arc<TableSchema>, f: &[FieldFilter], col: usize, agg: Agg) -> AggOut {
        let ti = c.table(co, s).unwrap();
        let (t, cols) = (ti.name.clone(), ti.cols.clone());
        let mut q = Sql::new();
        let w = where_sql(&mut q, s, f);
        let cn = format!("f{}", s.field_ids[col]);
        q.text = match agg {
            Agg::Count => format!("SELECT count(*) FROM {t}{w}"),
            Agg::Exists => format!("SELECT EXISTS (SELECT 1 FROM {t}{w})"),
            Agg::Sum => format!("SELECT coalesce(sum({cn}), 0)::numeric FROM {t}{w}"),
            Agg::Min => format!("SELECT {cols} FROM {t}{w} ORDER BY {cn} LIMIT 1"),
            Agg::Max => format!("SELECT {cols} FROM {t}{w} ORDER BY {cn} DESC LIMIT 1"),
        };
        let r = c.query(&q).unwrap();
        match agg {
            Agg::Count => AggOut::Count(r[0].get::<_, i64>(0)),
            Agg::Exists => AggOut::Exists(r[0].get::<_, bool>(0)),
            Agg::Sum => AggOut::Sum(r[0].get::<_, Decimal>(0)),
            Agg::Min | Agg::Max => AggOut::Value(r.first().map(|x| read_row(s, x).unwrap()[col].clone())),
        }
    }

    fn backend(test: &str) -> Option<(PgBackend, String)> {
        let url = std::env::var("BC_PG_TEST").ok()?;
        let be = PgBackend::connect_url(&url).unwrap();
        let co = format!("pgtest{}{test}", std::process::id());
        be.sh.admin.lock().batch_execute(&format!("DROP SCHEMA IF EXISTS \"c_{co}\" CASCADE")).unwrap();
        Some((be, co))
    }

    fn drop_company(be: &PgBackend, co: &str) {
        be.sh.admin.lock().batch_execute(&format!("DROP SCHEMA IF EXISTS \"c_{co}\" CASCADE")).unwrap();
    }

    #[test]
    fn caches_match_database() {
        let Some((be, co)) = backend("a") else {
            eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
            return;
        };
        let tables = [schema(990001), schema(990002)];
        for seed in [0x9e37_79b9_7f4a_7c15u64, 42, 7] {
            let mut r = Rng(seed);
            let mut c = be.connect_pg().unwrap();
            let mut sps: Vec<usize> = vec![];
            for step in 0..2500 {
                // table 2 is written rarely, so it is often empty
                let s = if r.chance(25) { &tables[1] } else { &tables[0] };
                let ctx = format!("seed {seed} step {step} table {}", s.id);
                match r.pick(100) {
                    0..=11 => {
                        if s.id == 990002 && !r.chance(20) {
                            continue;
                        }
                        let _ = c.insert(&co, s, &row(&mut r));
                    }
                    12..=19 => {
                        // Modify with a key that may differ in case from the stored one
                        let x = row(&mut r);
                        let _ = c.get(&co, s, &x, false);
                        let _ = c.modify(&co, s, &x);
                    }
                    20..=23 => {
                        let x = row(&mut r);
                        let _ = c.delete(&co, s, &x);
                    }
                    24..=37 => {
                        let x = row(&mut r);
                        let lock = r.chance(20);
                        let got = c.get(&co, s, &x, lock).unwrap();
                        let want = db_get(&mut c, &co, s, &x, lock);
                        assert!(same(got.as_slice(), want.as_slice()), "{ctx}: get {x:?}\n got {got:?}\nwant {want:?}");
                    }
                    38..=69 => {
                        // FindFirst/FindLast/IsEmpty, then Next() the way the runtime does it
                        let f = filters(&mut r);
                        let (key, reverse, lock) = (r.pick(3), r.chance(30), r.chance(10));
                        let req = ScanReq { key, reverse, filters: &f, after: None, limit: [1, 1, 1, 2, 50, usize::MAX][r.pick(6)], lock };
                        let got = c.scan(&co, s, &req).unwrap();
                        let want = db_scan(&mut c, &co, s, &req);
                        assert!(same(&got, &want), "{ctx}: scan {f:?} key {key} rev {reverse} limit {}\n got {got:?}\nwant {want:?}", req.limit);
                        let mut cur = got.first().cloned();
                        let mut guard = 0;
                        while let Some(after) = cur.take() {
                            if r.chance(10) {
                                // the record changes between FindFirst and Next
                                let _ = c.modify(&co, s, &after);
                            }
                            let next = ScanReq { key, reverse, filters: &f, after: Some(&after), limit: 50, lock: false };
                            let got = c.scan(&co, s, &next).unwrap();
                            let want = db_scan(&mut c, &co, s, &next);
                            // a batch may be shorter than asked for; an empty one ends the loop
                            let ok = got.is_empty() == want.is_empty() && got.len() <= want.len() && same(&got, &want[..got.len()]);
                            assert!(ok, "{ctx}: next after {after:?}\n got {got:?}\nwant {want:?}");
                            cur = got.last().cloned();
                            guard += 1;
                            if guard > 40 || r.chance(30) {
                                break;
                            }
                        }
                    }
                    70..=77 => {
                        let f = filters(&mut r);
                        assert_eq!(c.count(&co, s, &f).unwrap(), db_count(&mut c, &co, s, &f), "{ctx}: count {f:?}");
                    }
                    78..=87 => {
                        let f = filters(&mut r);
                        let (col, agg) = [(4, Agg::Sum), (3, Agg::Min), (3, Agg::Max), (0, Agg::Count), (0, Agg::Exists), (6, Agg::Max)][r.pick(6)];
                        let got = c.aggregate(&co, s, &f, col, agg).unwrap();
                        let want = db_agg(&mut c, &co, s, &f, col, agg);
                        assert!(agg_eq(&got, &want), "{ctx}: {agg:?} col {col} {f:?}\n got {got:?}\nwant {want:?}");
                    }
                    88..=90 => c.lock_table(&co, s).unwrap(),
                    91..=93 => sps.push(c.savepoint().unwrap()),
                    94..=95 => {
                        if let Some(sp) = sps.pop() {
                            c.rollback_to(sp).unwrap();
                        }
                    }
                    96..=97 => {
                        sps.clear();
                        c.commit().unwrap();
                    }
                    _ => {
                        sps.clear();
                        c.rollback();
                    }
                }
            }
            c.commit().unwrap();
        }
        drop_company(&be, &co);
    }

    #[test]
    fn transaction_knowledge_ends_at_commit() {
        let Some((be, co)) = backend("b") else {
            eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
            return;
        };
        let s = schema(990003);
        let mut a = be.connect_pg().unwrap();
        let mut b = be.connect_pg().unwrap();
        let mut x = vec![Value::Text("K".into()), Value::Int(1), Value::Text("n".into()), Value::Int(1), Value::Dec(Decimal::ONE), Value::Option(0), Value::Date(0)];
        let all = ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: 50, lock: false };
        // empty table, learned by A
        assert!(a.scan(&co, &s, &all).unwrap().is_empty());
        assert_eq!(a.count(&co, &s, &[]).unwrap(), 0);
        a.commit().unwrap();
        // B inserts and commits; A's next transaction must see it
        b.insert(&co, &s, &x).unwrap();
        b.commit().unwrap();
        assert_eq!(a.scan(&co, &s, &all).unwrap().len(), 1);
        assert_eq!(a.count(&co, &s, &[]).unwrap(), 1);
        // A locks and reads the row (held until commit)
        a.lock_table(&co, &s).unwrap();
        assert!(a.get(&co, &s, &x, false).unwrap().is_some());
        a.commit().unwrap();
        // B changes it; A's next locked read must see the change
        x[3] = Value::Int(7);
        b.modify(&co, &s, &x).unwrap();
        b.commit().unwrap();
        a.lock_table(&co, &s).unwrap();
        assert_eq!(a.get(&co, &s, &x, false).unwrap().unwrap()[3], Value::Int(7));
        a.commit().unwrap();
        // a modify through a key in another case keeps the stored key
        let mut y = x.clone();
        y[0] = Value::Text("k".into());
        y[3] = Value::Int(9);
        a.get(&co, &s, &y, false).unwrap();
        a.modify(&co, &s, &y).unwrap();
        let got = a.get(&co, &s, &y, true).unwrap().unwrap();
        assert_eq!((got[0].clone(), got[3].clone()), (Value::Text("K".into()), Value::Int(9)));
        a.commit().unwrap();
        drop(a);
        drop(b);
        drop_company(&be, &co);
    }

    /// `Commit(); if Codeunit.Run(..)` whose codeunit commits and then fails (SendToPosting):
    /// the rollback to the savepoint undoes the work after the inner commit and keeps the rest.
    #[test]
    fn rollback_to_a_savepoint_before_a_commit() {
        let Some((be, co)) = backend("c") else {
            eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
            return;
        };
        let s = schema(990004);
        let mut a = be.connect_pg().unwrap();
        let row = |k: &str| vec![Value::Text(k.into()), Value::Int(1), Value::Text("n".into()), Value::Int(1), Value::Dec(Decimal::ONE), Value::Option(0), Value::Date(0)];
        let all = ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: 50, lock: false };
        a.insert(&co, &s, &row("A")).unwrap();
        a.commit().unwrap();
        let sp = a.savepoint().unwrap();
        a.insert(&co, &s, &row("B")).unwrap();
        a.commit().unwrap();
        a.insert(&co, &s, &row("C")).unwrap();
        a.rollback_to(sp).unwrap();
        assert!(!a.in_write());
        let keys: Vec<Value> = a.scan(&co, &s, &all).unwrap().into_iter().map(|r| r[0].clone()).collect();
        assert_eq!(keys, vec![Value::Text("A".into()), Value::Text("B".into())]);
        // a savepoint of the current transaction still rolls back to itself only
        a.insert(&co, &s, &row("D")).unwrap();
        let sp2 = a.savepoint().unwrap();
        a.insert(&co, &s, &row("E")).unwrap();
        a.rollback_to(sp2).unwrap();
        a.commit().unwrap();
        let keys: Vec<Value> = a.scan(&co, &s, &all).unwrap().into_iter().map(|r| r[0].clone()).collect();
        assert_eq!(keys, vec![Value::Text("A".into()), Value::Text("B".into()), Value::Text("D".into())]);
        drop(a);
        drop_company(&be, &co);
    }
}
