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
    match activated_socket_fd()? {
        Some(socket_fd) => alphin::daemon::serve_activated(
            socket_fd,
            &config.socket,
            &config.sessions,
            config.users,
            Some(worker),
        ),
        None => alphin::daemon::serve(&config.socket, &config.sessions, config.users, Some(worker)),
    }
}

fn activated_socket_fd() -> Result<Option<i32>> {
    activated_socket_fd_from_values(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    )
}

fn activated_socket_fd_from_values(
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    daemon_pid: u32,
) -> Result<Option<i32>> {
    let Some(listen_pid) = listen_pid else {
        return Ok(None);
    };
    let received_pid: u32 = listen_pid.parse().context("invalid LISTEN_PID")?;
    if received_pid != daemon_pid {
        bail!("LISTEN_PID does not match daemon process");
    }
    let descriptors: u32 = listen_fds
        .context("LISTEN_PID set without LISTEN_FDS")?
        .parse()
        .context("invalid LISTEN_FDS")?;
    if descriptors != 1 {
        bail!("exactly one activated socket is required");
    }
    Ok(Some(3))
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

#[cfg(test)]
mod tests {
    use super::activated_socket_fd_from_values;

    #[test]
    fn accepts_one_socket_for_the_current_process() {
        assert_eq!(
            activated_socket_fd_from_values(Some("42"), Some("1"), 42).unwrap(),
            Some(3)
        );
    }

    #[test]
    fn rejects_foreign_or_multiple_activated_sockets() {
        assert!(activated_socket_fd_from_values(Some("41"), Some("1"), 42).is_err());
        assert!(activated_socket_fd_from_values(Some("42"), Some("2"), 42).is_err());
    }
}
