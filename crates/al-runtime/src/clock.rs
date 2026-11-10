//! The clock and the Guid source of AL code: CurrentDateTime, Today, Time, Sleep, CreateGuid,
//! SystemId, SystemCreatedAt/SystemModifiedAt, TaskScheduler due times, .NET DateTime.Now and
//! Guid.NewGuid.
//!
//! By default these are the wall clock and random (v4) Guids, as in BC. A thread can switch to a
//! deterministic mode (al-test does, per test isolation unit, see docs/test-isolation.md): a
//! virtual clock and a seeded Guid sequence, so that two runs of the same tests give the same
//! timestamps, Guids and therefore the same outcomes. The virtual clock keeps what AL code relies
//! on: it never stands still or goes back (every reading is at least 1 ms after the previous
//! one), time passes with the work done (10 µs per AL statement: a 14,000-statement posting takes
//! 140 ms, the order of BC on SQL Server, so "modified after the last synch" style comparisons see
//! realistic gaps), and Sleep(ms) moves it on by ms without waiting. Work is counted per session:
//! bc-rust runs background sessions and due scheduled tasks inline, with their own statement
//! counters, and a session's reading only counts its own statements since its last reading. A
//! Sleep ends at its start + ms, or later when the tasks it ran took longer, so work that BC would
//! do in parallel while the session sleeps overlaps the sleep instead of adding to it (Metrics
//! Collection Test TimerMetrics brackets Sleep(1000) and allows ±500 ms). Guids stay unique and
//! random-looking: valid version 4 / RFC 4122 variant values from a SplitMix64 stream.
use std::cell::RefCell;

/// Virtual time per AL statement, in nanoseconds.
pub const NS_PER_STEP: u64 = 10_000;

struct Virtual {
    /// UTC milliseconds of the last reading.
    now: i64,
    /// Work time not yet added to `now`, in nanoseconds (below 1 ms).
    frac_ns: u64,
    /// Per session (`Session` address): its statement counter at its last reading. A session not
    /// seen yet counts from 0; a smaller counter (a new session at the same address) adds no work.
    sessions: Vec<(usize, u64)>,
    /// SplitMix64 state of the Guid sequence.
    rng: u64,
}

