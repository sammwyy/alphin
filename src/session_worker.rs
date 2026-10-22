use std::{
    io::{BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc,
    thread,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    identity::{self, Account},
    pam::{self, Secret},
};

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    Begin {
        username: String,
    },
    Prompt {
        message: String,
        echo: bool,
    },
    Succeeded,
    Failed,
    Launch {
        command: Vec<String>,
        account: Account,
        runtime_dir: PathBuf,
    },
    SessionStarted,
    SessionFailed,
    SessionExited,
}

pub fn send(stream: &mut UnixStream, message: &Message) -> Result<()> {
    serde_json::to_writer(&mut *stream, message).context("write session worker message")?;
    stream
        .write_all(b"\n")
        .context("finish session worker message")
}

pub fn receive(reader: &mut BufReader<UnixStream>) -> Result<Message> {
    let mut line = String::new();
    if reader
        .read_line(&mut line)
        .context("read session worker message")?
        == 0
    {
        bail!("session worker disconnected");
    }
    serde_json::from_str(&line).context("parse session worker message")
}

pub fn send_secret(stream: &mut UnixStream, secret: &Secret) -> Result<()> {
    let length = u32::try_from(secret.as_bytes().len()).context("PAM response is too long")?;
    stream.write_all(&length.to_be_bytes())?;
    stream
        .write_all(secret.as_bytes())
        .context("write PAM response")
}

fn receive_secret(reader: &mut BufReader<UnixStream>) -> Result<Secret> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 1024 * 1024 {
        bail!("PAM response is too long");
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    reader.read_exact(&mut bytes)?;
    Secret::from_bytes(std::mem::take(&mut *bytes))
}

pub fn run(mut stream: UnixStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let Message::Begin { username } = receive(&mut reader)? else {
        bail!("session worker requires an authentication request");
    };
    let (prompts, prompt_receiver) = mpsc::channel::<pam::Prompt>();
    let (responses, response_receiver) = mpsc::channel();
    let forwarder = thread::spawn(move || -> Result<_> {
        while let Ok(prompt) = prompt_receiver.recv() {
            send(
                &mut stream,
                &Message::Prompt {
                    message: prompt.message,
                    echo: prompt.echo,
                },
            )?;
            if responses.send(receive_secret(&mut reader)?).is_err() {
                break;
            }
        }
        Ok((stream, reader))
    });
    let transaction =
        pam::Transaction::authenticate_interactive("alphin", &username, prompts, response_receiver);
    let (mut stream, mut reader) = forwarder
        .join()
        .map_err(|_| anyhow::anyhow!("PAM conversation thread panicked"))??;
    let transaction = match transaction {
        Ok(transaction) => transaction,
        Err(error) => {
            eprintln!("authentication failed for {username}: {error:#}");
            send(&mut stream, &Message::Failed)?;
            return Ok(());
        }
    };
    send(&mut stream, &Message::Succeeded)?;
    let Message::Launch {
        command,
        account,
        runtime_dir,
    } = receive(&mut reader)?
    else {
        bail!("session worker requires a launch command");
    };
    if account.username != username {
        bail!("session account does not match the authenticated account");
    }
    let transaction = match transaction.open_session() {
        Ok(transaction) => transaction,
        Err(error) => {
            eprintln!("failed to open PAM session for {username}: {error:#}");
            send(&mut stream, &Message::SessionFailed)?;
            return Ok(());
        }
    };
    let mut child = match identity::spawn(
        &command,
        &account,
        &runtime_dir,
        &transaction.session_environment(),
    ) {
        Ok(child) => child,
        Err(error) => {
            eprintln!("failed to launch session for {username}: {error:#}");
            send(&mut stream, &Message::SessionFailed)?;
            return Ok(());
        }
    };
    send(&mut stream, &Message::SessionStarted)?;
    let status = child.wait().context("wait for desktop session")?;
    eprintln!("desktop session for {username} exited with {status}");
    drop(transaction);
    send(&mut stream, &Message::SessionExited)?;
    stream.shutdown(Shutdown::Both).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_prompts_and_responses_without_serializing_secrets() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let mut reader = BufReader::new(receiver);
        send(
            &mut sender,
            &Message::Prompt {
                message: "Password:".into(),
                echo: false,
            },
        )
        .unwrap();
        assert!(matches!(
            receive(&mut reader).unwrap(),
            Message::Prompt {
                message,
                echo: false
            } if message == "Password:"
        ));
        send_secret(&mut sender, &Secret::new("example".into()).unwrap()).unwrap();
        assert_eq!(receive_secret(&mut reader).unwrap().as_bytes(), b"example");
    }
}
