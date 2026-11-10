//! Memory-budgeted admission for parity runs (al-test): runs that fit in memory run at the same
//! time, in arrival order.
//!
//! Every run holds a ticket file in [`dir`] (`/tmp/bc-rust-al-test.d`, on Windows
//! `%TEMP%\bc-rust-al-test.d`): its pid, arrival number, the memory it was granted and the memory
//! it wants, in MB. A run asks for more with [`Admission::reserve`] (before it loads a catalog,
//! before it starts its workers) and gives memory back the same way. A request waits until
//! - no run that arrived earlier is waiting (first come, first served), and
//! - the growth fits: MemAvailable, minus a safety margin of at least 6 GB
//!   (`AL_TEST_MEM_MARGIN_MB` raises it), minus the part of every run's grant it does not use yet
//!   (grant minus its current RSS, as MemAvailable already counts what runs use).
//!
//! When no other run is running (holding a grant and not waiting) the oldest request is admitted
//! even if it does not fit: nothing would free memory for it.
//!
//! Limited backfill: a later request may go ahead of earlier waiters when it fits now and is small
//! (at most [`BACKFILL_MAX_MB`] more at its current stage), but only while the oldest waiter (the
//! head) has waited less than [`BACKFILL_HEAD_WAIT_S`]; after that nothing goes ahead of the head
//! until it is granted, so big runs are not starved. A head whose ticket does not say since when it
//! waits (binaries from before this rule) gets no backfill around it. Backfills are logged.
//!
//! Each run keeps its own ticket file locked, so a ticket whose lock can be taken belongs to a
//! dead process and is removed. Ticket updates are serialized by a short lock on
//! `<dir>/admission.lock`. Admitted runs also hold a shared lock on the old exclusive lock file
//! (`/tmp/bc-rust-al-test.lock`), so al-test binaries from before this scheme, which lock it
//! exclusively for their whole run, never run beside new ones.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The minimum safety margin kept free (MB).
pub const MIN_MARGIN_MB: u64 = 6 * 1024;

/// Backfill: the most a request ahead of earlier waiters may grow by (MB).
pub const BACKFILL_MAX_MB: u64 = 6 * 1024;

/// Backfill stops once the oldest waiter has waited this long (seconds).
pub const BACKFILL_HEAD_WAIT_S: u64 = 15 * 60;

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The legacy exclusive lock file (still taken shared by admitted runs).
pub fn legacy_lock_path() -> PathBuf {
    if let Some(d) = std::env::var_os("AL_TEST_ADMISSION_DIR") {
        return PathBuf::from(d).join("legacy.lock");
    }
    if cfg!(unix) {
        PathBuf::from("/tmp/bc-rust-al-test.lock")
    } else {
        std::env::temp_dir().join("bc-rust-al-test.lock")
    }
}

/// The directory of the tickets (`AL_TEST_ADMISSION_DIR` overrides it, and then the legacy lock
/// file is `legacy.lock` in it: a private queue for testing).
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("AL_TEST_ADMISSION_DIR") {
        return PathBuf::from(d);
    }
    if cfg!(unix) {
        PathBuf::from("/tmp/bc-rust-al-test.d")
    } else {
        std::env::temp_dir().join("bc-rust-al-test.d")
    }
}

/// The safety margin in MB: `AL_TEST_MEM_MARGIN_MB`, at least [`MIN_MARGIN_MB`].
pub fn margin_mb() -> u64 {
    std::env::var("AL_TEST_MEM_MARGIN_MB").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0).max(MIN_MARGIN_MB)
}

/// The margin while `holders` other runs hold memory: [`margin_mb`] plus 1 GB per run beyond two,
/// at most [`MAX_EXTRA_MARGIN_MB`] more (the memory guard handles what is left of an overshoot).
/// Every running run can outgrow its estimate a little before its grant catches up; with many
/// runs those excesses add up (two OOM kills on 2026-11-08 with 5-6 runs and the flat 6 GB).
pub fn margin_for(holders: usize) -> u64 {
    margin_with(margin_mb(), holders)
}

/// The most [`margin_for`] adds to the base margin (MB).
pub const MAX_EXTRA_MARGIN_MB: u64 = 4096;

fn margin_with(base: u64, holders: usize) -> u64 {
    base + (1024 * holders.saturating_sub(2) as u64).min(MAX_EXTRA_MARGIN_MB)
}

