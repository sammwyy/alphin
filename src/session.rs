use std::{fs, path::Path};

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
}

pub fn discover(directory: &Path) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read sessions from {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "desktop")
        {
            if let Some(session) = parse_file(&path)? {
                sessions.push(session);
            }
        }
    }
    sessions.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(sessions)
}

pub fn parse_file(path: &Path) -> Result<Option<Session>> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("read desktop entry {}", path.display()))?;
    parse(&source, path)
}

fn parse(source: &str, path: &Path) -> Result<Option<Session>> {
    let mut section = "";
    let mut fields = std::collections::BTreeMap::new();
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            continue;
        }
        if section != "Desktop Entry" {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim(), value.trim());
        }
    }
    if fields.get("Type") != Some(&"Application") || matches!(fields.get("Hidden"), Some(&"true")) {
        return Ok(None);
    }
    let id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .context("desktop entry has invalid filename")?;
    if id.is_empty() {
        bail!("desktop entry has an empty id");
    }
    let name = fields.get("Name").copied().unwrap_or(id).to_owned();
    let exec = fields
        .get("Exec")
        .context("desktop entry is missing Exec")?;
    let command =
        parse_exec(exec).with_context(|| format!("invalid Exec in {}", path.display()))?;
    Ok(Some(Session {
        id: id.to_owned(),
        name,
        command,
    }))
}

fn parse_exec(value: &str) -> Result<Vec<String>> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            whitespace if whitespace.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    arguments.push(std::mem::take(&mut current));
                }
            }
            '%' => current.push(character),
            _ => current.push(character),
        }
    }
    if escaped || quoted {
        bail!("unterminated escape or quote");
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    for argument in &mut arguments {
        *argument = strip_field_codes(argument)?;
    }
    arguments.retain(|argument| !argument.is_empty());
    if arguments.is_empty() {
        bail!("Exec has no executable");
    }
    if arguments[0].contains('/') && !Path::new(&arguments[0]).is_absolute() {
        bail!("Exec executable path must be absolute when it contains a slash");
    }
    Ok(arguments)
}

fn strip_field_codes(value: &str) -> Result<String> {
    let mut result = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            result.push(character);
            continue;
        }
        match characters.next() {
            Some('%') => result.push('%'),
            Some('f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k') => {}
            Some(code) => bail!("unsupported field code %{code}"),
            None => bail!("trailing percent sign"),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_quoted_exec_and_strips_field_codes() {
        assert_eq!(
            parse_exec("launcher --title \"My session\" %U %%").unwrap(),
            ["launcher", "--title", "My session", "%"]
        );
    }

    #[test]
    fn ignores_hidden_entries() {
        assert!(parse(
            "[Desktop Entry]\nType=Application\nHidden=true\nExec=session\n",
            Path::new("hidden.desktop")
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn rejects_relative_path_executables() {
        assert!(parse_exec("bin/session").is_err());
    }
}
