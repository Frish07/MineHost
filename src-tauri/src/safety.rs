use crate::{error, Result};
use std::{
    io::Write,
    path::{Component, Path},
};

pub fn version(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 40
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err("Invalid Minecraft version".into());
    }
    Ok(())
}

pub fn command(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err("Commands must be a single printable line, up to 1024 bytes".into());
    }
    Ok(())
}

pub fn reject_links(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("Symbolic links are not allowed".into());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err("Reparse points are not allowed".into());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    reject_links(path)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(error)?;
        file.write_all(bytes).map_err(error)?;
        file.sync_all().map_err(error)?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(error)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub fn properties(content: &str, port: u16) -> Result<()> {
    if content.len() > 65536 || content.contains('\0') {
        return Err("Properties file is too large or contains NUL".into());
    }
    let mut found_port = false;
    let mut found_online = false;
    for line in content
        .lines()
        .map(str::trim)
        .filter(|s| !s.starts_with('#') && !s.starts_with('!') && !s.is_empty())
    {
        // Keep this editor a strict subset of Java Properties: no escapes or continued lines.
        if line.contains('\\') {
            return Err("Escapes and line continuations are unsupported in this editor".into());
        }
        let (key, value) = line.split_once('=').ok_or("Use key=value syntax")?;
        match key.trim() {
            "server-port" => {
                if found_port || value.trim().parse::<u16>().ok() != Some(port) {
                    return Err("Keep the unique profile server-port unchanged".into());
                }
                found_port = true;
            }
            "online-mode" => {
                if found_online || value.trim() != "true" {
                    return Err("This version requires online-mode=true".into());
                }
                found_online = true;
            }
            "level-name" => {
                if value.trim().is_empty()
                    || !value
                        .trim()
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    return Err("World name must be a simple directory name".into());
                }
            }
            "enable-rcon" | "enable-query" | "enable-jmx-monitoring" if value.trim() != "false" => {
                return Err("Additional network listeners are not supported in this version".into())
            }
            _ => (),
        }
    }
    if !found_port || !found_online {
        return Err("server-port and online-mode=true are required".into());
    }
    Ok(())
}

pub fn zip_path(name: &str) -> Result<&Path> {
    let path = Path::new(name);
    if name.contains('\\')
        || name.contains(':')
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || name
            .split('/')
            .any(|c| c == ".." || c.ends_with('.') || c.ends_with(' '))
    {
        return Err("Unsafe archive path".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_command_injection() {
        for value in ["", "stop\nsay hacked", "say\rstop", "say\0a"] {
            assert!(command(value).is_err());
        }
        assert!(command("say Hello world").is_ok());
    }
    #[test]
    fn rejects_archive_traversal() {
        for value in [
            "../world",
            "/root",
            "C:/Windows/x",
            "a/../../x",
            "a\\..\\x",
            "file:stream",
            "a./x",
        ] {
            assert!(zip_path(value).is_err(), "{value}");
        }
        assert!(zip_path("jdk-21/bin/java.exe").is_ok());
    }
    #[test]
    fn enforces_port_and_authentication() {
        assert!(properties("server-port=25565\nonline-mode=true\n", 25565).is_ok());
        for value in [
            "server-port=25566\nonline-mode=true",
            "server-port=25565\nonline-mode=false",
            "server-port=25565\nonline-mode=true\nlevel-name=../escape",
            "server-port=25565\nonline-mode=true\nserver-port=25566",
            "server-port=25565\nonline-mode=true\nenable-rcon=true",
        ] {
            assert!(properties(value, 25565).is_err());
        }
    }
    #[test]
    fn rejects_version_path_injection() {
        assert!(version("1.21.11").is_ok());
        assert!(version("../../x?query").is_err());
    }
}
