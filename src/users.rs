use std::{fs, path::Path};

use anyhow::{Context, Result};

use crate::config::UserPolicy;

pub const MINIMUM_UID: u32 = 1000;
pub const MAXIMUM_UID: u32 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub username: String,
    pub display_name: String,
}

pub fn discover(path: &Path, policy: &UserPolicy) -> Result<Vec<User>> {
    let source =
        fs::read_to_string(path).with_context(|| format!("read passwd file {}", path.display()))?;
    let mut users = source
        .lines()
        .filter_map(|line| parse(line, policy))
        .collect::<Vec<_>>();
    users.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.username.cmp(&right.username))
    });
    Ok(users)
}

pub fn is_eligible(path: &Path, username: &str, policy: &UserPolicy) -> Result<bool> {
    Ok(discover(path, policy)?
        .iter()
        .any(|user| user.username == username))
}

fn parse(line: &str, policy: &UserPolicy) -> Option<User> {
    let mut fields = line.split(':');
    let username = fields.next()?;
    fields.next()?;
    let uid = fields.next()?.parse::<u32>().ok()?;
    fields.next()?;
    let gecos = fields.next()?;
    fields.next()?;
    let shell = fields.next()?;
    if username.is_empty()
        || !(policy.minimum_uid..=policy.maximum_uid).contains(&uid)
        || policy.hidden.iter().any(|hidden| hidden == username)
        || shell.ends_with("nologin")
        || shell.ends_with("false")
    {
        return None;
    }
    let display_name = gecos
        .split(',')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(username)
        .to_owned();
    Some(User {
        username: username.to_owned(),
        display_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_system_and_non_login_accounts() {
        assert!(parse(
            "daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin",
            &UserPolicy::default()
        )
        .is_none());
        assert_eq!(
            parse(
                "alex:x:1000:1000:Alex Doe,,,:/home/alex:/bin/bash",
                &UserPolicy::default()
            )
            .unwrap()
            .display_name,
            "Alex Doe"
        );
    }

    #[test]
    fn applies_hidden_user_policy() {
        let policy = UserPolicy {
            hidden: vec!["alex".into()],
            ..Default::default()
        };
        assert!(parse("alex:x:1000:1000:Alex:/home/alex:/bin/bash", &policy).is_none());
    }
}
