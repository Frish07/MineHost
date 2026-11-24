use crate::{
    addons, archive, error, forge, runtime, safety, software::Software, Result, Server, Shared,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::Path,
};
use tauri::State;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    format_version: u32,
    game: String,
    name: String,
    version_id: String,
    dependencies: HashMap<String, String>,
    files: Vec<PackFile>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackFile {
    path: String,
    hashes: HashMap<String, String>,
    downloads: Vec<String>,
    file_size: u64,
    env: Option<HashMap<String, String>>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    path: String,
    size: u64,
    optional: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    name: String,
    version: String,
    dependencies: HashMap<String, String>,
    files: Vec<Item>,
    fingerprint: String,
}
fn fingerprint(path: &Path) -> Result<String> {
    safety::reject_links(path)?;
    let mut file = std::fs::File::open(path).map_err(error)?;
    if file.metadata().map_err(error)?.len() > 2 * 1024 * 1024 * 1024 {
        return Err("Modpack archive exceeds 2 GiB".into());
    }
    let mut hash = Sha512::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(error)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn destination(path: &str) -> Result<()> {
    archive::relative(path)?;
    let first = path.split('/').next().unwrap_or("").to_ascii_lowercase();
    if ![
        "mods",
        "config",
        "defaultconfigs",
        "kubejs",
        "scripts",
        "datapacks",
        "resourcepacks",
        "resources",
        "global_packs",
        "patchouli_books",
        "ftbquests",
        "openloader",
    ]
    .contains(&first.as_str())
        && path != "server.properties"
    {
        return Err(format!(
            "Pack file is outside supported server content directories: {path}"
        ));
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ["exe", "dll", "bat", "cmd", "ps1", "vbs", "com", "lnk"].contains(&extension.as_str())
        || path.split('/').any(|v| v.eq_ignore_ascii_case(".minehost"))
    {
        return Err("Executable bootstrap files are forbidden in modpacks".into());
    }
    Ok(())
}
fn index(path: &Path) -> Result<Index> {
    let mut zip = archive::open(path)?;
    archive::entries(&mut zip)?;
    let mut value = String::new();
    zip.by_name("modrinth.index.json").map_err(|_|"Only Modrinth .mrpack archives with modrinth.index.json are supported; CurseForge manifests require official API access")?.take(4*1024*1024+1).read_to_string(&mut value).map_err(error)?;
    if value.len() > 4 * 1024 * 1024 {
        return Err("Pack index exceeds 4 MiB".into());
    }
    let index: Index = serde_json::from_str(&value).map_err(error)?;
    if index.format_version != 1 || index.game != "minecraft" || index.files.len() > 4096 {
        return Err("Unsupported pack format or too many files".into());
    }
    Ok(index)
}
fn environment(file: &PackFile) -> Result<&str> {
    let value = file
        .env
        .as_ref()
        .and_then(|v| v.get("server"))
        .map(String::as_str)
        .unwrap_or("required");
    if !["required", "optional", "unsupported"].contains(&value) {
        return Err("Unknown server environment in modpack".into());
    }
    Ok(value)
}
fn compatibility(index: &Index, server: &Server, root: &Path) -> Result<()> {
    if index.dependencies.get("minecraft") != Some(&server.version) {
        return Err("Create a separate profile with the modpack's exact Minecraft version".into());
    }
    let (key, installed) = match server.software {
        Software::Fabric => {
            let path = root.join(".minehost/software.json");
            safety::reject_links(&path)?;
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).map_err(error)?).map_err(error)?;
            (
                "fabric-loader",
                value["fabricLoader"].as_str().map(str::to_string),
            )
        }
        Software::Forge => ("forge", forge::loader_version(root)?),
        Software::Neoforge => ("neoforge", forge::loader_version(root)?),
        _ => return Err("Modpacks require an installed Fabric, Forge or NeoForge profile".into()),
    };
    if index
        .dependencies
        .iter()
        .any(|(k, _)| k != "minecraft" && k != key)
        || index.dependencies.get(key) != installed.as_ref()
    {
        return Err(format!(
            "Install the exact {key} version listed by this pack in a clean profile"
        ));
    }
    Ok(())
}
fn preview(path: &Path) -> Result<Preview> {
    let fingerprint = fingerprint(path)?;
    let index = index(path)?;
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    for file in index.files {
        let env = environment(&file)?;
        if env == "unsupported" {
            continue;
        }
        let optional = env == "optional";
        destination(&file.path)?;
        if !seen.insert(file.path.to_lowercase()) {
            return Err("Duplicate pack destinations".into());
        }
        total += file.file_size;
        if total > 4 * 1024 * 1024 * 1024 || file.file_size > 256 * 1024 * 1024 {
            return Err("Modpack exceeds download limits".into());
        }
        files.push(Item {
            path: file.path,
            size: file.file_size,
            optional,
        });
    }
    Ok(Preview {
        name: index.name,
        version: index.version_id,
        dependencies: index.dependencies,
        files,
        fingerprint,
    })
}
#[tauri::command]
pub async fn inspect_pack(source: String) -> Result<Preview> {
    tauri::async_runtime::spawn_blocking(move || preview(Path::new(&source)))
        .await
        .map_err(error)?
}
#[tauri::command]
pub async fn install_pack(
    state: State<'_, Shared>,
    id: String,
    source: String,
    fingerprint: String,
    optional: Vec<String>,
    consent: bool,
    on_progress: tauri::ipc::Channel<runtime::DownloadProgress>,
) -> Result<()> {
    if !consent {
        return Err("Confirm execution risk for modpack code".into());
    }
    let _operation = crate::begin(&state, &id)?;
    let (server, root) = crate::profile_path(&state, &id)?;
    if !server.installed {
        return Err("Install server software first".into());
    }
    let source = std::path::PathBuf::from(source);
    let input = source.clone();
    let expected = fingerprint.clone();
    let index = tauri::async_runtime::spawn_blocking(move || {
        if self::fingerprint(&input)? != expected {
            return Err("Modpack changed; inspect it again".into());
        }
        index(&input)
    })
    .await
    .map_err(error)??;
    compatibility(&index, &server, &root)?;
    if !addons::installed(&root, server.software)?.is_empty() {
        return Err("Import modpacks into a clean profile without existing mods".into());
    }
    let optional: HashSet<String> = optional.into_iter().collect();
    let allowed: HashSet<String> = index
        .files
        .iter()
        .filter(|v| environment(v).ok() == Some("optional"))
        .map(|v| v.path.clone())
        .collect();
    if !optional.is_subset(&allowed) {
        return Err("Invalid optional file selection".into());
    }
    let stage = archive::Stage::new(&root.join(".minehost/packs"))?;
    let mut names = HashSet::new();
    let mut total = 0_u64;
    let mut selected = Vec::new();
    for file in index.files {
        let env = environment(&file)?;
        if env == "unsupported" || (env == "optional" && !optional.contains(&file.path)) {
            continue;
        }
        destination(&file.path)?;
        if !names.insert(file.path.to_lowercase()) {
            return Err("Duplicate pack path".into());
        }
        total += file.file_size;
        if total > 4 * 1024 * 1024 * 1024 || file.file_size > 256 * 1024 * 1024 {
            return Err("Pack exceeds download limits".into());
        }
        selected.push(file);
    }
    let report = |event| {
        let _ = on_progress.send(event);
    };
    let mut done = 0;
    for file in selected {
        let hash = file
            .hashes
            .get("sha512")
            .filter(|v| v.len() == 128 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or("Missing pack SHA-512")?;
        let url=file.downloads.iter().find(|v|reqwest::Url::parse(v).is_ok_and(|u|u.scheme()=="https"&&matches!(u.host_str(),Some("cdn.modrinth.com"|"github.com"|"raw.githubusercontent.com"|"gitlab.com")))).ok_or("Pack file has no download from an approved platform; direct downloads are not bypassed")?;
        let path = stage.0.join(&file.path);
        std::fs::create_dir_all(path.parent().ok_or("Missing parent")?).map_err(error)?;
        runtime::download(url, hash, &path, &|e| {
            report(runtime::DownloadProgress::new(
                e.phase,
                done + e.downloaded,
                Some(total),
            ))
        })
        .await?;
        if std::fs::metadata(&path).map_err(error)?.len() != file.file_size {
            return Err("Pack file length mismatch".into());
        }
        done += file.file_size;
    }
    let work = stage.0.clone();
    let source_clone = source.clone();
    tauri::async_runtime::spawn_blocking(move || overrides(&source_clone, &work))
        .await
        .map_err(error)??;
    let paths = archive::tree(&stage.0)?;
    for path in &paths {
        destination(path)?;
        if path.starts_with("mods/") {
            if !path.ends_with(".jar") {
                return Err("Only JARs may be installed in mods".into());
            }
            let loader = if server.software == Software::Fabric {
                let value: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(root.join(".minehost/software.json")).map_err(error)?,
                )
                .map_err(error)?;
                value["fabricLoader"].as_str().map(str::to_string)
            } else {
                None
            };
            addons::validate_jar(
                &stage.0.join(path),
                server.software,
                &server.version,
                loader.as_deref(),
            )?;
        }
        if path == "server.properties" {
            safety::properties(
                &std::fs::read_to_string(stage.0.join(path)).map_err(error)?,
                server.port,
            )?;
        }
    }
    let root_clone = root.clone();
    let work = stage.0.clone();
    tauri::async_runtime::spawn_blocking(move || archive::overlay(&root_clone, &work, &paths))
        .await
        .map_err(error)??;
    report(runtime::DownloadProgress::new(
        runtime::DownloadPhase::Complete,
        total,
        Some(total),
    ));
    Ok(())
}
fn overrides(source: &Path, stage: &Path) -> Result<()> {
    let mut zip = archive::open(source)?;
    archive::entries(&mut zip)?;
    for layer in ["overrides/", "server-overrides/"] {
        for i in 0..zip.len() {
            let mut item = zip.by_index(i).map_err(error)?;
            if item.is_dir() {
                continue;
            }
            let Some(path) = item.name().strip_prefix(layer) else {
                continue;
            };
            destination(path)?;
            let target = stage.join(path);
            std::fs::create_dir_all(target.parent().ok_or("Missing parent")?).map_err(error)?;
            let mut file = std::fs::File::create(target).map_err(error)?;
            let expected = item.size();
            if std::io::copy(&mut (&mut item).take(expected.saturating_add(1)), &mut file)
                .map_err(error)?
                != expected
            {
                return Err("Override length mismatch".into());
            }
            file.sync_all().map_err(error)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn respects_server_override_priority_and_exact_loader_dependencies() {
        use std::io::Write;
        let root = archive::Stage::new(&std::env::temp_dir()).unwrap();
        let source = root.0.join("test.mrpack");
        let index = serde_json::json!({"formatVersion":1,"game":"minecraft","name":"Test","versionId":"1","dependencies":{"minecraft":"1.21.1","fabric-loader":"0.18.4"},"files":[{"path":"mods/client.jar","fileSize":1,"hashes":{},"downloads":[],"env":{"server":"unsupported","client":"required"}}]});
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
        for (name, bytes) in [
            ("modrinth.index.json", index.to_string()),
            ("overrides/config/test.toml", "common = true".into()),
            ("server-overrides/config/test.toml", "server = true".into()),
            ("client-overrides/config/test.toml", "client = true".into()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        assert!(preview(&source).unwrap().files.is_empty());
        std::fs::create_dir_all(root.0.join(".minehost")).unwrap();
        std::fs::write(
            root.0.join(".minehost/software.json"),
            r#"{"fabricLoader":"0.18.4"}"#,
        )
        .unwrap();
        let mut server = Server {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Test".into(),
            version: "1.21.1".into(),
            port: 25565,
            ram_mb: 2048,
            eula: false,
            installed: true,
            java_major: 21,
            software: Software::Fabric,
            loader_version: None,
        };
        let pack = self::index(&source).unwrap();
        compatibility(&pack, &server, &root.0).unwrap();
        server.version = "1.21.2".into();
        assert!(compatibility(&pack, &server, &root.0).is_err());
        let stage = archive::Stage::new(&std::env::temp_dir()).unwrap();
        overrides(&source, &stage.0).unwrap();
        assert_eq!(
            std::fs::read_to_string(stage.0.join("config/test.toml")).unwrap(),
            "server = true"
        );
        archive::overlay(&root.0, &stage.0, &archive::tree(&stage.0).unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.0.join("config/test.toml")).unwrap(),
            "server = true"
        );
    }
    #[test]
    fn prevents_bootstrap_and_profile_overrides() {
        for value in [
            "../mods/a.jar",
            "eula.txt",
            ".minehost/forge.json",
            "run.bat",
            "libraries/a.jar",
            "config/a.exe",
        ] {
            assert!(destination(value).is_err(), "{value}");
        }
        assert!(destination("kubejs/server_scripts/main.js").is_ok());
    }
    #[test]
    fn excludes_client_files() {
        let file = PackFile {
            path: "mods/a.jar".into(),
            hashes: HashMap::new(),
            downloads: vec![],
            file_size: 1,
            env: Some(HashMap::from([("server".into(), "unsupported".into())])),
        };
        assert_eq!(environment(&file).unwrap(), "unsupported");
    }
}
