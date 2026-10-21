use std::path::PathBuf;

use alphin::{adapter::Adapter, worker};
use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let compositor = read_path(&mut arguments, "--compositor")?;
    let greeter = read_path(&mut arguments, "--greeter")?;
    let runtime = match arguments.next().as_deref() {
        None => None,
        Some("--runtime-dir") => Some(PathBuf::from(
            arguments.next().context("missing runtime directory")?,
        )),
        Some(argument) => bail!("unknown argument: {argument}"),
    };
    if arguments.next().is_some() {
        bail!("unexpected extra argument");
    }
    worker::run(
        &Adapter::load(&compositor)?,
        &Adapter::load(&greeter)?,
        &worker::runtime_dir(runtime)?,
    )
}

fn read_path(arguments: &mut impl Iterator<Item = String>, expected: &str) -> Result<PathBuf> {
    match arguments.next().as_deref() {
        Some(argument) if argument == expected => Ok(PathBuf::from(
            arguments.next().context("missing adapter path")?,
        )),
        Some(argument) => bail!("expected {expected}, received {argument}"),
        None => bail!("missing {expected}"),
    }
}
