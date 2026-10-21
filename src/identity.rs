use std::{
    collections::BTreeSet,
    fs,
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command},
};

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub username: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
    pub groups: Vec<u32>,
}

pub fn lookup(username: &str) -> Result<Account> {
    lookup_in(username, Path::new("/etc/passwd"), Path::new("/etc/group"))
}

pub fn lookup_in(username: &str, passwd_path: &Path, group_path: &Path) -> Result<Account> {
    let passwd = fs::read_to_string(passwd_path)
        .with_context(|| format!("read {}", passwd_path.display()))?;
    let mut account = passwd
        .lines()
        .find_map(|line| parse_passwd(line, username))
        .context("account was not found")?;
    let groups =
        fs::read_to_string(group_path).with_context(|| format!("read {}", group_path.display()))?;
    let mut memberships = BTreeSet::from([account.gid]);
    for group in groups.lines().filter_map(parse_group) {
        if group.members.iter().any(|member| member == username) {
            memberships.insert(group.gid);
        }
    }
    account.groups = memberships.into_iter().collect();
    Ok(account)
}

pub fn spawn(command: &[String], account: &Account, runtime_dir: &Path) -> Result<Child> {
    let executable = command.first().context("session command is empty")?;
    validate_runtime_dir(runtime_dir, account.uid)?;
    command_for(command, account, runtime_dir)?
        .spawn()
        .with_context(|| format!("start session executable {executable}"))
}

fn validate_runtime_dir(path: &Path, uid: u32) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("inspect runtime directory {}", path.display()))?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        bail!("session runtime directory must be private and owned by the session user");
    }
    Ok(())
}

fn command_for(command: &[String], account: &Account, runtime_dir: &Path) -> Result<Command> {
    let executable = command.first().context("session command is empty")?;
    if command.iter().any(|argument| argument.contains('\0')) {
        bail!("session command contains a NUL byte");
    }
    let home = account.home.as_os_str();
    let shell = account.shell.as_os_str();
    let groups = account.groups.clone();
    let uid = account.uid;
    let gid = account.gid;
    let mut process = Command::new(executable);
    process
        .args(&command[1..])
        .current_dir(&account.home)
        .env_clear()
        .env("HOME", home)
        .env("USER", &account.username)
        .env("LOGNAME", &account.username)
        .env("SHELL", shell)
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "Blair");
    unsafe {
        process.pre_exec(move || {
            if libc::geteuid() == 0 {
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
            } else if libc::geteuid() != uid || libc::getegid() != gid {
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            Ok(())
        });
    }
    Ok(process)
}

fn parse_passwd(line: &str, expected_username: &str) -> Option<Account> {
    let mut fields = line.split(':');
    let username = fields.next()?;
    fields.next()?;
    let uid = fields.next()?.parse().ok()?;
    let gid = fields.next()?.parse().ok()?;
    fields.next()?;
    let home = PathBuf::from(fields.next()?);
    let shell = PathBuf::from(fields.next()?);
    (username == expected_username).then_some(Account {
        username: username.to_owned(),
        uid,
        gid,
        home,
        shell,
        groups: Vec::new(),
    })
}

struct Group {
    gid: u32,
    members: Vec<String>,
}

fn parse_group(line: &str) -> Option<Group> {
    let mut fields = line.split(':');
    fields.next()?;
    fields.next()?;
    let gid = fields.next()?.parse().ok()?;
    let members = fields
        .next()?
        .split(',')
        .filter(|member| !member.is_empty())
        .map(str::to_owned)
        .collect();
    Some(Group { gid, members })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn resolves_primary_and_supplementary_groups() {
        let root = std::env::temp_dir().join(format!(
            "alphin-identity-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let passwd = root.join("passwd");
        let group = root.join("group");
        fs::write(&passwd, "alex:x:1000:100:Alex:/home/alex:/bin/bash\n").unwrap();
        fs::write(&group, "users:x:100:alex\nvideo:x:44:alex\n").unwrap();
        let account = lookup_in("alex", &passwd, &group).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(account.groups, [44, 100]);
        assert_eq!(account.home, Path::new("/home/alex"));
    }

    #[test]
    fn session_environment_is_explicit() {
        let account = Account {
            username: "alex".into(),
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            home: PathBuf::from("/tmp"),
            shell: PathBuf::from("/bin/bash"),
            groups: vec![unsafe { libc::getegid() }],
        };
        let process = command_for(
            &["/usr/bin/env".into()],
            &account,
            Path::new("/run/user/1000"),
        )
        .unwrap();
        let environment = process
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(|value| value.to_owned())))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(environment.len(), 8);
        assert_eq!(
            environment.get(std::ffi::OsStr::new("XDG_SESSION_TYPE")),
            Some(&Some("wayland".into()))
        );
        assert!(!environment.contains_key(std::ffi::OsStr::new("LD_PRELOAD")));
    }

    #[test]
    fn requires_private_runtime_directory() {
        let directory = std::env::temp_dir().join(format!("alphin-runtime-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_runtime_dir(&directory, unsafe { libc::geteuid() }).is_ok());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_runtime_dir(&directory, unsafe { libc::geteuid() }).is_err());
        fs::remove_dir(directory).unwrap();
    }
}
