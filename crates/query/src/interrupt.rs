//! Ctrl-C / SIGTERM handling and cooperative cancellation for `collect`.
//!
//! The signal handler only flips an atomic flag — that is async-signal-safe. Workers check
//! the flag before each instance and skip the rest, which drops the temp dir like a normal
//! exit. A second signal exits at once. Cancellation (GUI button) sets its own flag so a
//! later SIGINT still takes the graceful path instead of looking like the second signal.

use std::sync::atomic::{AtomicBool, Ordering};

static SIGNALED: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Exit status of a process ended by SIGINT, as shells report it.
const EXIT_INTERRUPTED: libc::c_int = 130;

extern "C" fn on_signal(_sig: libc::c_int) {
    // a second signal means "now": skip the graceful path (`_exit` is async-signal-safe)
    if SIGNALED.swap(true, Ordering::SeqCst) {
        unsafe { libc::_exit(EXIT_INTERRUPTED) };
    }
}

/// Restores the handlers that were in place before [`arm`] when dropped.
pub struct InterruptGuard {
    previous: [(libc::c_int, libc::sighandler_t); 2],
}

/// Arms SIGINT/SIGTERM flagging until the guard drops.
pub fn arm() -> InterruptGuard {
    SIGNALED.store(false, Ordering::SeqCst);
    CANCELLED.store(false, Ordering::SeqCst);
    let handler = on_signal as *const () as libc::sighandler_t;
    // SAFETY: the handler only touches an atomic (and `_exit`), both async-signal-safe
    let previous =
        [libc::SIGINT, libc::SIGTERM].map(|sig| (sig, unsafe { libc::signal(sig, handler) }));
    InterruptGuard { previous }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        for (sig, handler) in self.previous {
            // SAFETY: reinstalls the handler that was there before `arm`
            unsafe { libc::signal(sig, handler) };
        }
    }
}

/// Asks the current run to stop before its next source. Kept separate from the signal
/// flag so a SIGINT arriving after a cancel is the *first* signal, not the fatal second.
pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
}

pub fn interrupted() -> bool {
    SIGNALED.load(Ordering::SeqCst) || CANCELLED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::{arm, cancel, interrupted};

    #[test]
    fn flag_starts_clear_and_arms() {
        let _guard = arm();
        assert!(!interrupted());
    }

    #[test]
    fn cancel_stops_and_rearm_clears() {
        cancel();
        assert!(interrupted());
        let _guard = arm();
        assert!(!interrupted());
    }
}
