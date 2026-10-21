use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let socket = option_path(&mut arguments, "--socket")?;
    let sessions = option_path(&mut arguments, "--sessions")?;
    if arguments.next().is_some() {
        bail!("unexpected extra argument");
    }
    alphin::daemon::serve(&socket, &sessions)
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
