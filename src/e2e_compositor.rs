use std::{os::unix::net::UnixListener, path::PathBuf, thread, time::Duration};

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?;
    let path = PathBuf::from(runtime).join("wayland-0");
    let _ = std::fs::remove_file(&path);
    let _listener =
        UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
