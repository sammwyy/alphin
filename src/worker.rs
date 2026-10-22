use std::{
    collections::HashMap,
    fs,
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        io::RawFd,
        net::UnixStream,
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};

use crate::adapter::Adapter;

pub fn run(compositor: &Adapter, greeter: &Adapter, runtime_dir: &Path) -> Result<()> {
    let existing = wayland_sockets(runtime_dir)?;
    let mut compositor_process = spawn(compositor, runtime_dir, None)?;
    let display = match wait_for_wayland_socket(
        runtime_dir,
        &existing,
        &mut compositor_process,
        compositor.readiness_timeout(),
    ) {
        Ok(display) => display,
        Err(error) => {
            stop(&mut compositor_process, compositor.shutdown_timeout());
            return Err(error);
        }
    };
    let mut greeter_process = match spawn(greeter, runtime_dir, Some(&display)) {
        Ok(process) => process,
        Err(error) => {
            stop(&mut compositor_process, compositor.shutdown_timeout());
            return Err(error);
        }
    };
    supervise(
        &mut compositor_process,
        &mut greeter_process,
        compositor.shutdown_timeout(),
        greeter.shutdown_timeout(),
    )
}

pub fn protect_control_fd(control_fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(control_fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(control_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(std::io::Error::last_os_error())
            .context("protect worker control file descriptor");
    }
    Ok(())
}

pub fn notify_control(control_fd: RawFd) -> Result<()> {
    let signal = [1_u8];
    if unsafe { libc::write(control_fd, signal.as_ptr().cast(), signal.len()) } != 1 {
        return Err(std::io::Error::last_os_error()).context("notify daemon that greeter stopped");
    }
    Ok(())
}

fn supervise(
    compositor: &mut Child,
    greeter: &mut Child,
    compositor_shutdown_timeout: Duration,
    greeter_shutdown_timeout: Duration,
) -> Result<()> {
    loop {
        if let Some(status) = greeter.try_wait().context("check greeter status")? {
            stop(compositor, compositor_shutdown_timeout);
            if status.success() {
                return Ok(());
            }
            bail!("greeter exited with {status}");
        }
        if let Some(status) = compositor.try_wait().context("check compositor status")? {
            stop(greeter, greeter_shutdown_timeout);
            bail!("compositor exited before greeter with {status}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn spawn(adapter: &Adapter, runtime_dir: &Path, display: Option<&str>) -> Result<Child> {
    command_for(adapter, runtime_dir, display)
        .spawn()
        .with_context(|| format!("start {} adapter", adapter.id))
}

fn command_for(adapter: &Adapter, runtime_dir: &Path, display: Option<&str>) -> Command {
    let mut command = Command::new(&adapter.executable);
    command
        .args(&adapter.arguments)
        .current_dir(runtime_dir)
        .env_clear()
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("PATH", "/usr/bin:/bin")
        .envs(&adapter.environment);
    if let Some(display) = display {
        command.env("WAYLAND_DISPLAY", display);
    }
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    command
}

fn wayland_sockets(runtime_dir: &Path) -> Result<HashMap<String, (u64, u64)>> {
    Ok(fs::read_dir(runtime_dir)
        .with_context(|| format!("read runtime directory {}", runtime_dir.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let metadata = entry.metadata().ok()?;
            (name.starts_with("wayland-") && metadata.file_type().is_socket())
                .then_some((name, (metadata.dev(), metadata.ino())))
        })
        .collect())
}

fn wait_for_wayland_socket(
    runtime_dir: &Path,
    existing: &HashMap<String, (u64, u64)>,
    compositor: &mut Child,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut displays = wayland_sockets(runtime_dir)?
            .into_iter()
            .filter(|(name, identity)| existing.get(name) != Some(identity))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        displays.sort();
        if let Some(display) = displays
            .into_iter()
            .find(|name| UnixStream::connect(runtime_dir.join(name)).is_ok())
        {
            return Ok(display);
        }
        if let Some(status) = compositor.try_wait().context("check compositor status")? {
            bail!("compositor exited before Wayland socket was ready with {status}");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for compositor Wayland socket");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn stop(child: &mut Child, timeout: Duration) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub fn runtime_dir(path: Option<PathBuf>) -> Result<PathBuf> {
    let path = path
        .or_else(|| std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
        .context("XDG_RUNTIME_DIR is required")?;
    let metadata = fs::metadata(&path)
        .with_context(|| format!("inspect runtime directory {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("runtime path is not a directory");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn finds_wayland_socket() {
        let path = std::env::temp_dir().join(format!("alphin-worker-test-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let socket = path.join("wayland-1");
        let _listener = UnixListener::bind(&socket).unwrap();
        let mut compositor = Command::new("/usr/bin/sleep").arg("5").spawn().unwrap();
        assert_eq!(
            wait_for_wayland_socket(&path, &Default::default(), &mut compositor, Duration::ZERO)
                .unwrap(),
            "wayland-1"
        );
        compositor.kill().unwrap();
        compositor.wait().unwrap();
        fs::remove_file(socket).unwrap();
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn skips_socket_left_by_previous_compositor() {
        let path = std::env::temp_dir().join(format!("alphin-stale-socket-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let stale = path.join("wayland-0");
        let listener = UnixListener::bind(&stale).unwrap();
        let existing = wayland_sockets(&path).unwrap();
        drop(listener);
        let fresh = path.join("wayland-1");
        let _listener = UnixListener::bind(&fresh).unwrap();
        let mut compositor = Command::new("/usr/bin/sleep").arg("5").spawn().unwrap();
        assert_eq!(
            wait_for_wayland_socket(&path, &existing, &mut compositor, Duration::ZERO).unwrap(),
            "wayland-1"
        );
        compositor.kill().unwrap();
        compositor.wait().unwrap();
        fs::remove_file(stale).unwrap();
        fs::remove_file(fresh).unwrap();
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn recognizes_rebound_socket_with_the_same_name() {
        let path =
            std::env::temp_dir().join(format!("alphin-rebound-socket-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let socket = path.join("wayland-1");
        let listener = UnixListener::bind(&socket).unwrap();
        let existing = wayland_sockets(&path).unwrap();
        drop(listener);
        fs::remove_file(&socket).unwrap();
        let _listener = UnixListener::bind(&socket).unwrap();
        let mut compositor = Command::new("/usr/bin/sleep").arg("5").spawn().unwrap();
        assert_eq!(
            wait_for_wayland_socket(&path, &existing, &mut compositor, Duration::ZERO).unwrap(),
            "wayland-1"
        );
        compositor.kill().unwrap();
        compositor.wait().unwrap();
        fs::remove_file(socket).unwrap();
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn terminates_greeter_after_compositor_crash() {
        let mut compositor = Command::new("/usr/bin/true").spawn().unwrap();
        let mut greeter = Command::new("/usr/bin/sleep").arg("5").spawn().unwrap();
        assert!(supervise(
            &mut compositor,
            &mut greeter,
            Duration::ZERO,
            Duration::ZERO
        )
        .is_err());
        assert!(greeter.try_wait().unwrap().is_some());
    }

    #[test]
    fn terminates_compositor_after_greeter_exit() {
        let mut compositor = Command::new("/usr/bin/sleep").arg("5").spawn().unwrap();
        let mut greeter = Command::new("/usr/bin/true").spawn().unwrap();
        supervise(
            &mut compositor,
            &mut greeter,
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap();
        assert!(compositor.try_wait().unwrap().is_some());
    }

    #[test]
    fn signals_worker_completion_over_private_fd() {
        let mut descriptors = [0; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        protect_control_fd(descriptors[1]).unwrap();
        assert_ne!(
            unsafe { libc::fcntl(descriptors[1], libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        notify_control(descriptors[1]).unwrap();
        let mut signal = [0_u8];
        assert_eq!(
            unsafe { libc::read(descriptors[0], signal.as_mut_ptr().cast(), signal.len()) },
            1
        );
        unsafe {
            libc::close(descriptors[0]);
            libc::close(descriptors[1]);
        }
        assert_eq!(signal, [1]);
    }

    #[test]
    fn greeter_environment_excludes_user_home_configuration() {
        let adapter = Adapter {
            id: "test-greeter".into(),
            executable: PathBuf::from("/usr/bin/true"),
            arguments: Vec::new(),
            environment: Default::default(),
            ipc_version: Some(1),
            capabilities: Vec::new(),
            readiness: crate::adapter::Readiness::WaylandSocket { timeout_seconds: 1 },
            shutdown_timeout_seconds: 1,
        };
        let command = command_for(&adapter, Path::new("/run/alphin"), Some("wayland-0"));
        let environment = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(|value| value.to_owned())))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(!environment.contains_key(std::ffi::OsStr::new("HOME")));
        assert_eq!(
            environment.get(std::ffi::OsStr::new("XDG_RUNTIME_DIR")),
            Some(&Some("/run/alphin".into()))
        );
    }
}
