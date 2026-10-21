use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::Path,
};

use anyhow::{bail, Context, Result};

use crate::{
    ipc::{Request, Response, SessionInfo, PROTOCOL_VERSION},
    session,
};

pub fn serve(socket_path: &Path, sessions_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    if socket_path.exists() {
        let metadata = fs::symlink_metadata(socket_path)?;
        if !metadata.file_type().is_socket() {
            bail!("refusing to replace non-socket {}", socket_path.display());
        }
        fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind private socket {}", socket_path.display()))?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
    let expected_uid = fs::metadata(socket_path)?.uid();
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                if peer_uid(&stream)? == expected_uid {
                    serve_connection(stream, sessions_path)?;
                }
            }
            Err(error) => return Err(error).context("accept private socket connection"),
        }
    }
    Ok(())
}

fn serve_connection(mut stream: UnixStream, sessions_path: &Path) -> Result<()> {
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
        Ok(request) => handle(request, sessions_path)?,
        Err(_) => Response::Error {
            version: PROTOCOL_VERSION,
            code: "invalid_request",
        },
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn handle(request: Request, sessions_path: &Path) -> Result<Response> {
    match request {
        Request::GetState { .. } => Ok(Response::State {
            version: PROTOCOL_VERSION,
            phase: "greeter",
        }),
        Request::Status { .. } => Ok(Response::Status {
            version: PROTOCOL_VERSION,
            status: "ready",
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
        Request::BeginAuthentication { .. } | Request::Respond { .. } | Request::Cancel { .. } => {
            Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "authentication_unavailable",
            })
        }
        Request::StartSession { .. } | Request::PowerAction { .. } => Ok(Response::Error {
            version: PROTOCOL_VERSION,
            code: "operation_unavailable",
        }),
    }
}

fn peer_uid(stream: &UnixStream) -> Result<u32> {
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
    Ok(unsafe { credentials.assume_init().uid })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::Request;
    use std::{io::Read, os::unix::net::UnixListener, thread};

    #[test]
    fn exposes_ready_greeter_state() {
        assert_eq!(
            handle(
                Request::GetState {
                    version: PROTOCOL_VERSION
                },
                Path::new("/missing")
            )
            .unwrap(),
            Response::State {
                version: PROTOCOL_VERSION,
                phase: "greeter"
            }
        );
    }

    #[test]
    fn rejects_unauthenticated_operations_until_pam_is_configured() {
        assert_eq!(
            handle(
                Request::BeginAuthentication {
                    version: PROTOCOL_VERSION,
                    username: "user".into()
                },
                Path::new("/missing")
            )
            .unwrap(),
            Response::Error {
                version: PROTOCOL_VERSION,
                code: "authentication_unavailable"
            }
        );
    }

    #[test]
    fn returns_a_versioned_json_response() {
        let path = std::env::temp_dir().join(format!("alphin-ipc-test-{}", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let sessions_path = path.parent().unwrap().to_owned();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_connection(stream, &sessions_path).unwrap();
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
