use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::{AsRawFd, FromRawFd, RawFd},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use anyhow::{bail, Context, Result};

use crate::{
    identity::{self, Account},
    ipc::{Request, Response, SessionInfo, UserInfo, PROTOCOL_VERSION},
    logind,
    pam::{self, Secret},
    session, users,
};

pub struct WorkerMonitor {
    child: Child,
    control_fd: RawFd,
    stopped: bool,
}

impl WorkerMonitor {
    pub fn spawn(
        executable: &Path,
        compositor: &Path,
        greeter: &Path,
        runtime_dir: &Path,
        account: &Account,
    ) -> Result<Self> {
        let mut descriptors = [0; 2];
        if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error()).context("create worker control pipe");
        }
        let groups = account.groups.clone();
        let uid = account.uid;
        let gid = account.gid;
        let mut command = Command::new(executable);
        command
            .args(["--compositor"])
            .arg(compositor)
            .args(["--greeter"])
            .arg(greeter)
            .args(["--runtime-dir"])
            .arg(runtime_dir)
            .args(["--control-fd", "3"]);
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(descriptors[1], 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if descriptors[0] != 3 {
                    libc::close(descriptors[0]);
                }
                if descriptors[1] != 3 {
                    libc::close(descriptors[1]);
                }
                let groups = groups
                    .iter()
                    .map(|group| *group as libc::gid_t)
                    .collect::<Vec<_>>();
                if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe {
                    libc::close(descriptors[0]);
                    libc::close(descriptors[1]);
                }
                return Err(error).context("start greeter worker");
            }
        };
        unsafe {
            libc::close(descriptors[1]);
        }
        let flags = unsafe { libc::fcntl(descriptors[0], libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(descriptors[0], libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            unsafe {
                libc::close(descriptors[0]);
            }
            return Err(std::io::Error::last_os_error()).context("configure worker control pipe");
        }
        Ok(Self {
            child,
            control_fd: descriptors[0],
            stopped: false,
        })
    }

    fn poll(&mut self) -> Result<WorkerState> {
        if self.stopped {
            return Ok(WorkerState::Stopped);
        }
        let mut signal = [0_u8];
        let read = unsafe { libc::read(self.control_fd, signal.as_mut_ptr().cast(), signal.len()) };
        if read == 1 && signal == [1] {
            self.stopped = true;
            return Ok(WorkerState::Stopped);
        }
        if read == 0 {
            return Ok(WorkerState::Exited(self.child.try_wait()?.is_some()));
        }
        if read < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) {
            return Ok(WorkerState::Running);
        }
        Err(std::io::Error::last_os_error()).context("read worker control pipe")
    }
}

impl Drop for WorkerMonitor {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.control_fd);
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

enum WorkerState {
    Running,
    Stopped,
    Exited(bool),
}

pub fn serve(
    socket_path: &Path,
    sessions_path: &Path,
    worker: Option<WorkerMonitor>,
) -> Result<()> {
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
    listener.set_nonblocking(true)?;
    let path = std::ffi::CString::new(socket_path.as_os_str().as_bytes())
        .context("invalid private socket path")?;
    if unsafe { libc::chown(path.as_ptr(), 0, expected_gid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("set private socket ownership");
    }
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o660))?;
    let expected_uid = fs::metadata(socket_path)?.uid();
    serve_listener(listener, sessions_path, worker, expected_uid, expected_gid)
}

pub fn serve_activated(
    socket_fd: RawFd,
    socket_path: &Path,
    sessions_path: &Path,
    worker: Option<WorkerMonitor>,
) -> Result<()> {
    let expected_gid = socket_directory_gid(socket_path)?;
    let listener = unsafe { UnixListener::from_raw_fd(socket_fd) };
    listener.set_nonblocking(true)?;
    let address = listener.local_addr().context("inspect activated socket")?;
    if address.as_pathname() != Some(socket_path) {
        bail!("activated socket does not match {}", socket_path.display());
    }
    let socket_metadata = fs::metadata(socket_path)?;
    if socket_metadata.uid() != 0
        || socket_metadata.gid() != expected_gid
        || socket_metadata.mode() & 0o777 != 0o660
    {
        bail!("activated socket has unexpected ownership or permissions");
    }
    serve_listener(
        listener,
        sessions_path,
        worker,
        socket_metadata.uid(),
        expected_gid,
    )
}

