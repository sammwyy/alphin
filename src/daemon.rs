use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use anyhow::{bail, Context, Result};

use crate::{
    ipc::{Request, Response, SessionInfo, UserInfo, PROTOCOL_VERSION},
    pam::{self, Secret},
    session, users,
};

pub fn serve(socket_path: &Path, sessions_path: &Path) -> Result<()> {
    let expected_gid = socket_directory_gid(socket_path)?;
    if socket_path.exists() {
        let metadata = fs::symlink_metadata(socket_path)?;
        if !metadata.file_type().is_socket() {
            bail!("refusing to replace non-socket {}", socket_path.display());
        }
        fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind private socket {}", socket_path.display()))?;
    let path = std::ffi::CString::new(socket_path.as_os_str().as_bytes())
        .context("invalid private socket path")?;
    if unsafe { libc::chown(path.as_ptr(), 0, expected_gid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("set private socket ownership");
    }
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o660))?;
    let expected_uid = fs::metadata(socket_path)?.uid();
    let mut state = DaemonState::default();
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let peer = peer_credentials(&stream)?;
                if peer.uid == expected_uid || peer.gid == expected_gid {
                    serve_connection(stream, sessions_path, &mut state)?;
                }
            }
            Err(error) => return Err(error).context("accept private socket connection"),
        }
    }
    Ok(())
}

fn socket_directory_gid(socket_path: &Path) -> Result<u32> {
    let parent = socket_path
        .parent()
        .context("private socket has no parent directory")?;
    let parent_metadata = fs::metadata(parent)
        .with_context(|| format!("inspect private socket directory {}", parent.display()))?;
    if !parent_metadata.is_dir() || parent_metadata.mode() & 0o077 != 0 {
        bail!("private socket directory must not be accessible to other users");
    }
    Ok(parent_metadata.gid())
}

fn serve_connection(
    mut stream: UnixStream,
    sessions_path: &Path,
    state: &mut DaemonState,
) -> Result<()> {
    let mut line = String::new();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    BufReader::new(stream.try_clone()?)
        .take(16 * 1024 + 1)
        .read_line(&mut line)?;
    if line.len() > 16 * 1024 {
        bail!("IPC message exceeds 16 KiB");
    }
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) if request.version() != PROTOCOL_VERSION => Response::Error {
            version: PROTOCOL_VERSION,
            code: "unsupported_version",
        },
        Ok(request) => handle(request, sessions_path, state)?,
        Err(_) => Response::Error {
            version: PROTOCOL_VERSION,
            code: "invalid_request",
        },
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn handle(request: Request, sessions_path: &Path, state: &mut DaemonState) -> Result<Response> {
    state.pump();
    match request {
        Request::GetState { .. } => Ok(Response::State {
            version: PROTOCOL_VERSION,
            phase: state.phase(),
        }),
        Request::Status { .. } => match state.prompt.take() {
            Some(prompt) => Ok(Response::Prompt {
                version: PROTOCOL_VERSION,
                message: prompt.message,
                echo: prompt.echo,
            }),
            None => Ok(Response::Status {
                version: PROTOCOL_VERSION,
                status: "ready",
            }),
        },
        Request::ListUsers { .. } => Ok(Response::Users {
            version: PROTOCOL_VERSION,
            users: users::discover(Path::new("/etc/passwd"), 1000, 60_000)?
                .into_iter()
                .map(|user| UserInfo {
                    username: user.username,
                    display_name: user.display_name,
                })
                .collect(),
        }),
        Request::ListSessions { .. } => Ok(Response::Sessions {
            version: PROTOCOL_VERSION,
            sessions: session::discover(sessions_path)?
                .into_iter()
                .map(|session| SessionInfo {
                    id: session.id,
                    name: session.name,
                })
                .collect(),
        }),
        Request::BeginAuthentication { username, .. } => state.begin(username),
        Request::Respond { response, .. } => state.respond(response),
        Request::Cancel { .. } => Ok(state.cancel()),
        Request::StartSession { .. } | Request::PowerAction { .. } => Ok(Response::Error {
            version: PROTOCOL_VERSION,
            code: "operation_unavailable",
        }),
    }
}

#[derive(Default)]
struct DaemonState {
    authentication: Option<Authentication>,
    prompt: Option<pam::Prompt>,
    authenticated: bool,
}

struct Authentication {
    responses: Sender<Secret>,
    events: Receiver<AuthenticationEvent>,
}

enum AuthenticationEvent {
    Failed,
    Succeeded,
    Prompt(pam::Prompt),
}

