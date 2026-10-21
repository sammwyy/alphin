use std::{
    fs, thread,
    time::{Duration, Instant},
};

use alphin::client::Client;
use anyhow::{bail, Context, Result};

const SOCKET: &str = "/run/alphin/alphind.sock";

fn main() -> Result<()> {
    wait_for_phase("greeter", Duration::from_secs(45))?;
    let client = Client::new(SOCKET);
    require_user(&client, "coconut")?;
    require_session(&client, "alphin-e2e")?;
    client.begin_authentication("coconut")?;
    wait_for_prompt(&client, Duration::from_secs(10))?;
    client.respond("coconut".to_owned())?;
    wait_for_phase("authenticated", Duration::from_secs(10))?;
    client.start_session("alphin-e2e")?;
    kill_process("coconut-greeter")?;
    wait_for_phase("session_running", Duration::from_secs(30))?;
    kill_process("alphin-e2e-ses")?;
    wait_for_phase("greeter", Duration::from_secs(45))?;
    println!("ALPHIN_E2E_OK boot greeter pam handoff logout recovery");
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
            bail!("timed out waiting for Alphin phase {expected}");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_prompt(client: &Client, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if client.status()?.get("kind").and_then(|v| v.as_str()) == Some("prompt") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for PAM prompt");
        }
        thread::sleep(Duration::from_millis(100));
    }
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

fn kill_process(name: &str) -> Result<()> {
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
        if comm.trim() == name {
            if status.lines().any(|line| line == "State:\tZ (zombie)") {
                continue;
            }
            if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
                return Ok(());
            }
        }
    }
    bail!("process {name} is not running")
}
