use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use anyhow::{bail, Context, Result};

use crate::{adapter::Adapter, config::Config, session};

pub struct Report {
    pub compositor: String,
    pub greeter: String,
    pub session_count: usize,
}

pub fn inspect(config: &Config) -> Result<Report> {
    let compositor = Adapter::load(&config.compositor)?;
    let greeter = Adapter::load(&config.greeter)?;
    executable(&compositor.executable)?;
    executable(&greeter.executable)?;
    let sessions = session::discover(&config.sessions)?;
    Ok(Report {
        compositor: compositor.id,
        greeter: greeter.id,
        session_count: sessions.len(),
    })
}

fn executable(path: &Path) -> Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("inspect executable {}", path.display()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!("adapter executable is not executable: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn validates_installed_components_without_starting_them() {
        let root = std::env::temp_dir().join(format!(
            "alphin-doctor-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sessions = root.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(root.join("compositor.toml"), "id = 'compositor'\nexecutable = '/usr/bin/true'\nshutdown_timeout_seconds = 1\n[readiness]\nkind = 'wayland_socket'\ntimeout_seconds = 1\n").unwrap();
        fs::write(root.join("greeter.toml"), "id = 'greeter'\nexecutable = '/usr/bin/true'\nshutdown_timeout_seconds = 1\n[readiness]\nkind = 'wayland_socket'\ntimeout_seconds = 1\n").unwrap();
        fs::write(
            sessions.join("test.desktop"),
            "[Desktop Entry]\nType=Application\nExec=/usr/bin/true\n",
        )
        .unwrap();
        let report = inspect(&Config {
            socket: PathBuf::from("/run/alphin/socket"),
            sessions,
            worker: PathBuf::from("/usr/bin/true"),
            compositor: root.join("compositor.toml"),
            greeter: root.join("greeter.toml"),
            runtime_dir: PathBuf::from("/run/alphin"),
            users: Default::default(),
        })
        .unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(report.session_count, 1);
    }

    #[test]
    fn rejects_non_executable_adapter_programs() {
        let path =
            std::env::temp_dir().join(format!("alphin-doctor-program-{}", std::process::id()));
        fs::write(&path, "program").unwrap();
        assert!(executable(&path).is_err());
        fs::remove_file(path).unwrap();
    }
}