fn serve_listener(
    listener: UnixListener,
    sessions_path: &Path,
    worker: Option<WorkerMonitor>,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<()> {
    let mut state = DaemonState::default();
    let mut worker = worker;
    loop {
        if let Some(worker) = &mut worker {
            match worker.poll()? {
                WorkerState::Running => {}
                WorkerState::Stopped => state.greeter_stopped = true,
                WorkerState::Exited(true) => {
                    return Err(anyhow::anyhow!(
                        "greeter worker exited without completion signal"
                    ))
                }
                WorkerState::Exited(false) => {
                    return Err(anyhow::anyhow!("greeter worker exited before completion"))
                }
            }
        }
        state.launch_pending()?;
        if state.restart_required {
            bail!("desktop session exited; restarting greeter");
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let peer = peer_credentials(&stream)?;
                if peer.uid == expected_uid || peer.gid == expected_gid {
                    serve_connection(stream, sessions_path, &mut state)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("accept private socket connection"),
        }
    }
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
            users: users::discover(
                Path::new("/etc/passwd"),
                users::MINIMUM_UID,
                users::MAXIMUM_UID,
            )?
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
        Request::StartSession { session, .. } => state.select_session(session, sessions_path),
        Request::PowerAction { .. } if !state.authenticated => Ok(Response::Error {
            version: PROTOCOL_VERSION,
            code: "authentication_required",
        }),
        Request::PowerAction { action, .. } => match logind::PowerAction::parse(&action) {
            Ok(action) => match logind::request_power(action) {
                Ok(()) => Ok(Response::Status {
                    version: PROTOCOL_VERSION,
                    status: "power_requested",
                }),
                Err(_) => Ok(Response::Error {
                    version: PROTOCOL_VERSION,
                    code: "power_denied",
                }),
            },
            Err(_) => Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "invalid_power_action",
            }),
        },
    }
}

#[derive(Default)]
struct DaemonState {
    authentication: Option<Authentication>,
    prompt: Option<pam::Prompt>,
    authenticated: bool,
    greeter_stopped: bool,
    pending_session: Option<Vec<String>>,
    session_starting: bool,
    session_started: bool,
    restart_required: bool,
}

struct Authentication {
    username: String,
    responses: Sender<Secret>,
    commands: Sender<AuthenticationCommand>,
    events: Receiver<AuthenticationEvent>,
}

enum AuthenticationCommand {
    Launch {
        command: Vec<String>,
        account: Account,
        runtime_dir: PathBuf,
    },
}

enum AuthenticationEvent {
    Failed,
    Succeeded,
    Prompt(pam::Prompt),
    SessionStarted,
    SessionFailed,
    SessionExited,
}

