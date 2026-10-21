use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub socket: PathBuf,
    pub sessions: PathBuf,
    pub worker: PathBuf,
    pub compositor: PathBuf,
    pub greeter: PathBuf,
    pub runtime_dir: PathBuf,
    #[serde(default)]
    pub users: UserPolicy,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UserPolicy {
    #[serde(default = "default_minimum_uid")]
    pub minimum_uid: u32,
    #[serde(default = "default_maximum_uid")]
    pub maximum_uid: u32,
    #[serde(default)]
    pub hidden: Vec<String>,
}

impl Default for UserPolicy {
    fn default() -> Self {
        Self {
            minimum_uid: default_minimum_uid(),
            maximum_uid: default_maximum_uid(),
            hidden: Vec::new(),
        }
    }
}

fn default_minimum_uid() -> u32 {
    1000
}

fn default_maximum_uid() -> u32 {
    60_000
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("read configuration {}", path.display()))?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("parse configuration {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_system(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect configuration {}", path.display()))?;
        if metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0
        {
            bail!("system configuration must be root-owned and not group or world writable");
        }
        Self::load(path)
    }

    fn validate(&self) -> Result<()> {
        for (name, path) in [
            ("socket", &self.socket),
            ("sessions", &self.sessions),
            ("worker", &self.worker),
            ("compositor", &self.compositor),
            ("greeter", &self.greeter),
            ("runtime_dir", &self.runtime_dir),
        ] {
            if !path.is_absolute() {
                bail!("{name} must be an absolute path");
            }
        }
        if !self.compositor.starts_with("/usr/lib/alphin/compositors") {
            bail!("compositor adapter must be installed under /usr/lib/alphin/compositors");
        }
        if !self.greeter.starts_with("/usr/lib/alphin/greeters") {
            bail!("greeter adapter must be installed under /usr/lib/alphin/greeters");
        }
        if self.users.minimum_uid > self.users.maximum_uid {
            bail!("minimum user UID must not exceed maximum user UID");
        }
        if self.users.hidden.iter().any(|username| {
            username.is_empty()
                || username.contains(':')
                || username.bytes().any(|byte| byte.is_ascii_control())
        }) {
            bail!("hidden user list contains an invalid username");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_paths() {
        let config: Config = toml::from_str("socket = 'socket'\nsessions = '/sessions'\nworker = '/worker'\ncompositor = '/compositor'\ngreeter = '/greeter'\nruntime_dir = '/run/alphin'\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn packaged_configuration_validates() {
        Config::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/config/config.toml"))
            .unwrap();
    }

    #[test]
    fn system_configuration_rejects_symlinks() {
        let root = std::env::temp_dir().join(format!("alphin-config-{}", std::process::id()));
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/config/config.toml"),
            &root,
        )
        .unwrap();
        assert!(Config::load_system(&root).is_err());
        fs::remove_file(root).unwrap();
    }

    #[test]
    fn rejects_adapters_outside_package_roots() {
        let config: Config = toml::from_str("socket = '/run/alphin/socket'\nsessions = '/sessions'\nworker = '/worker'\ncompositor = '/tmp/compositor.toml'\ngreeter = '/tmp/greeter.toml'\nruntime_dir = '/run/alphin'\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_inverted_user_uid_range() {
        let config: Config = toml::from_str("socket = '/run/alphin/socket'\nsessions = '/sessions'\nworker = '/worker'\ncompositor = '/usr/lib/alphin/compositors/blair.toml'\ngreeter = '/usr/lib/alphin/greeters/coconut.toml'\nruntime_dir = '/run/alphin'\n[users]\nminimum_uid = 2000\nmaximum_uid = 1000\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn packaged_socket_is_restricted_to_the_greeter_account() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/systemd/alphind.socket");
        let unit = fs::read_to_string(path).unwrap();
        assert!(unit.lines().any(|line| line == "SocketGroup=alphin"));
        assert!(unit.lines().any(|line| line == "SocketMode=0660"));
    }
}
