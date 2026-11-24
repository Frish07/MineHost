use crate::{error, safety, Result, Shared};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
use tauri::State;

const TEXT_LIMIT: u64 = 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    name: String,
    directory: bool,
    size: u64,
    editable: bool,
}
#[derive(Serialize)]
pub struct Document {
    content: String,
    revision: String,
}

pub fn name(value: &str) -> Result<()> {
    let stem = value
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    if value.is_empty()
        || value.len() > 180
        || value == "."
        || value == ".."
        || value.ends_with(['.', ' '])
        || value.starts_with(' ')
        || value
            .chars()
            .any(|c| c.is_control() || "\\/:<>\"|?*".contains(c))
        || matches!(
            stem.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "CONIN$"
                | "CONOUT$"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
                | "COM¹"
                | "COM²"
                | "COM³"
                | "LPT¹"
                | "LPT²"
                | "LPT³"
        )
    {
        return Err("Invalid Windows file or directory name".into());
    }
    Ok(())
}
pub fn root_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.len() > 1024 {
        return Err("Path is too long".into());
    }
    let mut result = root.to_path_buf();
    if !relative.is_empty() {
        for part in relative.split('/') {
            name(part)?;
            if part.eq_ignore_ascii_case(".minehost") {
                return Err(
                    "MineHost private data cannot be accessed from the file manager".into(),
                );
            }
            result.push(part);
        }
    }
    safety::reject_links(&result)?;
    Ok(result)
}
fn text_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "txt" | "log" | "yaml" | "yml" | "json" | "toml" | "properties" | "cfg" | "conf" | "mcmeta"
    )
}
fn writable(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = root_path(root, relative)?;
    if !text_file(&path) || relative.eq_ignore_ascii_case("eula.txt") {
        return Err("Only text configuration files can be changed here; use the software/addons/world panels for other files".into());
    }
    Ok(path)
}
pub fn backup(root: &Path, path: &Path, category: &str) -> Result<PathBuf> {
    safety::reject_links(path)?;
    let folder = root.join(".minehost").join(category);
    safety::reject_links(&folder)?;
    std::fs::create_dir_all(&folder).map_err(error)?;
    let target = folder.join(format!(
        "{}-{}",
        uuid::Uuid::new_v4(),
        path.file_name()
            .ok_or("Missing filename")?
            .to_string_lossy()
    ));
    let mut input = std::fs::File::open(path).map_err(error)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(error)?;
    std::io::copy(&mut input, &mut output).map_err(error)?;
    output.sync_all().map_err(error)?;
    Ok(target)
}
fn read(path: &Path) -> Result<Document> {
    safety::reject_links(path)?;
    if !text_file(path) {
        return Err("This file is not a supported text configuration".into());
    }
    let file = std::fs::File::open(path).map_err(error)?;
    if file.metadata().map_err(error)?.len() > TEXT_LIMIT {
        return Err("Text editor limit is 1 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(TEXT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > TEXT_LIMIT || bytes.contains(&0) {
        return Err("File is too large or contains binary data".into());
    }
    let revision = format!("{:x}", Sha256::digest(&bytes));
    Ok(Document {
        content: String::from_utf8(bytes).map_err(|_| "Only UTF-8 text files can be edited")?,
        revision,
    })
}
fn list(root: &Path, relative: &str) -> Result<Vec<Entry>> {
    let path = root_path(root, relative)?;
    let mut items = Vec::new();
    for entry in std::fs::read_dir(path).map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case(".minehost") {
            continue;
        }
        safety::reject_links(&entry.path())?;
        let meta = entry.metadata().map_err(error)?;
        items.push(Entry {
            name,
            directory: meta.is_dir(),
            size: meta.len(),
            editable: meta.is_file() && text_file(&entry.path()) && meta.len() <= TEXT_LIMIT,
        });
        if items.len() > 4096 {
            return Err("Directory exceeds 4096 entries".into());
        }
    }
    items.sort_by_key(|v| (!v.directory, v.name.to_ascii_lowercase()));
    Ok(items)
}
fn save(
    root: &Path,
    relative: &str,
    content: &str,
    revision: Option<&str>,
    port: u16,
) -> Result<()> {
    let path = writable(root, relative)?;
    if content.len() as u64 > TEXT_LIMIT || content.contains('\0') {
        return Err("Text editor limit is 1 MiB; NUL is not allowed".into());
    }
    if relative.eq_ignore_ascii_case("server.properties") {
        safety::properties(content, port)?;
    }
    if path.exists() {
        let current = read(&path)?;
        if revision != Some(current.revision.as_str()) {
            return Err("File changed since it was opened. Reload it before saving.".into());
        }
        backup(root, &path, "file-backups")?;
    } else if revision.is_some() {
        return Err("File was removed. Reload the directory.".into());
    }
    safety::atomic_write(&path, content.as_bytes())
}
#[tauri::command]
pub async fn list_files(
    state: State<'_, Shared>,
    id: String,
    relative: String,
) -> Result<Vec<Entry>> {
    crate::profile_task(state.inner().clone(), id, false, move |_, root| {
        list(&root, &relative)
    })
    .await
}
#[tauri::command]
pub async fn read_file(state: State<'_, Shared>, id: String, relative: String) -> Result<Document> {
    crate::profile_task(state.inner().clone(), id, false, move |_, root| {
        read(&root_path(&root, &relative)?)
    })
    .await
}
#[tauri::command]
pub async fn save_file(
    state: State<'_, Shared>,
    id: String,
    relative: String,
    content: String,
    revision: Option<String>,
) -> Result<()> {
    crate::profile_task(state.inner().clone(), id, true, move |server, root| {
        save(&root, &relative, &content, revision.as_deref(), server.port)
    })
    .await
}
#[tauri::command]
pub async fn file_action(
    state: State<'_, Shared>,
    id: String,
    relative: String,
    action: String,
    target: String,
) -> Result<()> {
    crate::profile_task(state.inner().clone(), id, true, move |_, root| {
        let path = root_path(&root, &relative)?;
        match action.as_str() {
            "mkdir" => {
                if relative.is_empty() {
                    return Err("Choose a directory name".into());
                }
                std::fs::create_dir(path).map_err(error)
            }
            "rename" | "trash" => {
                let path = writable(&root, &relative)?;
                if !path.is_file() {
                    return Err("Choose a text file, not a directory".into());
                }
                if relative.eq_ignore_ascii_case("server.properties") {
                    return Err("The main server properties file cannot be moved".into());
                }
                if action == "trash" {
                    if target != relative {
                        return Err("Confirm the exact file path".into());
                    }
                    let trash = backup(&root, &path, "file-trash")?;
                    // Keep the backup if deletion fails: never lose the only copy.
                    std::fs::remove_file(&path)
                        .map_err(|e| format!("{e}; retained copy: {}", trash.display()))
                } else {
                    name(&target)?;
                    let parent = relative
                        .rsplit_once('/')
                        .map(|(v, _)| format!("{v}/"))
                        .unwrap_or_default();
                    if parent.is_empty() && target.eq_ignore_ascii_case("server.properties") {
                        return Err(
                            "Use the properties editor to change the main server configuration"
                                .into(),
                        );
                    }
                    let destination = writable(&root, &format!("{parent}{target}"))?;
                    if destination.exists() {
                        return Err("Destination already exists".into());
                    }
                    std::fs::rename(path, destination).map_err(error)
                }
            }
            _ => Err("Unsupported file operation".into()),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("minehost-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn confines_windows_paths_and_private_files() {
        let root = temp();
        for value in [
            "../x",
            "a/../x",
            "C:/Windows",
            "a\\b",
            "a//b",
            "/x",
            "a/CON.txt",
            "a./x",
            ".minehost/x",
            "x/.MINEHOST/a",
            "file:stream",
            "LPT¹.txt",
        ] {
            assert!(root_path(&root, value).is_err(), "{value}");
        }
        assert!(root_path(&root, "plugins/Example/config.yml").is_ok());
        assert!(writable(&root, "eula.txt").is_err());
        assert!(writable(&root, "server.jar").is_err());
    }
    #[test]
    fn edits_detect_conflicts_and_keep_previous_bytes() {
        let root = temp();
        save(&root, "config.json", "{}", None, 25565).unwrap();
        let document = read(&root.join("config.json")).unwrap();
        std::fs::write(root.join("config.json"), "{\"changed\":true}").unwrap();
        assert!(save(&root, "config.json", "[]", Some(&document.revision), 25565).is_err());
        let current = read(&root.join("config.json")).unwrap();
        save(&root, "config.json", "[]", Some(&current.revision), 25565).unwrap();
        let backup = std::fs::read_dir(root.join(".minehost/file-backups"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read_to_string(backup).unwrap(),
            "{\"changed\":true}"
        );
        assert!(save(&root, "server.properties", "online-mode=false", None, 25565).is_err());
    }
}
