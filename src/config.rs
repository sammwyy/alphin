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
}
