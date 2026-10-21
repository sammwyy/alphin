use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Adapter {
    pub id: String,
    pub executable: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub readiness: Readiness,
    #[serde(default)]
    pub shutdown_timeout_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Readiness {
    WaylandSocket { timeout_seconds: u64 },
}

impl Adapter {
    pub fn load(path: &Path) -> Result<Self> {
        let contents =
            fs::read_to_string(path).with_context(|| format!("read adapter {}", path.display()))?;
        let adapter: Self = toml::from_str(&contents)
            .with_context(|| format!("parse adapter {}", path.display()))?;
        adapter.validate()?;
        Ok(adapter)
    }

    pub fn readiness_timeout(&self) -> Duration {
        match self.readiness {
            Readiness::WaylandSocket { timeout_seconds } => Duration::from_secs(timeout_seconds),
        }
    }

    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.shutdown_timeout_seconds)
    }

    fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!("adapter id must contain lowercase ASCII letters, digits, or hyphens");
        }
        if !self.executable.is_absolute() {
            bail!("adapter executable must be an absolute path");
        }
        if self
            .arguments
            .iter()
            .any(|argument| argument.contains('\0'))
        {
            bail!("adapter arguments cannot contain NUL bytes");
        }
        if self.environment.iter().any(|(key, value)| {
            key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0')
        }) {
            bail!("adapter environment contains an invalid name or value");
        }
        match self.readiness {
            Readiness::WaylandSocket { timeout_seconds }
                if timeout_seconds == 0 || timeout_seconds > 120 =>
            {
                bail!("wayland readiness timeout must be between 1 and 120 seconds");
            }
            _ => {}
        }
        if self.shutdown_timeout_seconds == 0 || self.shutdown_timeout_seconds > 120 {
            bail!("shutdown timeout must be between 1 and 120 seconds");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_executables() {
        let adapter: Adapter = toml::from_str(
            "id = 'blair'\nexecutable = 'blair'\nshutdown_timeout_seconds = 5\n[readiness]\nkind = 'wayland_socket'\ntimeout_seconds = 5\n",
        )
        .unwrap();
        assert!(adapter.validate().is_err());
    }

    #[test]
    fn parses_vetted_adapter() {
        let adapter: Adapter = toml::from_str(
            "id = 'blair'\nexecutable = '/usr/bin/blair'\narguments = ['--profile', 'greeter']\nshutdown_timeout_seconds = 5\n[readiness]\nkind = 'wayland_socket'\ntimeout_seconds = 10\n",
        )
        .unwrap();
        adapter.validate().unwrap();
    }

    #[test]
    fn packaged_adapters_validate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/adapters");
        Adapter::load(&root.join("compositors/blair.toml")).unwrap();
        Adapter::load(&root.join("greeters/coconut.toml")).unwrap();
    }
}
