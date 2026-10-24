mod session;
mod setup;

use std::process;

fn main() {
    // A client disconnect must not kill pid 1.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    if let Err(error) = run() {
        eprintln!("rtftp-storekeeper: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let root = setup::prepare()?;
    session::listen(root)
}
