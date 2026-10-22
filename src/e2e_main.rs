use std::{
    fs, thread,
    time::{Duration, Instant},
};

use alphin::client::Client;
use anyhow::{bail, Context, Result};

const SOCKET: &str = "/run/alphin/alphind.sock";
const SESSION_UID: u32 = 1000;

fn main() -> Result<()> {
    wait_for_phase("greeter", Duration::from_secs(45))?;
    let client = Client::new(SOCKET);
    require_user(&client, "coconut")?;
    require_session(&client, "coconut")?;
    wait_for_phase("session_running", Duration::from_secs(45))?;
    require_process("blair", SESSION_UID)?;
    kill_process("blair", SESSION_UID)?;
    wait_for_phase("greeter", Duration::from_secs(45))?;
    require_process("coconut-greete", account_uid("alphin")?)?;
    println!("ALPHIN_E2E_OK graphical greeter pam handoff coconut logout recovery");
    Ok(())
}

fn wait_for_phase(expected: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(state) = Client::new(SOCKET).state() {
            if state["phase"] == expected {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for Alphin phase {expected}: {}",
                greeter_log()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn greeter_log() -> String {
    fs::read_to_string("/run/alphin/state/blair/compositor.log")
        .map(|log| log.lines().rev().take(20).collect::<Vec<_>>().join("\n"))
        .unwrap_or_else(|error| format!("no Blair log: {error}"))
}

fn require_user(client: &Client, username: &str) -> Result<()> {
    let users = client.users()?;
    if users["users"]
        .as_array()
        .is_some_and(|users| users.iter().any(|user| user["username"] == username))
    {
        Ok(())
    } else {
        bail!("expected user {username} is absent from greeter catalogue")
    }
}

fn require_session(client: &Client, id: &str) -> Result<()> {
    let sessions = client.sessions()?;
    if sessions["sessions"]
        .as_array()
        .is_some_and(|sessions| sessions.iter().any(|session| session["id"] == id))
    {
        Ok(())
    } else {
        bail!("expected session {id} is absent from greeter catalogue")
    }
}

fn require_process(name: &str, uid: u32) -> Result<()> {
    process_pid(name, uid).map(|_| ())
}

fn kill_process(name: &str, uid: u32) -> Result<()> {
    let pid = process_pid(name, uid)?;
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("terminate desktop session")
    }
}

fn process_pid(name: &str, uid: u32) -> Result<i32> {
    for entry in fs::read_dir("/proc").context("read process table")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(comm) = fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        if comm.trim() == name && process_uid(&status) == Some(uid) {
            return Ok(pid);
        }
    }
    bail!("process {name} for uid {uid} is not running")
}

fn account_uid(username: &str) -> Result<u32> {
    fs::read_to_string("/etc/passwd")?
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next()? == username).then(|| fields.nth(1)?.parse().ok())?
        })
        .context("read account uid")
}

fn process_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:\t"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_process_owner_from_proc_status() {
        assert_eq!(
            process_uid("Name:\tblair\nUid:\t1000\t1000\t1000\t1000\n"),
            Some(1000)
        );
        assert_eq!(process_uid("Name:\tblair\n"), None);
    }
}
