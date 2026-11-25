use crate::{error, lock, safety, Result, Shared};
use std::path::{Path, PathBuf};

fn erase_tree(path: &Path) -> Result<()> {
    let mut pending = vec![(path.to_path_buf(), false)];
    while let Some((item, visited)) = pending.pop() {
        if !item.starts_with(path) {
            return Err("Deletion escaped the selected profile".into());
        }
        // Check all ancestors before reading entries so a junction is never traversed.
        if let Some(parent) = item.parent() {
            safety::reject_links(parent)?;
        }
        let metadata = match std::fs::symlink_metadata(&item) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(error(e)),
        };
        #[cfg(windows)]
        let link = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let link = metadata.file_type().is_symlink();
        if metadata.is_dir() && !link {
            if visited {
                std::fs::remove_dir(&item).map_err(error)?;
            } else {
                pending.push((item.clone(), true));
                for entry in std::fs::read_dir(&item).map_err(error)? {
                    pending.push((entry.map_err(error)?.path(), false));
                }
            }
        } else if metadata.is_dir() {
            std::fs::remove_dir(item).map_err(error)?;
        } else {
            std::fs::remove_file(item).map_err(error)?;
        }
    }
    Ok(())
}
fn owned_paths(root: &Path, id: &str) -> Result<Vec<PathBuf>> {
    uuid::Uuid::parse_str(id).map_err(error)?;
    safety::reject_links(root)?;
    let mut paths = vec![root.join("servers").join(id)];
    for folder in ["backups", "trash"] {
        let base = root.join(folder);
        safety::reject_links(&base)?;
        if !base.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&base).map_err(error)? {
            let entry = entry.map_err(error)?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{id}-"))
            {
                paths.push(entry.path());
            }
        }
    }
    Ok(paths)
}
pub fn delete(shared: &Shared, id: &str, confirmation: &str) -> Result<()> {
    let _operation = crate::begin(shared, id)?;
    let root = {
        let mut inner = lock(shared)?;
        let server = crate::load(&inner, id)?;
        if confirmation != server.name {
            return Err("Type the exact server name to confirm permanent deletion".into());
        }
        if inner
            .agent
            .as_mut()
            .is_some_and(crate::playit::ManagedAgent::alive)
        {
            return Err("Stop the managed playit agent before deleting this server".into());
        }
        if inner.drive.cancel.is_some() {
            return Err(
                "Wait for Google Drive operations to finish before deleting a server".into(),
            );
        }
        inner.root.clone()
    };
    // Only exact UUID-owned local paths are erased. Shared runtimes/cache and remote
    // account resources do not belong exclusively to this profile.
    for path in owned_paths(&root, id)? {
        erase_tree(&path).map_err(|e|format!("Deletion incomplete: {e}. Stop programs using server files and retry; the profile record was retained."))?;
    }
    let mut inner = lock(shared)?;
    let tx = inner.db.unchecked_transaction().map_err(error)?;
    for table in [
        "connection_history",
        "network_settings",
        "backup_schedules",
        "restart_policies",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE server_id=?1"), [id])
            .map_err(error)?;
    }
    tx.execute("DELETE FROM playit_requests WHERE profile_id=?1", [id])
        .map_err(error)?;
    tx.execute("DELETE FROM servers WHERE id=?1", [id])
        .map_err(error)?;
    tx.commit().map_err(error)?;
    inner.lifecycle.remove(id);
    inner.maintenance.remove(id);
    inner.logs.remove(id);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permanent_delete_removes_only_owned_files_and_requires_exact_confirmation() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let shared = crate::tests::memory_state();
        let id = {
            let mut inner = lock(&shared).unwrap();
            inner.root = stage.0.clone();
            for schema in [
                crate::history::SCHEMA,
                crate::network::SCHEMA,
                crate::backup_schedule::SCHEMA,
                crate::lifecycle::SCHEMA,
            ] {
                inner.db.execute_batch(schema).unwrap();
            }
            inner
                .db
                .query_row("SELECT id FROM servers", [], |r| r.get::<_, String>(0))
                .unwrap()
        };
        let name = crate::load(&lock(&shared).unwrap(), &id).unwrap().name;
        let folder = stage.0.join("servers").join(&id);
        std::fs::create_dir_all(folder.join("world")).unwrap();
        std::fs::write(folder.join("world/level.dat"), b"world").unwrap();
        std::fs::create_dir_all(stage.0.join("backups")).unwrap();
        std::fs::create_dir_all(stage.0.join("trash").join(format!("{id}-old"))).unwrap();
        std::fs::write(
            stage.0.join("backups").join(format!("{id}-backup.zip")),
            b"backup",
        )
        .unwrap();
        std::fs::write(stage.0.join("backups/other.zip"), b"other").unwrap();
        assert!(delete(&shared, &id, "incorrect").is_err());
        assert!(folder.join("world/level.dat").exists());
        delete(&shared, &id, &name).unwrap();
        assert!(!folder.exists());
        assert!(stage.0.join("backups/other.zip").exists());
        assert!(!stage
            .0
            .join("backups")
            .join(format!("{id}-backup.zip"))
            .exists());
        assert!(!stage.0.join("trash").join(format!("{id}-old")).exists());
        assert!(crate::load(&lock(&shared).unwrap(), &id).is_err());
    }
    #[test]
    fn junction_deletion_keeps_external_target_files() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let selected = stage.0.join("selected");
        let outside = stage.0.join("outside");
        std::fs::create_dir(&selected).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let junction = selected.join("junction");
        let status = std::process::Command::new("cmd.exe")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .status()
            .unwrap();
        assert!(status.success());
        erase_tree(&selected).unwrap();
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
        assert!(!selected.exists());
    }
}
