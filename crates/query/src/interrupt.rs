//! Ctrl-C / SIGTERM handling for `collect`.
//!
//! The signal handler only flips an atomic flag — that is async-signal-safe. Workers check
//! the flag before each instance and skip the rest, which drops the temp dir like a normal
//! exit. A second signal exits at once.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Exit status of a process ended by SIGINT, as shells report it.
const EXIT_INTERRUPTED: libc::c_int = 130;

extern "C" fn on_signal(_sig: libc::c_int) {
    // a second signal means "now": skip the graceful path (`_exit` is async-signal-safe)
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        unsafe { libc::_exit(EXIT_INTERRUPTED) };
    }
}

/// Restores the handlers that were in place before [`arm`] when dropped.
pub struct InterruptGuard {
    previous: [(libc::c_int, libc::sighandler_t); 2],
}

/// Arms SIGINT/SIGTERM flagging until the guard drops.
pub fn arm() -> InterruptGuard {
    INTERRUPTED.store(false, Ordering::SeqCst);
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

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::{arm, interrupted};

    #[test]
    fn flag_starts_clear_and_arms() {
        let _guard = arm();
        assert!(!interrupted());
    }
}
