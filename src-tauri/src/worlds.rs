use crate::{error, files, safety, Result, Shared};
use serde::Serialize;
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
};
use tauri::State;

const ARCHIVE_LIMIT: u64 = 32 * 1024 * 1024 * 1024;
const ENTRY_LIMIT: usize = 100_000;
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct World {
    name: String,
    active: bool,
    generated: bool,
    nether: bool,
    end: bool,
}

pub(crate) fn world_name(value: &str) -> Result<()> {
    files::name(value)?;
    if value.len() > 48
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || [
            "plugins",
            "mods",
            "libraries",
            "versions",
            "config",
            "logs",
            "cache",
            "crash-reports",
        ]
        .contains(&value.to_ascii_lowercase().as_str())
    {
        return Err("World name: 1–48 letters, digits, underscores or hyphens; system directories are reserved".into());
    }
    Ok(())
}
fn properties(root: &Path) -> Result<String> {
    let path = files::root_path(root, "server.properties")?;
    let file = std::fs::File::open(path).map_err(error)?;
    let mut value = String::new();
    file.take(65537).read_to_string(&mut value).map_err(error)?;
    if value.len() > 65536 {
        return Err("Properties exceed 64 KiB".into());
    }
    Ok(value)
}
fn active(root: &Path) -> Result<String> {
    let content = properties(root)?;
    let selected = crate::properties::values(&content)?
        .get("level-name")
        .cloned()
        .unwrap_or_else(|| "world".into());
    world_name(&selected)?;
    Ok(selected)
}
fn update_properties(root: &Path, port: u16, values: &[(&str, &str)]) -> Result<()> {
    let old = properties(root)?;
    let content = crate::properties::patch(&old, values)?;
    safety::properties(&content, port)?;
    let path = files::root_path(root, "server.properties")?;
    files::backup(root, &path, "file-backups")?;
    safety::atomic_write(&path, content.as_bytes())
}
fn list(root: &Path) -> Result<Vec<World>> {
    let active = active(root)?;
    let mut worlds = Vec::new();
    for entry in std::fs::read_dir(root).map_err(error)? {
        let entry = entry.map_err(error)?;
        if !entry.file_type().map_err(error)?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if world_name(&name).is_err() {
            continue;
        }
        let path = files::root_path(root, &name)?;
        let generated = files::root_path(root, &format!("{name}/level.dat"))?.is_file();
        if !path.is_dir() || (!generated && !name.eq_ignore_ascii_case(&active)) {
            continue;
        }
        let nether = files::root_path(root, &format!("{name}/DIM-1"))?.is_dir();
        let end = files::root_path(root, &format!("{name}/DIM1"))?.is_dir();
        worlds.push(World {
            active: name.eq_ignore_ascii_case(&active),
            name,
            generated,
            nether,
            end,
        });
        if worlds.len() > 256 {
            return Err("Too many world directories".into());
        }
    }
    worlds.sort_by_key(|v| (!v.active, v.name.clone()));
    Ok(worlds)
}
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn staging(root: &Path) -> Result<Staging> {
    let parent = root.join(".minehost");
    safety::reject_links(&parent)?;
    std::fs::create_dir_all(&parent).map_err(error)?;
    let path = parent.join(format!("world-staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).map_err(error)?;
    Ok(Staging(path))
}
fn archive_name(value: &str) -> Result<()> {
    let value = value.trim_end_matches('/');
    if value.is_empty() || value.split('/').count() > 32 {
        return Err("Invalid archive depth".into());
    }
    for part in value.split('/') {
        files::name(part)?;
        if part.eq_ignore_ascii_case(".minehost") {
            return Err("Private application files are not world data".into());
        }
    }
    let extension = Path::new(value)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if [
        "exe", "dll", "com", "bat", "cmd", "ps1", "vbs", "js", "jar", "lnk", "url", "msi",
    ]
    .contains(&extension.as_str())
    {
        return Err("Executable files are not permitted in world archives".into());
    }
    Ok(())
}
fn import(root: &Path, source: &Path, target: &str) -> Result<()> {
    world_name(target)?;
    let destination = files::root_path(root, target)?;
    if destination.exists() {
        return Err("World directory already exists; choose another name".into());
    }
    safety::reject_links(source)?;
    if source.is_dir(){return import_folder(root,source,&destination);}
    let input = std::fs::File::open(source).map_err(error)?;
    if input.metadata().map_err(error)?.len() > ARCHIVE_LIMIT {
        return Err("Archive exceeds 32 GiB".into());
    }
    let mut zip = zip::ZipArchive::new(input).map_err(error)?;
    if zip.len() > ENTRY_LIMIT {
        return Err("World archive exceeds 100000 entries".into());
    }
    let stage = staging(root)?;
    let mut names = HashSet::new();
    let mut total = 0_u64;
    // Validate every entry before creating any extracted files.
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(error)?;
        archive_name(entry.name())?;
        if entry.unix_mode().is_some_and(|v| v & 0o170000 == 0o120000) {
            return Err("Archive links are forbidden".into());
        }
        if !names.insert(entry.name().trim_end_matches('/').to_lowercase()) {
            return Err("Duplicate or case-colliding archive paths".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Archive size overflow")?;
        if total > ARCHIVE_LIMIT {
            return Err("Expanded world exceeds 32 GiB".into());
        }
    }
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(error)?;
        let path = stage.0.join(entry.name().trim_end_matches('/'));
        if entry.is_dir() {
            std::fs::create_dir_all(path).map_err(error)?;
            continue;
        }
        std::fs::create_dir_all(path.parent().ok_or("Missing archive parent")?).map_err(error)?;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(error)?;
        let expected = entry.size();
        let copied = std::io::copy(
            &mut (&mut entry).take(expected.saturating_add(1)),
            &mut file,
        )
        .map_err(error)?;
        if copied != expected {
            return Err("Archive entry length mismatch".into());
        }
        file.sync_all().map_err(error)?;
    }
    let extracted = if stage.0.join("level.dat").is_file() {
        stage.0.clone()
    } else {
        let children = std::fs::read_dir(&stage.0)
            .map_err(error)?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(error)?;
        if children.len() != 1 || !children[0].path().join("level.dat").is_file() {
            return Err("ZIP must contain level.dat at its root or inside one world folder".into());
        }
        children[0].path()
    };
    std::fs::rename(extracted, destination).map_err(error)
}
fn import_folder(root:&Path,source:&Path,destination:&Path)->Result<()> {
    if !source.join("level.dat").is_file(){return Err("Select the world folder containing level.dat".into());}
    let names=crate::archive::tree(source)?;
    for name in &names {archive_name(name)?;}
    let stage=staging(root)?;
    let mut total=0_u64;
    for name in names {
        let input=source.join(&name);safety::reject_links(&input)?;
        let mut file=std::fs::File::open(&input).map_err(error)?;let expected=file.metadata().map_err(error)?.len();
        total=total.checked_add(expected).ok_or("World size overflow")?;if total>ARCHIVE_LIMIT{return Err("World exceeds 32 GiB".into());}
        let output=stage.0.join(&name);std::fs::create_dir_all(output.parent().ok_or("Missing world parent")?).map_err(error)?;
        let mut output=std::fs::OpenOptions::new().create_new(true).write(true).open(output).map_err(error)?;
        if std::io::copy(&mut (&mut file).take(expected.saturating_add(1)),&mut output).map_err(error)?!=expected {return Err("World source changed while copying; retry with a stopped source server".into());}
        output.sync_all().map_err(error)?;
    }
    std::fs::rename(&stage.0,destination).map_err(error)
}
fn export(root: &Path, world: &str, destination: &Path) -> Result<()> {
    world_name(world)?;
    let source = files::root_path(root, world)?;
    if !files::root_path(root, &format!("{world}/level.dat"))?.is_file() {
        return Err("World has not been generated yet".into());
    }
    safety::reject_links(destination)?;
    if destination
        .extension()
        .and_then(|v| v.to_str())
        .is_none_or(|v| !v.eq_ignore_ascii_case("zip"))
    {
        return Err("Export requires a new .zip filename".into());
    }
    let parent = destination
        .parent()
        .ok_or("Missing export directory")?
        .canonicalize()
        .map_err(error)?;
    if parent.starts_with(root.canonicalize().map_err(error)?) {
        return Err("Export outside the server directory".into());
    }
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(error)?;
    let result = (|| {
        let mut zip = zip::ZipWriter::new(file);
        let mut pending = vec![(source.clone(), 0_u8)];
        let mut total = 0_u64;
        let mut count = 0;
        while let Some((directory, depth)) = pending.pop() {
            if depth > 32 {
                return Err("World exceeds 32 directory levels".into());
            }
            for entry in std::fs::read_dir(directory).map_err(error)? {
                let path = entry.map_err(error)?.path();
                safety::reject_links(&path)?;
                let name = path
                    .strip_prefix(&source)
                    .map_err(error)?
                    .to_string_lossy()
                    .replace('\\', "/");
                archive_name(&name)?;
                count += 1;
                if count > ENTRY_LIMIT {
                    return Err("World exceeds 100000 entries".into());
                }
                if path.is_dir() {
                    zip.add_directory(format!("{name}/"), zip::write::SimpleFileOptions::default())
                        .map_err(error)?;
                    pending.push((path, depth + 1));
                } else if path.is_file() {
                    total = total
                        .checked_add(std::fs::metadata(&path).map_err(error)?.len())
                        .ok_or("World size overflow")?;
                    if total > ARCHIVE_LIMIT {
                        return Err("World exceeds 32 GiB".into());
                    }
                    zip.start_file(
                        name,
                        zip::write::SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Deflated),
                    )
                    .map_err(error)?;
                    std::io::copy(&mut std::fs::File::open(path).map_err(error)?, &mut zip)
                        .map_err(error)?;
                }
            }
        }
        zip.finish().map_err(error)?.sync_all().map_err(error)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}
#[tauri::command]
pub async fn list_worlds(state: State<'_, Shared>, id: String) -> Result<Vec<World>> {
    crate::profile_task(state.inner().clone(), id, false, move |_, root| list(&root)).await
}
#[tauri::command]
pub async fn configure_world(
    state: State<'_, Shared>,
    id: String,
    name: String,
    seed: Option<String>,
    generation: String,
    difficulty: String,
    game_mode: String,
) -> Result<()> {
    crate::profile_task(state.inner().clone(), id, true, move |server, root| {
        world_name(&name)?;
        let dir = files::root_path(&root, &name)?;
        if !["normal", "flat", "large_biomes", "amplified"].contains(&generation.as_str())
            || !["peaceful", "easy", "normal", "hard"].contains(&difficulty.as_str())
            || !["survival", "creative", "adventure", "spectator"].contains(&game_mode.as_str())
        {
            return Err("Invalid world settings".into());
        }
        if let Some(seed) = seed {
            if dir.exists() {
                return Err("For a new seed choose a new, unused world name".into());
            }
            if seed.len() > 128 || seed.contains('\\') || seed.chars().any(char::is_control) {
                return Err("Invalid seed".into());
            }
            std::fs::create_dir(&dir).map_err(error)?;
            if let Err(e) = update_properties(
                &root,
                server.port,
                &[
                    ("level-name", &name),
                    ("level-seed", &seed),
                    ("level-type", &format!("minecraft:{generation}")),
                    ("difficulty", &difficulty),
                    ("gamemode", &game_mode),
                ],
            ) {
                let _ = std::fs::remove_dir(dir);
                return Err(e);
            }
        } else {
            if !files::root_path(&root, &format!("{name}/level.dat"))?.is_file() {
                return Err("Choose a generated or imported world".into());
            }
            update_properties(&root, server.port, &[("level-name", &name)])?;
        }
        Ok(())
    })
    .await
}
#[tauri::command]
pub async fn import_world(
    state: State<'_, Shared>,
    id: String,
    source: String,
    name: String,
) -> Result<()> {
    crate::profile_task(state.inner().clone(), id, true, move |_, root| {
        import(&root, Path::new(&source), &name)
    })
    .await
}
#[tauri::command]
pub async fn export_world(
    state: State<'_, Shared>,
    id: String,
    name: String,
    destination: String,
) -> Result<()> {
    crate::profile_task(state.inner().clone(), id, true, move |_, root| {
        export(&root, &name, Path::new(&destination))
    })
    .await
}
#[tauri::command]
pub async fn trash_world(
    state: State<'_, Shared>,
    id: String,
    name: String,
    confirmation: String,
) -> Result<()> {
    crate::profile_task(state.inner().clone(), id, true, move |_, root| {
        trash(&root, &name, &confirmation)
    })
    .await
}
fn trash(root: &Path, name: &str, confirmation: &str) -> Result<()> {
    world_name(name)?;
    if confirmation != name || active(root)?.eq_ignore_ascii_case(name) {
        return Err(
            "Confirm the world name; select a different world before removing the active one"
                .into(),
        );
    }
    let path = files::root_path(root, name)?;
    if !files::root_path(root, &format!("{name}/level.dat"))?.is_file() {
        return Err("World data not found".into());
    }
    let trash = root.join(".minehost/world-trash");
    safety::reject_links(&trash)?;
    std::fs::create_dir_all(&trash).map_err(error)?;
    std::fs::rename(path, trash.join(format!("{}-{name}", uuid::Uuid::new_v4()))).map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_folder_without_altering_source_or_replacing_existing_world(){
        let stage=crate::archive::Stage::new(&std::env::temp_dir()).unwrap();let source=stage.0.join("source");let profile=stage.0.join("profile");std::fs::create_dir_all(source.join("region")).unwrap();std::fs::create_dir(&profile).unwrap();std::fs::write(source.join("level.dat"),b"world data").unwrap();std::fs::write(source.join("region/r.0.0.mca"),b"region").unwrap();
        import(&profile,&source,"uploaded").unwrap();assert_eq!(std::fs::read(profile.join("uploaded/level.dat")).unwrap(),b"world data");assert_eq!(std::fs::read(source.join("region/r.0.0.mca")).unwrap(),b"region");assert!(import(&profile,&source,"uploaded").is_err());
        std::fs::write(source.join("run.jar"),b"executable").unwrap();assert!(import(&profile,&source,"unsafe").is_err());assert!(!profile.join("unsafe").exists());
    }
    use std::io::Write;
    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!("minehost-worlds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("server.properties"),
            "server-port=25565\nonline-mode=true\n",
        )
        .unwrap();
        path
    }
    fn archive(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    #[test]
    fn imports_exports_without_overwriting_or_escaping() {
        let root = temp();
        let source = root.join("input.zip");
        archive(
            &source,
            &[
                ("old/level.dat", b"world-data"),
                ("old/DIM-1/region/r.0.0.mca", b"region"),
            ],
        );
        import(&root, &source, "survival").unwrap();
        assert_eq!(
            std::fs::read(root.join("survival/level.dat")).unwrap(),
            b"world-data"
        );
        assert!(import(&root, &source, "survival").is_err());
        update_properties(&root, 25565, &[("level-name", "survival")]).unwrap();
        let items = list(&root).unwrap();
        assert!(items[0].active && items[0].nether);
        let out =
            std::env::temp_dir().join(format!("minehost-export-{}.zip", uuid::Uuid::new_v4()));
        export(&root, "survival", &out).unwrap();
        import(&root, &out, "copy").unwrap();
        assert_eq!(
            std::fs::read(root.join("copy/level.dat")).unwrap(),
            b"world-data"
        );
        assert!(export(&root, "survival", &out).is_err());
    }
    #[test]
    fn removing_world_requires_confirmation_and_preserves_data() {
        let root = temp();
        std::fs::create_dir(root.join("world")).unwrap();
        std::fs::write(root.join("world/level.dat"), b"original").unwrap();
        assert!(trash(&root, "world", "world").is_err());
        update_properties(&root, 25565, &[("level-name", "other")]).unwrap();
        assert!(trash(&root, "world", "wrong").is_err());
        assert!(root.join("world/level.dat").is_file());
        trash(&root, "world", "world").unwrap();
        assert!(!root.join("world").exists());
        let retained = std::fs::read_dir(root.join(".minehost/world-trash"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read(retained.join("level.dat")).unwrap(),
            b"original"
        );
    }
    #[test]
    fn rejects_unsafe_archives_without_leaving_a_world() {
        let root = temp();
        let source = root.join("bad.zip");
        for bad in [
            "../escape",
            "a/../../escape",
            "C:/escape",
            "CON.txt",
            "a\\b",
            "a./b",
            "run.jar",
            ".minehost/x",
        ] {
            archive(&source, &[("level.dat", b"ok"), (bad, b"bad")]);
            assert!(import(&root, &source, "bad").is_err(), "{bad}");
            assert!(!root.join("bad").exists());
        }
        archive(&source, &[("level.dat", b"one"), ("LEVEL.DAT", b"two")]);
        assert!(import(&root, &source, "bad").is_err());
        archive(&source, &[("no-world.txt", b"no")]);
        assert!(import(&root, &source, "bad").is_err());
    }
}