/// The memory guard's decision (`Admission::grow_or_pause`): may the run with ticket `seq` take
/// new work, given the other runs that hold memory as (seq, unused MB), MemAvailable and the
/// base margin? Yes while MemAvailable stays above the margin after their unused reservations,
/// and always for the oldest of them (so that two runs over their grants can't wait for each
/// other); also when memory is unknown.
/// Memory pressure that MemAvailable alone doesn't show (host OOM 2026-11-08 14:41 UTC: swap 8/8 GB
/// full, 1 GB available, about ten al-test runs holding their catalogs while waiting for workers,
/// plus rustc builds, scratch servers and agents outside admission): PSI memory stall time, "full
/// avg10" at 5 % or more or "some avg10" at 20 % or more. While there is pressure nothing new is
/// admitted and runs take no new work (the oldest holder still goes on, so runs never wait for each
/// other).
pub fn memory_pressure() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let psi = std::fs::read_to_string("/proc/pressure/memory").ok();
        pressure_reason(&meminfo, psi.as_deref())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn pressure_reason(_meminfo: &str, psi: Option<&str>) -> Option<String> {
    // swap occupancy alone says nothing: swapped-out pages stay out after the pressure has gone
    // (8 of 8 GB still used at 14:47 UTC with 15 GB available and no stalls), so stall time decides
    let avg10 = |kind: &str| psi.and_then(|p| p.lines().find(|l| l.starts_with(kind)).and_then(|l| l.split_whitespace().find_map(|f| f.strip_prefix("avg10="))).and_then(|v| v.parse::<f64>().ok()));
    let (some, full) = (avg10("some").unwrap_or(0.0), avg10("full").unwrap_or(0.0));
    (full >= 5.0 || some >= 20.0).then(|| format!("memory pressure (PSI avg10 some {some:.1} %, full {full:.1} %)"))
}

fn may_continue(seq: u64, holders: &[(u64, u64)], avail_mb: Option<u64>, margin_base: u64) -> bool {
    let oldest = holders.iter().all(|&(s, _)| s > seq);
    let pending: u64 = holders.iter().map(|&(_, u)| u).sum();
    let margin = margin_with(margin_base, holders.len());
    oldest || avail_mb.map_or(true, |a| a as i64 - margin as i64 - pending as i64 > 0)
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn pressure_from_psi() {
        let mi = "MemTotal: 64000000 kB\nSwapTotal: 8388604 kB\nSwapFree: 3588 kB\n";
        let psi = |some: &str, full: &str| format!("some avg10={some} avg60=0.50 avg300=0.10 total=1\nfull avg10={full} avg60=0.20 avg300=0.05 total=1\n");
        // a full swap without stalls (as at 14:47 UTC) is no pressure
        assert!(pressure_reason(mi, Some(&psi("0.00", "0.00"))).is_none());
        assert!(pressure_reason(mi, None).is_none());
        assert!(pressure_reason(mi, Some(&psi("4.00", "0.40"))).is_none());
        assert!(pressure_reason(mi, Some(&psi("8.00", "7.25"))).is_some());
        assert!(pressure_reason(mi, Some(&psi("25.00", "1.00"))).is_some());
    }

    #[test]
    fn memory_guard_decision() {
        // alone: always go on, even with nothing available
        assert!(may_continue(5, &[], Some(0), 6144));
        // an older run holds memory: go on only while above the margin plus its unused grant
        assert!(may_continue(5, &[(3, 1000)], Some(8000), 6144));
        assert!(!may_continue(5, &[(3, 1000)], Some(7000), 6144));
        // the oldest run always goes on
        assert!(may_continue(2, &[(3, 1000)], Some(0), 6144));
        // the margin grows by 1 GB per holder beyond two: 3 holders -> 7 GB
        assert!(may_continue(9, &[(1, 0), (2, 0), (3, 0)], Some(7200), 6144));
        assert!(!may_continue(9, &[(1, 0), (2, 0), (3, 0)], Some(7100), 6144));
        assert_eq!(margin_with(6144, 0), 6144);
        assert_eq!(margin_with(6144, 6), 6144 + 4096);
        // the extra is capped at 4 GB: 10 holders give 10 GB, not 14
        assert_eq!(margin_with(6144, 10), 6144 + MAX_EXTRA_MARGIN_MB);
        assert_eq!(margin_with(6144, 40), 6144 + 4096);
        // memory unknown: don't hold work back
        assert!(may_continue(9, &[(1, 0)], None, 6144));
    }
}

