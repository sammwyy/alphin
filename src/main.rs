use std::{os::fd::RawFd, path::PathBuf};

use alphin::{adapter::Adapter, worker};
use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let compositor = read_path(&mut arguments, "--compositor")?;
    let greeter = read_path(&mut arguments, "--greeter")?;
    let mut runtime = None;
    let mut control_fd = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--runtime-dir" => {
                runtime = Some(PathBuf::from(
                    arguments.next().context("missing runtime directory")?,
                ))
            }
            "--control-fd" => {
                control_fd = Some(
                    arguments
                        .next()
                        .context("missing control file descriptor")?
                        .parse::<RawFd>()
                        .context("invalid control file descriptor")?,
                )
            }
            _ => bail!("unknown argument: {argument}"),
        }
    }
    if let Some(control_fd) = control_fd {
        worker::protect_control_fd(control_fd)?;
    }
    let result = worker::run(
        &Adapter::load(&compositor)?,
        &Adapter::load(&greeter)?,
        &worker::runtime_dir(runtime)?,
    );
    if result.is_ok() {
        if let Some(control_fd) = control_fd {
            worker::notify_control(control_fd)?;
        }
    }
    result
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
