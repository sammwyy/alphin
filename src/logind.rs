use anyhow::{bail, Context, Result};
use zbus::blocking::{Connection, Proxy};

const DESTINATION: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const INTERFACE: &str = "org.freedesktop.login1.Manager";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    PowerOff,
    Reboot,
    Suspend,
    Hibernate,
}

impl PowerAction {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "poweroff" => Ok(Self::PowerOff),
            "reboot" => Ok(Self::Reboot),
            "suspend" => Ok(Self::Suspend),
            "hibernate" => Ok(Self::Hibernate),
            _ => bail!("unsupported power action"),
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::PowerOff => "PowerOff",
            Self::Reboot => "Reboot",
            Self::Suspend => "Suspend",
            Self::Hibernate => "Hibernate",
        }
    }
}

pub fn request_power(action: PowerAction) -> Result<()> {
    let connection = Connection::system().context("connect to system bus")?;
    let proxy =
        Proxy::new(&connection, DESTINATION, PATH, INTERFACE).context("open logind manager")?;
    proxy
        .call_method(action.method(), &(true,))
        .with_context(|| format!("request logind {}", action.method()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_supported_power_actions() {
        assert_eq!(PowerAction::parse("reboot").unwrap(), PowerAction::Reboot);
        assert!(PowerAction::parse("shell-command").is_err());
    }
}
