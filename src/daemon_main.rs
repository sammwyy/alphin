use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let config = option_path(&mut arguments, "--config")?;
    if arguments.next().is_some() {
        bail!("unexpected extra argument");
    }
    let config = alphin::config::Config::load_system(&config)?;
    let account = alphin::identity::lookup("alphin")?;
    let worker = alphin::daemon::WorkerMonitor::spawn(
        &config.worker,
        &config.compositor,
        &config.greeter,
        &config.runtime_dir,
        &account,
    )?;
    alphin::daemon::serve(&config.socket, &config.sessions, Some(worker))
}

fn option_path(arguments: &mut impl Iterator<Item = String>, expected: &str) -> Result<PathBuf> {
    match arguments.next().as_deref() {
        Some(value) if value == expected => {
            Ok(PathBuf::from(arguments.next().context("missing path")?))
        }
        Some(value) => bail!("expected {expected}, received {value}"),
        None => bail!("missing {expected}"),
    }
}
