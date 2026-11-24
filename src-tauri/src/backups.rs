use crate::{archive, error, files, safety, Result, Server, Shared};
use serde::Serialize;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tauri::State;
const MANIFEST: &str = ".minehost-backup.json";
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    name: String,
    size: u64,
    modified_at: u64,
}
#[derive(Serialize)]
pub struct Preview {
    files: usize,
    size: u64,
    worlds: Vec<String>,
    software: Option<String>,
    minecraft: Option<String>,
}
pub(crate) fn source(root: &Path, id: &str, name: &str) -> Result<PathBuf> {
    files::name(name)?;
    if !name.starts_with(&format!("{id}-")) || !name.ends_with(".zip") {
        return Err("Backup does not belong to this profile".into());
    }
    let path = root.join("backups").join(name);
    safety::reject_links(&path)?;
    Ok(path)
}
pub fn create(root: &Path, server: &Server, profile: &Path) -> Result<PathBuf> {
    let folder = root.join("backups");
    safety::reject_links(&folder)?;
    std::fs::create_dir_all(&folder).map_err(error)?;
    let path = folder.join(format!("{}-{}.zip", server.id, uuid::Uuid::new_v4()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(error)?;
    let result = (|| {
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(MANIFEST, options).map_err(error)?;
        writer
            .write_all(&serde_json::to_vec(server).map_err(error)?)
            .map_err(error)?;
        for name in archive::tree(profile)? {
            if name == MANIFEST {
                continue;
            }
            writer.start_file(&name, options).map_err(error)?;
            std::io::copy(
                &mut std::fs::File::open(profile.join(name)).map_err(error)?,
                &mut writer,
            )
            .map_err(error)?;
        }
        writer.finish().map_err(error)?.sync_all().map_err(error)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result?;
    Ok(path)
}
fn metadata(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<Option<Server>> {
    match zip.by_name(MANIFEST) {
        Ok(file) => {
            let mut text = String::new();
            file.take(65537).read_to_string(&mut text).map_err(error)?;
            if text.len() > 65536 {
                return Err("Invalid backup manifest".into());
            }
            Ok(Some(serde_json::from_str(&text).map_err(error)?))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(error(e)),
    }
}
pub(crate) fn preview(path: &Path) -> Result<Preview> {
    let mut zip = archive::open(path)?;
    let entries = archive::entries(&mut zip)?;
    let profile = metadata(&mut zip)?;
    let mut worlds = entries
        .iter()
        .filter_map(|(v, _)| v.strip_suffix("/level.dat"))
        .filter(|v| !v.contains('/') && crate::worlds::world_name(v).is_ok())
        .map(str::to_string)
        .collect::<Vec<_>>();
    worlds.sort();
    Ok(Preview {
        files: entries.len(),
        size: entries.iter().map(|(_, size)| *size).sum(),
        worlds,
        software: profile.as_ref().map(|v| format!("{:?}", v.software)),
        minecraft: profile.map(|v| v.version),
    })
}
#[tauri::command]
pub async fn list_backups(state: State<'_, Shared>, id: String) -> Result<Vec<Backup>> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let inner = crate::lock(&shared)?;
        crate::load(&inner, &id)?;
        let folder = inner.root.join("backups");
        drop(inner);
        safety::reject_links(&folder)?;
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(folder).map_err(error)? {
            let path = entry.map_err(error)?.path();
            safety::reject_links(&path)?;
            let name = path
                .file_name()
                .ok_or("Invalid filename")?
                .to_string_lossy()
                .into_owned();
            if name.starts_with(&format!("{id}-")) && name.ends_with(".zip") && path.is_file() {
                let metadata = std::fs::metadata(path).map_err(error)?;
                let modified_at = metadata
                    .modified()
                    .ok()
                    .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|v| v.as_secs())
                    .unwrap_or(0);
                rows.push(Backup {
                    name,
                    size: metadata.len(),
                    modified_at,
                });
            }
            if rows.len() > 4096 {
                return Err("Too many backups".into());
            }
        }
        rows.sort_by_key(|v| std::cmp::Reverse(v.modified_at));
        Ok(rows)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn inspect_backup(state: State<'_, Shared>, id: String, name: String) -> Result<Preview> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let path = {
            let inner = crate::lock(&shared)?;
            crate::load(&inner, &id)?;
            source(&inner.root, &id, &name)?
        };
        preview(&path)
    })
    .await
    .map_err(error)?
}
fn restore(root: &Path, server: &Server, path: &Path, world: Option<&str>) -> Result<()> {
    let profile = root.join("servers").join(&server.id);
    safety::reject_links(&profile)?;
    let mut zip = archive::open(path)?;
    archive::entries(&mut zip)?;
    let saved = metadata(&mut zip)?;
    if let Some(saved) = &saved {
        if saved.software != server.software || saved.version != server.version {
            return Err(
                "Backup core/Minecraft version differs; restore into a compatible profile".into(),
            );
        }
    }
    drop(zip);
    let stage = archive::Stage::new(&root.join("cache/restores"))?;
    archive::extract(path, &stage.0)?;
    if let Some(world) = world {
        crate::worlds::world_name(world)?;
        let source = stage.0.join(world);
        safety::reject_links(&source.join("level.dat"))?;
        if !source.join("level.dat").is_file() {
            return Err("World is absent from this backup".into());
        }
        let destination = profile.join(world);
        safety::reject_links(&destination)?;
        create(root, server, &profile)?;
        let trash = profile.join(".minehost/world-trash");
        safety::reject_links(&trash)?;
        std::fs::create_dir_all(&trash).map_err(error)?;
        let retained = trash.join(format!("{}-{world}", uuid::Uuid::new_v4()));
        let existed = destination.exists();
        if existed {
            std::fs::rename(&destination, &retained).map_err(error)?;
        }
        if let Err(e) = std::fs::rename(source, &destination) {
            if existed {
                std::fs::rename(&retained, &destination).map_err(|rollback| {
                    format!(
                        "{e}; rollback failed: {rollback}; retained: {}",
                        retained.display()
                    )
                })?;
            }
            return Err(error(e));
        }
        return Ok(());
    }
    if !stage.0.join(server.software.launcher()).is_file() {
        return Err("Backup lacks this profile's installed server software".into());
    }
    if matches!(
        server.software,
        crate::software::Software::Forge | crate::software::Software::Neoforge
    ) {
        crate::forge::launch_arguments(&stage.0, server.software, &server.version)?;
    }
    let properties = stage.0.join("server.properties");
    safety::reject_links(&properties)?;
    let text = std::fs::read_to_string(&properties).map_err(error)?;
    let mut lines = text
        .lines()
        .filter(|v| {
            !v.split_once('=')
                .is_some_and(|(k, _)| matches!(k.trim(), "server-port" | "online-mode"))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    lines.push(format!("server-port={}", server.port));
    lines.push("online-mode=true".into());
    let text = lines.join("\n") + "\n";
    safety::properties(&text, server.port)?;
    safety::atomic_write(&properties, text.as_bytes())?;
    safety::atomic_write(
        &stage.0.join("eula.txt"),
        if server.eula {
            b"eula=true\n"
        } else {
            b"eula=false\n"
        },
    )?;
    if stage.0.join(MANIFEST).exists() {
        std::fs::remove_file(stage.0.join(MANIFEST)).map_err(error)?;
    }
    create(root, server, &profile)?;
    let trash = root.join("trash");
    safety::reject_links(&trash)?;
    std::fs::create_dir_all(&trash).map_err(error)?;
    let retained = trash.join(format!("{}-restore-{}", server.id, uuid::Uuid::new_v4()));
    std::fs::rename(&profile, &retained).map_err(error)?;
    if let Err(e) = std::fs::rename(&stage.0, &profile) {
        std::fs::rename(&retained, &profile).map_err(|rollback| {
            format!(
                "{e}; rollback failed: {rollback}; original profile: {}",
                retained.display()
            )
        })?;
        return Err(error(e));
    }
    Ok(())
}
#[tauri::command]
pub async fn restore_backup(
    state: State<'_, Shared>,
    id: String,
    name: String,
    world: Option<String>,
    confirmation: String,
    consent: bool,
) -> Result<()> {
    if !consent {
        return Err("Confirm trust in the executable mod/plugin files inside this backup".into());
    }
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = crate::begin(&shared, &id)?;
        let (server, root, path) = {
            let inner = crate::lock(&shared)?;
            let server = crate::load(&inner, &id)?;
            if confirmation != server.name {
                return Err("Type the exact server name to confirm restoration".into());
            }
            let path = source(&inner.root, &id, &name)?;
            (server, inner.root.clone(), path)
        };
        restore(&root, &server, &path, world.as_deref())?;
        if world.is_none() {
            let inner = crate::lock(&shared)?;
            let mut updated = server;
            updated.installed = true;
            crate::save(&inner, &updated)?;
        }
        Ok(())
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn import_backup(state: State<'_, Shared>, id: String, source: String) -> Result<String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = crate::begin(&shared, &id)?;
        let (server, root) = {
            let inner = crate::lock(&shared)?;
            (crate::load(&inner, &id)?, inner.root.clone())
        };
        import_local(&root, &server, Path::new(&source))
    })
    .await
    .map_err(error)?
}
pub trait BackupProvider {
    fn store(&self, source: &Path) -> Result<String>;
}
pub struct LocalBackupProvider<'a> {
    pub root: &'a Path,
    pub server: &'a Server,
}
impl BackupProvider for LocalBackupProvider<'_> {
    fn store(&self, source: &Path) -> Result<String> {
        import_local(self.root, self.server, source)
    }
}
pub fn import_local(root: &Path, server: &Server, source: &Path) -> Result<String> {
    let mut zip = archive::open(source)?;
    archive::entries(&mut zip)?;
    if let Some(saved) = metadata(&mut zip)? {
        if saved.software != server.software || saved.version != server.version {
            return Err("Import into a profile with the same core and Minecraft version".into());
        }
    }
    drop(zip);
    let folder = root.join("backups");
    safety::reject_links(&folder)?;
    std::fs::create_dir_all(&folder).map_err(error)?;
    let name = format!("{}-{}.zip", server.id, uuid::Uuid::new_v4());
    let target = folder.join(&name);
    let result = (|| {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(error)?;
        let input = std::fs::File::open(source).map_err(error)?;
        let size =
            std::io::copy(&mut input.take(archive::LIMIT + 1), &mut output).map_err(error)?;
        if size > archive::LIMIT {
            return Err("Backup exceeds 32 GiB".into());
        }
        output.sync_all().map_err(error)?;
        drop(output);
        preview(&target)?;
        Ok(name)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&target);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_preserves_port_consent_and_previous_server() {
        let root = archive::Stage::new(&std::env::temp_dir()).unwrap();
        let server = Server {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Test".into(),
            version: "1.21.1".into(),
            port: 25566,
            ram_mb: 2048,
            eula: false,
            installed: true,
            java_major: 21,
            software: crate::software::Software::Paper,
            loader_version: None,
        };
        let profile = root.0.join("servers").join(&server.id);
        std::fs::create_dir_all(profile.join("world")).unwrap();
        std::fs::write(profile.join("server.jar"), b"server").unwrap();
        std::fs::write(profile.join("world/level.dat"), b"old-world").unwrap();
        std::fs::write(
            profile.join("server.properties"),
            "server-port=25565\nonline-mode=true\n",
        )
        .unwrap();
        std::fs::write(profile.join("eula.txt"), "eula=true").unwrap();
        let backup = create(&root.0, &server, &profile).unwrap();
        std::fs::write(profile.join("world/level.dat"), b"new-world").unwrap();
        restore(&root.0, &server, &backup, Some("world")).unwrap();
        assert_eq!(
            std::fs::read(profile.join("world/level.dat")).unwrap(),
            b"old-world"
        );
        restore(&root.0, &server, &backup, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(profile.join("eula.txt")).unwrap(),
            "eula=false\n"
        );
        assert!(std::fs::read_to_string(profile.join("server.properties"))
            .unwrap()
            .contains("server-port=25566"));
        assert!(std::fs::read_dir(root.0.join("trash"))
            .unwrap()
            .next()
            .is_some());
    }
}
