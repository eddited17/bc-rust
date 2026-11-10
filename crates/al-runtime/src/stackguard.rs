//! Native stack guard. The interpreter recurses on the native stack (per AL call, statement and
//! expression), so deep AL recursion uses native stack in proportion. A thread that runs AL
//! registers its stack with `register(size)`. `Interp::invoke` then fails with "Maximum call depth
//! exceeded" once three quarters of the stack are used, so the limit always fires before an
//! overflow, whatever a call level costs on the path taken. Unregistered threads are not checked.
//! `deepest()` reports the most stack a registered thread used (al-test prints it).

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    /// (stack address at registration, lowest address allowed); (0, 0) when unregistered
    static BOUNDS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}
static DEEPEST: AtomicUsize = AtomicUsize::new(0);
static DEEPEST_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// The current stack address (stacks grow down on every target bc-rust builds for).
#[inline(always)]
fn sp() -> usize {
    let x = 0u8;
    std::hint::black_box(&x) as *const u8 as usize
}

/// Register this thread's stack of `size` bytes; call first thing in the thread.
pub fn register(size: usize) {
    let top = sp();
    BOUNDS.with(|b| b.set((top, top.saturating_sub(size - size / 4))));
}

/// True when this thread has used more than three quarters of its registered stack. Records the
/// deepest use and the AL call depth it happened at.
#[inline]
pub fn exhausted(depth: usize) -> bool {
    let (top, low) = BOUNDS.with(|b| b.get());
    if top == 0 {
        return false;
    }
    let here = sp();
    let used = top.saturating_sub(here);
    if used > DEEPEST.load(Ordering::Relaxed) && DEEPEST.fetch_max(used, Ordering::Relaxed) < used {
        DEEPEST_DEPTH.store(depth, Ordering::Relaxed);
    }
    here < low
}

/// The most stack a registered thread used so far (bytes), and the AL call depth at that point.
pub fn deepest() -> (usize, usize) {
    (DEEPEST.load(Ordering::Relaxed), DEEPEST_DEPTH.load(Ordering::Relaxed))
}

thread_local! {
    /// al-test's per-test memory cap: set while this thread's test has to stop.
    static ABORT: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

/// Register this thread's abort flag (al-test's per-test memory cap). While it is set, every
/// 1,024th AL statement fails with "Memory limit exceeded"; it stays set until the owner clears
/// it, so asserterror or a TryFunction can't swallow it for good.
pub fn register_abort(flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    ABORT.with(|a| *a.borrow_mut() = Some(flag));
}

/// True while this thread's abort flag is set (see `register_abort`).
#[inline]
pub fn abort_requested() -> bool {
    ABORT.with(|a| a.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recurse(n: usize) -> usize {
        let pad = std::hint::black_box([0u8; 4096]);
        if exhausted(n) {
            return n;
        }
        recurse(n + 1) + pad[0] as usize
    }

    #[test]
    fn fires_before_the_stack_runs_out() {
        // 8 MB stack, about 4 KB per level: the guard stops at about 6 MB, without an overflow
        let n = std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn(|| {
                register(8 << 20);
                recurse(1)
            })
            .unwrap()
            .join()
            .unwrap();
        assert!((1_000..2_048).contains(&n), "{n}");
        assert!(deepest().0 >= 6 << 20);
        // unregistered threads are not checked
        assert!(!std::thread::spawn(|| exhausted(1)).join().unwrap());
    }
}