/// MemAvailable from /proc/meminfo, in MB (None where there is no /proc, e.g. macOS).
#[cfg(not(windows))]
pub fn mem_available_mb() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// Windows: available physical memory (free + standby list, like MemAvailable) from
/// GlobalMemoryStatusEx, in MB.
#[cfg(windows)]
pub fn mem_available_mb() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut m = MemoryStatusEx { length: std::mem::size_of::<MemoryStatusEx>() as u32, memory_load: 0, total_phys: 0, avail_phys: 0, total_page_file: 0, avail_page_file: 0, total_virtual: 0, avail_virtual: 0, avail_extended_virtual: 0 };
    // SAFETY: m is a valid MEMORYSTATUSEX with dwLength set, as the API requires
    (unsafe { GlobalMemoryStatusEx(&mut m) } != 0).then_some(m.avail_phys / (1024 * 1024))
}

/// Resident memory of a process in MB (Linux; None elsewhere).
pub fn rss_mb(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096 / (1024 * 1024))
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Ticket {
    pid: u32,
    seq: u64,
    granted_mb: u64,
    want_mb: u64,
    desc: String,
    since: String,
    /// Unix time the current wait began (0: not waiting, or a binary from before backfill).
    #[serde(default)]
    wait_since: u64,
}

/// A run's place in the admission queue; dropping it leaves the queue.
pub struct Admission {
    path: PathBuf,
    file: File,
    t: Ticket,
    legacy: Option<File>,
}

fn open_rw(p: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(p)
}

/// The admission lock (short critical sections only).
fn with_mutex<T>(d: &Path, f: impl FnOnce() -> T) -> T {
    let m = open_rw(&d.join("admission.lock"));
    if let Ok(m) = &m {
        let _ = m.lock();
    }
    let r = f();
    if let Ok(m) = &m {
        let _ = m.unlock();
    }
    r
}

/// All live tickets other than `own`, oldest first; tickets of dead processes are removed.
fn live_tickets(d: &Path, own: &Path) -> Vec<Ticket> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(d) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "ticket") || p == own {
            continue;
        }
        // a ticket whose lock can be taken has no owner any more
        if let Ok(f) = File::open(&p) {
            if f.try_lock().is_ok() {
                let _ = f.unlock();
                drop(f);
                let _ = std::fs::remove_file(&p);
                continue;
            }
        }
        if let Some(t) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Ticket>(&s).ok()) {
            out.push(t);
        }
    }
    out.sort_by_key(|t| t.seq);
    out
}

fn gb(mb: u64) -> String {
    format!("{:.1} GB", mb as f64 / 1024.0)
}

impl Admission {
    /// Join the queue (nothing granted yet). `desc` says what the run is (shown to waiting runs).
    /// None when the ticket directory cannot be used (the run then goes ahead unchecked).
    pub fn join(desc: &str) -> Option<Admission> {
        let d = dir();
        if let Err(e) = std::fs::create_dir_all(&d) {
            eprintln!("warning: cannot create {}: {e}; running without memory admission", d.display());
            return None;
        }
        let pid = std::process::id();
        with_mutex(&d, || {
            let mut seq = 0u64;
            if let Ok(mut f) = open_rw(&d.join("seq")) {
                let mut s = String::new();
                let _ = f.read_to_string(&mut s);
                seq = s.trim().parse::<u64>().unwrap_or(0) + 1;
                let _ = f.set_len(0);
                let _ = f.seek(SeekFrom::Start(0));
                let _ = write!(f, "{seq}");
            }
            let path = d.join(format!("{seq:012}-{pid}.ticket"));
            let file = open_rw(&path).ok()?;
            file.try_lock().ok()?;
            let t = Ticket { pid, seq, granted_mb: 0, want_mb: 0, desc: desc.chars().take(200).collect(), since: chrono::Local::now().format("%H:%M:%S").to_string(), wait_since: 0 };
            let mut a = Admission { path, file, t, legacy: None };
            a.write();
            Some(a)
        })
    }

    fn write(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = self.file.write_all(serde_json::to_string(&self.t).unwrap_or_default().as_bytes());
        let _ = self.file.flush();
    }

