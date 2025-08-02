use libc::SIGINT;
use std::io::Error;
use std::sync::atomic::{AtomicBool, Ordering};

static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn sigint_handler(_signum: libc::c_int) {
    SIGINT_RECEIVED.store(true, Ordering::Relaxed);
}

pub(super) fn register_shutdown_flag() -> Result<&'static AtomicBool, Error> {
    if unsafe { libc::signal(SIGINT, sigint_handler as usize) } == libc::SIG_ERR {
        Err(Error::last_os_error())
    } else {
        Ok(&SIGINT_RECEIVED)
    }
}