impl DaemonState {
    fn phase(&self) -> &'static str {
        if self.session_started {
            "session_running"
        } else if self.session_starting {
            "session_starting"
        } else if self.authenticated {
            "authenticated"
        } else if self.greeter_stopped {
            "greeter_stopped"
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
        if !users::is_eligible(Path::new("/etc/passwd"), &username)? {
            return Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "ineligible_user",
            });
        }
        let (responses, response_receiver) = mpsc::channel();
        let (commands, command_receiver) = mpsc::channel();
        let (events, event_receiver) = mpsc::channel();
        let thread_username = username.clone();
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
                &thread_username,
                prompts,
                response_receiver,
            );
            let Ok(transaction) = result else {
                let _ = events.send(AuthenticationEvent::Failed);
                let _ = forwarder.join();
                return;
            };
            let _ = events.send(AuthenticationEvent::Succeeded);
            let _ = forwarder.join();
            let Ok(AuthenticationCommand::Launch {
                command,
                account,
                runtime_dir,
            }) = command_receiver.recv()
            else {
                return;
            };
            let transaction = match transaction.open_session() {
                Ok(transaction) => transaction,
                Err(_) => {
                    let _ = events.send(AuthenticationEvent::SessionFailed);
                    return;
                }
            };
            let mut child = match identity::spawn(&command, &account, &runtime_dir) {
                Ok(child) => child,
                Err(_) => {
                    let _ = events.send(AuthenticationEvent::SessionFailed);
                    return;
                }
            };
            let _ = events.send(AuthenticationEvent::SessionStarted);
            let _ = child.wait();
            drop(transaction);
            let _ = events.send(AuthenticationEvent::SessionExited);
        });
        self.authentication = Some(Authentication {
            username,
            responses,
            commands,
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
        self.pending_session = None;
        self.session_starting = false;
        self.session_started = false;
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
                Ok(AuthenticationEvent::Succeeded) => self.authenticated = true,
                Ok(AuthenticationEvent::SessionStarted) => self.session_started = true,
                Ok(AuthenticationEvent::SessionExited) => {
                    self.authentication = None;
                    self.authenticated = false;
                    self.session_started = false;
                    self.session_starting = false;
                    self.restart_required = true;
                    break;
                }
                Ok(AuthenticationEvent::SessionFailed) => {
                    self.session_starting = false;
                    self.pending_session = None;
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

    fn select_session(&mut self, id: String, sessions_path: &Path) -> Result<Response> {
        if !self.authenticated {
            return Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "authentication_required",
            });
        }
        if self.pending_session.is_some() || self.session_starting || self.session_started {
            return Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "session_in_progress",
            });
        }
        let session = session::discover(sessions_path)?
            .into_iter()
            .find(|session| session.id == id);
        let Some(session) = session else {
            return Ok(Response::Error {
                version: PROTOCOL_VERSION,
                code: "unknown_session",
            });
        };
        self.pending_session = Some(session.command);
        Ok(Response::Status {
            version: PROTOCOL_VERSION,
            status: "session_queued",
        })
    }

    fn launch_pending(&mut self) -> Result<()> {
        self.pump();
        if !self.greeter_stopped
            || !self.authenticated
            || self.session_starting
            || self.pending_session.is_none()
        {
            return Ok(());
        }
        let command = self.pending_session.take().unwrap();
        let authentication = self
            .authentication
            .as_ref()
            .context("PAM transaction is unavailable")?;
        let account = identity::lookup(&authentication.username)?;
        let runtime_dir = PathBuf::from(format!("/run/user/{}", account.uid));
        authentication
            .commands
            .send(AuthenticationCommand::Launch {
                command,
                account,
                runtime_dir,
            })
            .map_err(|_| anyhow::anyhow!("PAM session thread is unavailable"))?;
        self.session_starting = true;
        Ok(())
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
    fn rejects_ineligible_authentication_usernames() {
        let mut state = DaemonState::default();
        assert_eq!(
            state.begin("root".into()).unwrap(),
            Response::Error {
                version: PROTOCOL_VERSION,
                code: "ineligible_user",
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
            greeter_stopped: false,
            pending_session: None,
            session_starting: false,
            session_started: false,
            restart_required: false,
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
    fn recognizes_private_worker_completion() {
        let mut descriptors = [0; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        assert_eq!(
            unsafe { libc::write(descriptors[1], [1_u8].as_ptr().cast(), 1) },
            1
        );
        unsafe {
            libc::close(descriptors[1]);
        }
        let mut worker = WorkerMonitor {
            child: Command::new("/usr/bin/true").spawn().unwrap(),
            control_fd: descriptors[0],
            stopped: false,
        };
        assert!(matches!(worker.poll().unwrap(), WorkerState::Stopped));
        assert!(matches!(worker.poll().unwrap(), WorkerState::Stopped));
    }

    #[test]
    fn queues_selected_session_until_greeter_stops() {
        let directory =
            std::env::temp_dir().join(format!("alphin-session-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("blair.desktop"),
            "[Desktop Entry]\nType=Application\nName=Blair\nExec=/usr/bin/blair\n",
        )
        .unwrap();
        let (responses, _) = mpsc::channel();
        let (commands, _) = mpsc::channel();
        let (_, events) = mpsc::channel();
        let mut state = DaemonState {
            authentication: Some(Authentication {
                username: "user".into(),
                responses,
                commands,
                events,
            }),
            prompt: None,
            authenticated: true,
            greeter_stopped: false,
            pending_session: None,
            session_starting: false,
            session_started: false,
            restart_required: false,
        };
        assert_eq!(
            state.select_session("blair".into(), &directory).unwrap(),
            Response::Status {
                version: PROTOCOL_VERSION,
                status: "session_queued"
            }
        );
        assert_eq!(state.pending_session, Some(vec!["/usr/bin/blair".into()]));
        fs::remove_dir_all(directory).unwrap();
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

    #[test]
    fn requires_authentication_for_power_actions() {
        let mut state = DaemonState::default();
        assert_eq!(
            handle(
                Request::PowerAction {
                    version: PROTOCOL_VERSION,
                    action: "reboot".into(),
                },
                Path::new("/missing"),
                &mut state,
            )
            .unwrap(),
            Response::Error {
                version: PROTOCOL_VERSION,
                code: "authentication_required",
            }
        );
    }

    #[test]
    fn requests_greeter_restart_after_session_exit() {
        let (responses, _) = mpsc::channel();
        let (commands, _) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        events.send(AuthenticationEvent::SessionExited).unwrap();
        let mut state = DaemonState {
            authentication: Some(Authentication {
                username: "user".into(),
                responses,
                commands,
                events: receiver,
            }),
            prompt: None,
            authenticated: true,
            greeter_stopped: true,
            pending_session: None,
            session_starting: false,
            session_started: true,
            restart_required: false,
        };
        state.pump();
        assert!(state.restart_required);
        assert!(state.authentication.is_none());
    }
}
