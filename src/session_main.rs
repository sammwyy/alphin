use std::{os::fd::FromRawFd, os::unix::net::UnixStream};

use anyhow::{bail, Result};

fn main() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("session worker must run as root");
    }
    let stream = unsafe { UnixStream::from_raw_fd(0) };
    alphin::session_worker::run(stream)
}