    /// Set this run's grant to `mb` at once, up or down, without waiting: the run's own estimate
    /// of what it holds now (its company is built, its workers have reached their working set),
    /// so that other runs are admitted against the real figure.
    pub fn adjust(&mut self, mb: u64) {
        let d = dir();
        with_mutex(&d, || {
            self.t.granted_mb = mb;
            self.t.want_mb = mb;
            self.t.wait_since = 0;
            self.write();
        });
    }

    /// Memory granted to this run (MB).
    pub fn granted_mb(&self) -> u64 {
        self.t.granted_mb
    }

    /// Memory this run could have if it were alone: what is available now plus what the other
    /// runs use (they finish eventually) and its own unused grant, minus the margin (MB; None
    /// when memory is unknown).
    pub fn capacity_mb(&self) -> Option<u64> {
        let avail = mem_available_mb()?;
        let d = dir();
        let others: u64 = with_mutex(&d, || live_tickets(&d, &self.path)).iter().map(|t| rss_mb(t.pid).unwrap_or(0)).sum();
        let own_unused = self.t.granted_mb.saturating_sub(rss_mb(self.t.pid).unwrap_or(0));
        Some((avail + others + own_unused).saturating_sub(margin_mb()))
    }

    /// For a run that needs more than its grant (its RSS plus headroom, `target_mb`): record the
    /// new figure (other runs are admitted against it) and say whether the run may go on taking
    /// new work. It may while MemAvailable stays above the margin after the other runs' unused
    /// reservations; the oldest run that holds memory always may, so that two runs over their
    /// grants never wait for each other.
    pub fn grow_or_pause(&mut self, target_mb: u64) -> bool {
        let d = dir();
        with_mutex(&d, || {
            let others = live_tickets(&d, &self.path);
            let holders: Vec<(u64, u64)> = others.iter().filter(|t| t.granted_mb > 0).map(|t| (t.seq, t.granted_mb.saturating_sub(rss_mb(t.pid).unwrap_or(0)))).collect();
            self.t.granted_mb = self.t.granted_mb.max(target_mb);
            self.t.want_mb = self.t.granted_mb;
            self.t.wait_since = 0;
            self.write();
            let oldest = holders.iter().all(|&(s, _)| s > self.t.seq);
            may_continue(self.t.seq, &holders, mem_available_mb(), margin_mb()) && (oldest || memory_pressure().is_none())
        })
    }