impl DaemonState {
    fn phase(&self) -> &'static str {
        if self.authenticated {
            "authenticated"
        } else if self.authentication.is_some() {
            "authenticating"
        } else {
            "greeter"
        }
    }

    fn begin(&mut self, username: String) -> Result<Response> {
        if self.authentication.is_some() || self.authenticated {
            return Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "authentication_in_progress",
            });
        }
        if username.is_empty()
            || username.contains(':')
            || username.bytes().any(|byte| byte.is_ascii_control())
        {
            return Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "invalid_username",
            });
        }
        let (responses, response_receiver) = mpsc::channel();
        let (events, event_receiver) = mpsc::channel();
        thread::spawn(move || {
            let (prompts, prompt_receiver) = mpsc::channel();
            let prompt_events = events.clone();
            let forwarder = thread::spawn(move || {
                while let Ok(prompt) = prompt_receiver.recv() {
                    if prompt_events
                        .send(AuthenticationEvent::Prompt(prompt))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let result = pam::Transaction::authenticate_interactive(
                "alphin",
                &username,
                prompts,
                response_receiver,
            );
            let _ = events.send(if result.is_ok() {
                AuthenticationEvent::Succeeded
            } else {
                AuthenticationEvent::Failed
            });
            let _ = forwarder.join();
        });
        self.authentication = Some(Authentication {
            responses,
            events: event_receiver,
        });
        Ok(Response::Status {
            version: PROTOCOL_VERSION,
            status: "authenticating",
        })
    }

    fn respond(&mut self, response: String) -> Result<Response> {
        let Some(authentication) = &self.authentication else {
            return Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "no_authentication",
            });
        };
        authentication
            .responses
            .send(Secret::new(response)?)
            .map_err(|_| anyhow::anyhow!("authentication conversation is closed"))?;
        Ok(Response::Status {
            version: PROTOCOL_VERSION,
            status: "authenticating",
        })
    }

    fn cancel(&mut self) -> Response {
        self.authentication = None;
        self.prompt = None;
        self.authenticated = false;
        Response::Status {
            version: PROTOCOL_VERSION,
            status: "cancelled",
        }
    }

    fn pump(&mut self) {
        let Some(authentication) = &self.authentication else {
            return;
        };
        loop {
            match authentication.events.try_recv() {
                Ok(AuthenticationEvent::Prompt(prompt)) => self.prompt = Some(prompt),
                Ok(AuthenticationEvent::Succeeded) => {
                    self.authenticated = true;
                    self.authentication = None;
                    break;
                }
                Ok(AuthenticationEvent::Failed) | Err(TryRecvError::Disconnected) => {
                    self.authentication = None;
                    self.prompt = None;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
    }
}

struct PeerCredentials {
    uid: u32,
    gid: u32,
}

fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::zeroed();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 || length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(std::io::Error::last_os_error()).context("read Unix peer credentials");
    }
    let credentials = unsafe { credentials.assume_init() };
    Ok(PeerCredentials {
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::Request;
    use std::{io::Read, os::unix::net::UnixListener, thread};

    #[test]
    fn exposes_ready_greeter_state() {
        let mut state = DaemonState::default();
        assert_eq!(
            handle(
                Request::GetState {
                    version: PROTOCOL_VERSION
                },
                Path::new("/missing"),
                &mut state
            )
            .unwrap(),
            Response::State {
                version: PROTOCOL_VERSION,
                phase: "greeter"
            }
        );
    }

    #[test]
    fn rejects_invalid_authentication_usernames() {
        let mut state = DaemonState::default();
        assert_eq!(
            handle(
                Request::BeginAuthentication {
                    version: PROTOCOL_VERSION,
                    username: "bad:name".into()
                },
                Path::new("/missing"),
                &mut state
            )
            .unwrap(),
            Response::Error {
                version: PROTOCOL_VERSION,
                code: "invalid_username"
            }
        );
    }

    #[test]
    fn returns_pending_authentication_prompt() {
        let mut state = DaemonState {
            authentication: None,
            prompt: Some(pam::Prompt {
                message: "Password:".into(),
                echo: false,
            }),
            authenticated: false,
        };
        assert_eq!(
            handle(
                Request::Status {
                    version: PROTOCOL_VERSION,
                },
                Path::new("/missing"),
                &mut state,
            )
            .unwrap(),
            Response::Prompt {
                version: PROTOCOL_VERSION,
                message: "Password:".into(),
                echo: false,
            }
        );
    }

    #[test]
    fn requires_private_socket_directory() {
        let path = std::env::temp_dir().join(format!("alphin-runtime-test-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(socket_directory_gid(&path.join("alphind.sock")).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(socket_directory_gid(&path.join("alphind.sock")).is_err());
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn returns_a_versioned_json_response() {
        let path = std::env::temp_dir().join(format!("alphin-ipc-test-{}", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let sessions_path = path.parent().unwrap().to_owned();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut state = DaemonState::default();
            serve_connection(stream, &sessions_path, &mut state).unwrap();
        });
        let mut client = UnixStream::connect(&path).unwrap();
        client
            .write_all(b"{\"method\":\"status\",\"version\":1}\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(
            response,
            "{\"kind\":\"status\",\"version\":1,\"status\":\"ready\"}\n"
        );
    }
}
