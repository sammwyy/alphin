use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::ipc::PROTOCOL_VERSION;

#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    pub fn status(&self) -> Result<Value> {
        self.request(json!({ "method": "status", "version": PROTOCOL_VERSION }))
    }

    pub fn state(&self) -> Result<Value> {
        self.request(json!({ "method": "get_state", "version": PROTOCOL_VERSION }))
    }

    pub fn users(&self) -> Result<Value> {
        self.request(json!({ "method": "list_users", "version": PROTOCOL_VERSION }))
    }

    pub fn sessions(&self) -> Result<Value> {
        self.request(json!({ "method": "list_sessions", "version": PROTOCOL_VERSION }))
    }

    pub fn begin_authentication(&self, username: &str) -> Result<Value> {
        self.request(json!({ "method": "begin_authentication", "version": PROTOCOL_VERSION, "username": username }))
    }

    pub fn respond(&self, response: String) -> Result<Value> {
        let mut response = response;
        let result = self.request(RespondRequest {
            method: "respond",
            version: PROTOCOL_VERSION,
            response: &response,
        });
        response.zeroize();
        result
    }

    pub fn cancel(&self) -> Result<Value> {
        self.request(json!({ "method": "cancel", "version": PROTOCOL_VERSION }))
    }

    pub fn start_session(&self, session: &str) -> Result<Value> {
        self.request(
            json!({ "method": "start_session", "version": PROTOCOL_VERSION, "session": session }),
        )
    }

    fn request(&self, request: impl Serialize) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect to {}", self.socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        serde_json::to_writer(&mut stream, &request)?;
        stream.write_all(b"\n")?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response)?;
        if response.len() > 16 * 1024 {
            bail!("daemon response exceeds 16 KiB");
        }
        serde_json::from_str(&response).context("parse daemon response")
    }
}

#[derive(Serialize)]
struct RespondRequest<'a> {
    method: &'static str,
    version: u16,
    response: &'a str,
}

pub fn socket_from_environment() -> Result<Client> {
    let socket = std::env::var_os("ALPHIN_SOCKET").context("ALPHIN_SOCKET is required")?;
    Ok(Client::new(socket))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::net::UnixListener, thread};

    #[test]
    fn sends_versioned_requests_and_parses_responses() {
        let path = std::env::temp_dir().join(format!("alphin-client-{}", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["method"],
                "list_sessions"
            );
            stream
                .write_all(b"{\"kind\":\"sessions\",\"version\":1,\"sessions\":[]}\n")
                .unwrap();
        });
        let response = Client::new(&path).sessions().unwrap();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(response["kind"], "sessions");
    }
}
