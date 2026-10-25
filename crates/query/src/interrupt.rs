//! Ctrl-C / SIGTERM handling for `collect`.
//!
//! The signal handler only flips an atomic flag — that is async-signal-safe. The
//! collection loop checks the flag between instances and aborts gracefully, which
//! drops the temp dir like a normal exit.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub struct InterruptGuard;

/// Arms SIGINT/SIGTERM flagging. The previous handlers are not restored (the process
/// exits right after an interrupt anyway).
pub fn arm() -> InterruptGuard {
    INTERRUPTED.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    InterruptGuard
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