impl Virtual {
    fn work(&mut self, at: Option<(usize, u64)>) {
        let Some((session, steps)) = at else { return };
        match self.sessions.iter_mut().find(|(s, _)| *s == session) {
            Some(e) => {
                if steps >= e.1 {
                    let ns = self.frac_ns + (steps - e.1) * NS_PER_STEP;
                    self.now += (ns / 1_000_000) as i64;
                    self.frac_ns = ns % 1_000_000;
                }
                e.1 = steps;
            }
            None => {
                // a session seen for the first time: its own statements since it started
                let ns = self.frac_ns + steps * NS_PER_STEP;
                self.now += (ns / 1_000_000) as i64;
                self.frac_ns = ns % 1_000_000;
                // a few sessions at a time (the test's, its background sessions and tasks)
                if self.sessions.len() >= 16 {
                    self.sessions.remove(0);
                }
                self.sessions.push((session, steps));
            }
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

thread_local! {
    static VIRT: RefCell<Option<Virtual>> = const { RefCell::new(None) };
}

/// Switch this thread to the deterministic mode, or start it again: the clock stands at `start_ms`
/// (UTC milliseconds) and the Guid sequence starts from `seed`. Call it at the start of each unit
/// whose database is fresh (a test codeunit, or a test with per-test isolation): within a unit
/// the clock must keep running and the sequence must go on, or Guids repeat.
pub fn set_virtual(start_ms: i64, seed: u64) {
    VIRT.with(|v| *v.borrow_mut() = Some(Virtual { now: start_ms, frac_ns: 0, sessions: Vec::new(), rng: seed }));
}

/// Back to the wall clock and random Guids (the default).
pub fn set_wall() {
    VIRT.with(|v| *v.borrow_mut() = None);
}

pub fn is_virtual() -> bool {
    VIRT.with(|v| v.borrow().is_some())
}

/// A reading of the clock (CurrentDateTime, Time, record timestamps), UTC milliseconds. `at` is
/// the reading session (any stable identity, the `Session` address) and its statement counter
/// (`Session::steps`), None where no session is at hand.
pub fn now_ms(at: Option<(usize, u64)>) -> i64 {
    VIRT.with(|v| match &mut *v.borrow_mut() {
        None => chrono::Utc::now().timestamp_millis(),
        Some(v) => {
            v.work(at);
            v.now += 1;
            v.now
        }
    })
}

/// The `at` of a reading by `session`: its address and statement counter.
pub fn at<T>(session: &T, steps: u64) -> Option<(usize, u64)> {
    Some((session as *const T as usize, steps))
}

/// The time without a reading (Today, due times of scheduled tasks): the virtual clock does not
/// move on.
pub fn peek_ms() -> i64 {
    VIRT.with(|v| match &*v.borrow() {
        None => chrono::Utc::now().timestamp_millis(),
        Some(v) => v.now,
    })
}

/// Sleep(ms): true when the virtual clock moved on by `ms` (the caller doesn't wait), false on
/// the wall clock (the caller waits).
pub fn sleep(ms: i64) -> bool {
    sleep_from(peek_ms(), ms)
}

/// Sleep(ms) that started at `start` (`peek_ms()` before the due tasks it ran): the clock ends at
/// `start + ms` or where the tasks left it, whichever is later. True on the virtual clock.
pub fn sleep_from(start: i64, ms: i64) -> bool {
    VIRT.with(|v| match &mut *v.borrow_mut() {
        None => false,
        Some(v) => {
            v.now = v.now.max(start + ms.max(0));
            true
        }
    })
}

/// The local date and time of `ms` (UTC milliseconds), in the machine's time zone.
pub fn local(ms: i64) -> chrono::NaiveDateTime {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.naive_local(),
        chrono::LocalResult::None => chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default().naive_utc(),
    }
}

/// UTC milliseconds of a local date and time of day.
pub fn local_to_ms(t: chrono::NaiveDateTime) -> i64 {
    use chrono::TimeZone;
    match chrono::Local.from_local_datetime(&t) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.timestamp_millis(),
        chrono::LocalResult::None => t.and_utc().timestamp_millis(),
    }
}

/// A new Guid (CreateGuid, SystemId, Guid.NewGuid): random, or the next value of the seeded
/// sequence. Always version 4 with the RFC 4122 variant.
pub fn new_guid() -> [u8; 16] {
    VIRT.with(|v| match &mut *v.borrow_mut() {
        None => *uuid::Uuid::new_v4().as_bytes(),
        Some(v) => {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&v.next_u64().to_le_bytes());
            b[8..].copy_from_slice(&v.next_u64().to_le_bytes());
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            b
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_moves_on_and_repeats() {
        let run = || {
            set_virtual(1_000_000, 42);
            let a = now_ms(Some((1, 0)));
            let b = now_ms(Some((1, 0)));
            let c = now_ms(Some((1, 1_000))); // 1,000 statements = 10 ms of work
            let today = peek_ms();
            assert!(sleep(20));
            let d = now_ms(None);
            let g = (new_guid(), new_guid());
            set_wall();
            (a, b, c, today, d, g)
        };
        let (a, b, c, today, d, g) = run();
        assert_eq!((a, b, c, today, d), (1_000_001, 1_000_002, 1_000_013, 1_000_013, 1_000_034));
        assert_ne!(g.0, g.1);
        for x in [g.0, g.1] {
            let u = uuid::Uuid::from_bytes(x);
            assert_eq!(u.get_version_num(), 4);
            assert_eq!(u.get_variant(), uuid::Variant::RFC4122);
        }
        assert_eq!(run(), (a, b, c, today, d, g));
        // sessions count their own statements: a second session's reading neither adds the first
        // one's work nor makes the first one's next reading count the difference of the counters
        set_virtual(0, 1);
        assert_eq!(now_ms(Some((1, 5_000))), 50 + 1);
        assert_eq!(now_ms(Some((2, 10))), 50 + 2);
        assert_eq!(now_ms(Some((1, 5_000))), 50 + 3);
        // a Sleep overlaps the work of the tasks it ran: 2 s of task work during a Sleep(1000)
        let start = peek_ms();
        assert_eq!(now_ms(Some((3, 200_000))), 53 + 2_000 + 1);
        assert!(sleep_from(start, 1_000));
        assert_eq!(now_ms(Some((1, 5_000))), 2_055);
        // and a Sleep longer than the work ends at its start + ms
        let start = peek_ms();
        now_ms(Some((3, 210_000)));
        assert!(sleep_from(start, 1_000));
        assert_eq!(now_ms(Some((1, 5_000))), 2_055 + 1_000 + 1);
        set_wall();
        assert!(!is_virtual() && !sleep(1));
    }
}
