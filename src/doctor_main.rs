use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("--config") => {}
        Some(argument) => bail!("expected --config, received {argument}"),
        None => bail!("missing --config"),
    }
    let config = PathBuf::from(arguments.next().context("missing configuration path")?);
    if arguments.next().is_some() {
        bail!("unexpected extra argument");
    }
    let report = alphin::doctor::inspect(&alphin::config::Config::load(&config)?)?;
    println!(
        "compositor={} greeter={} sessions={}",
        report.compositor, report.greeter, report.session_count
    );
    Ok(())
}
