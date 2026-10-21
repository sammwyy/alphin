use std::{
    fs,
    os::unix::{fs::FileTypeExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};

use crate::adapter::Adapter;

pub fn run(compositor: &Adapter, greeter: &Adapter, runtime_dir: &Path) -> Result<()> {
    let mut compositor_process = spawn(compositor, runtime_dir, None)?;
    let display = match wait_for_wayland_socket(runtime_dir, compositor.readiness_timeout()) {
        Ok(display) => display,
        Err(error) => {
            stop(&mut compositor_process, compositor.shutdown_timeout());
            return Err(error);
        }
    };
    let mut greeter_process = spawn(greeter, runtime_dir, Some(&display))?;
    supervise(
        &mut compositor_process,
        &mut greeter_process,
        compositor.shutdown_timeout(),
        greeter.shutdown_timeout(),
    )
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
        .spawn()
        .with_context(|| format!("start {} adapter", adapter.id))
}

fn wait_for_wayland_socket(runtime_dir: &Path, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut displays = fs::read_dir(runtime_dir)
            .with_context(|| format!("read runtime directory {}", runtime_dir.display()))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                (name.starts_with("wayland-") && entry.file_type().ok()?.is_socket())
                    .then_some(name)
            })
            .collect::<Vec<_>>();
        displays.sort();
        if let Some(display) = displays.into_iter().next() {
            return Ok(display);
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
        assert_eq!(
            wait_for_wayland_socket(&path, Duration::ZERO).unwrap(),
            "wayland-1"
        );
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
}