    /// Hold `want_mb` in total from now on: less is granted at once, more waits for its turn and
    /// for the memory (see the module docs). `what` names the stage in the waiting messages.
    pub fn reserve(&mut self, want_mb: u64, what: &str) {
        let d = dir();
        if want_mb <= self.t.granted_mb {
            with_mutex(&d, || {
                self.t.granted_mb = want_mb;
                self.t.want_mb = want_mb;
                self.write();
            });
            return;
        }
        let t0 = Instant::now();
        let mut said: Option<Instant> = None;
        loop {
            let (admit, msg) = with_mutex(&d, || {
                self.t.want_mb = want_mb;
                if self.t.wait_since == 0 {
                    self.t.wait_since = now_s();
                }
                self.write();
                let others = live_tickets(&d, &self.path);
                let earlier: Vec<&Ticket> = others.iter().filter(|t| t.want_mb > t.granted_mb && t.seq < self.t.seq).collect();
                // any run that holds memory counts, also one waiting for more (its catalog is loaded):
                // admitting past the fit is only for a host where no run holds anything
                let running = others.iter().any(|t| t.granted_mb > 0);
                let pressure = memory_pressure();
                let margin = margin_for(others.iter().filter(|t| t.granted_mb > 0).count());
                let unused = |t: &Ticket| t.granted_mb.saturating_sub(rss_mb(t.pid).unwrap_or(0));
                let pending: u64 = others.iter().map(unused).sum::<u64>() + unused(&self.t);
                let grow = want_mb - self.t.granted_mb;
                let (fits, free) = match mem_available_mb() {
                    Some(avail) => {
                        let free = avail.saturating_sub(margin).saturating_sub(pending);
                        (grow <= free && pressure.is_none(), Some(free))
                    }
                    // memory unknown: one run at a time
                    None => (!running, None),
                };
                // limited backfill around a head that has not waited long
                let head_wait = earlier.first().map(|h| if h.wait_since == 0 { u64::MAX } else { now_s().saturating_sub(h.wait_since) });
                // backfill only completes a run that already holds memory: catalog grants backfilled
                // ahead of a waiting head piled up (about ten runs holding 2.45 GB each, waiting for
                // workers, before the 14:41 OOM)
                let backfill = !earlier.is_empty() && fits && self.t.granted_mb > 0 && grow <= BACKFILL_MAX_MB && head_wait.is_some_and(|w| w < BACKFILL_HEAD_WAIT_S);
                if (earlier.is_empty() && (fits || (!running && pressure.is_none()))) || backfill {
                    if !fits {
                        eprintln!("warning: admitting {what} ({} more) although only {} is free: no other run holds memory", gb(grow), free.map(gb).unwrap_or_else(|| "?".into()));
                    }
                    if backfill {
                        eprintln!(
                            "backfill: admitted {what} ({} more) ahead of {} waiting PIDs {}; the head has waited {} s (backfill stops at {} s)",
                            gb(grow),
                            earlier.len(),
                            earlier.iter().map(|t| t.pid.to_string()).collect::<Vec<_>>().join(", "),
                            head_wait.unwrap_or(0),
                            BACKFILL_HEAD_WAIT_S
                        );
                    }
                    self.t.granted_mb = want_mb;
                    self.t.wait_since = 0;
                    self.write();
                    return (true, String::new());
                }
                let held: Vec<String> = others.iter().filter(|t| t.granted_mb > 0).map(|t| format!("{} ({}: {})", t.pid, gb(t.granted_mb), t.desc.chars().take(70).collect::<String>())).collect();
                let mut msg = format!(
                    "waiting ({:.0} s) for {what}: {} more needed, {} free after the {} margin{}; {} reserved by PIDs {}",
                    t0.elapsed().as_secs_f64(),
                    gb(grow),
                    free.map(gb).unwrap_or_else(|| "?".into()),
                    gb(margin),
                    pressure.as_ref().map(|p| format!(", stalled by {p}")).unwrap_or_default(),
                    gb(others.iter().map(|t| t.granted_mb).sum()),
                    if held.is_empty() { "-".to_string() } else { held.join(", ") }
                );
                if !earlier.is_empty() {
                    msg.push_str(&format!("; queued behind PIDs {}", earlier.iter().map(|t| t.pid.to_string()).collect::<Vec<_>>().join(", ")));
                    if let Some(w) = head_wait.filter(|&w| w != u64::MAX) {
                        msg.push_str(&format!(" (head waiting {w} s; backfill {})", if w < BACKFILL_HEAD_WAIT_S { "possible for <= 6 GB" } else { "stopped" }));
                    }
                }
                (false, msg)
            });
            if admit {
                if said.is_some() {
                    eprintln!("admitted ({what}, {} reserved) after {:.0} s", gb(want_mb), t0.elapsed().as_secs_f64());
                }
                break;
            }
            if said.is_none_or(|t| t.elapsed().as_secs() >= 60) {
                eprintln!("{msg}");
                said = Some(Instant::now());
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        self.hold_legacy();
    }

    /// Admitted runs share the legacy lock file, so older al-test binaries (exclusive lock for a
    /// whole run) wait for them, and they wait for an older binary's run.
    fn hold_legacy(&mut self) {
        if self.legacy.is_some() {
            return;
        }
        let p = legacy_lock_path();
        let Ok(f) = open_rw(&p) else { return };
        match f.try_lock_shared() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                let t0 = Instant::now();
                let holder = std::fs::read_to_string(&p).unwrap_or_default();
                eprintln!("waiting for an older al-test binary holding {}: {}", p.display(), holder.trim());
                // block (not poll): the kernel hands the lock to a blocked waiter on release,
                // ahead of older binaries that poll for it, which would otherwise starve this run
                if f.lock_shared().is_err() {
                    return;
                }
                eprintln!("older binaries done after {:.0} s", t0.elapsed().as_secs_f64());
            }
            Err(_) => return,
        }
        self.legacy = Some(f);
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let d = dir();
        with_mutex(&d, || {
            let _ = std::fs::remove_file(&self.path);
        });
        let _ = self.file.unlock();
        if let Some(l) = &self.legacy {
            let _ = l.unlock();
        }
    }
}
