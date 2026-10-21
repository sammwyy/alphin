use std::{
    fs,
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
}
