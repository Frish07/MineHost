use crate::{
    error,
    modrinth::PlanItem,
    runtime::{self, DownloadPhase, DownloadProgress},
    safety,
    software::Software,
    Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::{
    collections::HashSet,
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Provider { #[default] Modrinth, Curseforge }
#[derive(Clone)]
pub struct InstallItem {
    pub provider: Provider, pub project_id: String, pub version_id: String,
    pub version_number: String, pub file_name: String, pub size: u64,
    pub url: String, pub checksum: String,
}
impl From<&PlanItem> for InstallItem {
    fn from(v: &PlanItem) -> Self { Self{provider:Provider::Modrinth,project_id:v.project_id.clone(),version_id:v.version_id.clone(),version_number:v.version_number.clone(),file_name:v.file_name.clone(),size:v.size,url:v.url.clone(),checksum:v.sha512.clone()} }
}
pub type Progress<'a> = dyn Fn(DownloadProgress) + Send + Sync + 'a;
pub type DownloadFuture<'a> = Pin<Box<dyn Future<Output=Result<()>>+Send+'a>>;
pub trait AddonProvider: Send + Sync {
    fn provider(&self)->Provider;
    fn download<'a>(&'a self,item: &'a InstallItem,path: &'a Path,progress: &'a Progress<'a>)->DownloadFuture<'a>;
}
struct ModrinthProvider;
impl AddonProvider for ModrinthProvider {
    fn provider(&self)->Provider {Provider::Modrinth}
    fn download<'a>(&'a self,item: &'a InstallItem,path: &'a Path,progress: &'a Progress<'a>)->DownloadFuture<'a> {Box::pin(async move {runtime::download(&item.url,&item.checksum,path,progress).await})}
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Managed {
    #[serde(default)]
    pub provider: Provider,
    pub project_id: String,
    pub version_id: String,
    pub file_name: String,
    pub version_number: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub provider: Option<Provider>,
    pub file_name: String,
    pub enabled: bool,
    pub size: u64,
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    pub version_number: Option<String>,
}

pub fn jar_filename(name: &str) -> Result<()> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    if name.is_empty()
        || name.len() > 180
        || !name.to_ascii_lowercase().ends_with(".jar")
        || name.trim() != name
        || name
            .chars()
            .any(|c| c.is_control() || "\\/:<>\"|?*".contains(c))
        || matches!(
            stem.as_str(),
            "CON"
                | "CONIN$"
                | "CONOUT$"
                | "COM¹"
                | "COM²"
                | "COM³"
                | "LPT¹"
                | "LPT²"
                | "LPT³"
                | "PRN"
                | "AUX"
                | "NUL"
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
        )
    {
        return Err("Unsafe JAR filename".into());
    }
    Ok(())
}
fn canonical(name: &str) -> Result<&str> {
    let value = name.strip_suffix(".disabled").unwrap_or(name);
    jar_filename(value)?;
    Ok(value)
}
fn metadata_path(root: &Path) -> PathBuf {
    root.join(".minehost/addons.json")
}
fn private_dir(root: &Path, name: &str) -> Result<PathBuf> {
    let dir = root.join(".minehost").join(name);
    safety::reject_links(&dir)?;
    std::fs::create_dir_all(&dir).map_err(error)?;
    Ok(dir)
}
pub fn managed(root: &Path) -> Result<Vec<Managed>> {
    let path = metadata_path(root);
    safety::reject_links(&path)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    if std::fs::metadata(&path).map_err(error)?.len() > 512 * 1024 {
        return Err("Addon registry exceeds 512 KiB".into());
    }
    let values: Vec<Managed> =
        serde_json::from_slice(&std::fs::read(&path).map_err(error)?).map_err(error)?;
    let mut projects = HashSet::new();
    let mut names = HashSet::new();
    for item in &values {
        jar_filename(&item.file_name)?;
        match item.provider {
            Provider::Modrinth => { crate::modrinth::identifier(&item.project_id)?; crate::modrinth::identifier(&item.version_id)?; }
            Provider::Curseforge => {crate::curseforge::identifier(&item.project_id)?;crate::curseforge::identifier(&item.version_id)?;}
        }
        if !projects.insert((item.provider,&item.project_id)) || !names.insert(item.file_name.to_ascii_lowercase())
        {
            return Err("Duplicate addon registry entry".into());
        }
    }
    Ok(values)
}
fn write_registry(root: &Path, values: &[Managed]) -> Result<()> {
    private_dir(root, "")?;
    safety::atomic_write(
        &metadata_path(root),
        &serde_json::to_vec_pretty(values).map_err(error)?,
    )
}
fn addon_dir(root: &Path, software: Software) -> Result<PathBuf> {
    let dir = root.join(software.addon_directory()?);
    safety::reject_links(&dir)?;
    std::fs::create_dir_all(&dir).map_err(error)?;
    Ok(dir)
}

pub fn installed(root: &Path, software: Software) -> Result<Vec<Installed>> {
    if software == Software::Vanilla {
        return Ok(Vec::new());
    }
    let directory = addon_dir(root, software)?;
    let registry = managed(root)?;
    let mut result = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.to_ascii_lowercase().ends_with(".jar")
            || name.to_ascii_lowercase().ends_with(".jar.disabled"))
        {
            continue;
        }
        safety::reject_links(&entry.path())?;
        let meta = entry.metadata().map_err(error)?;
        if !meta.is_file() {
            continue;
        }
        let base = canonical(&name)?;
        let record = registry
            .iter()
            .find(|v| v.file_name.eq_ignore_ascii_case(base));
        result.push(Installed {
            provider: record.map(|v|v.provider),
            enabled: !name.ends_with(".disabled"),
            size: meta.len(),
            file_name: name,
            project_id: record.map(|v| v.project_id.clone()),
            version_id: record.map(|v| v.version_id.clone()),
            version_number: record.map(|v| v.version_number.clone()),
        });
        if result.len() > 4096 {
            return Err("Too many installed addons to display safely".into());
        }
    }
    result.sort_by_key(|v| v.file_name.to_ascii_lowercase());
    Ok(result)
}

fn minecraft_matches(requirement: &str, game: &str) -> Result<bool> {
    let mut parts: Vec<&str> = game.split('.').collect();
    while parts.len() < 3 {
        parts.push("0");
    }
    let game = semver::Version::parse(&parts.join(".")).map_err(error)?;
    for alternative in requirement.split("||") {
        let mut req = alternative
            .trim()
            .replace(">= ", ">=")
            .replace("<= ", "<=")
            .replace("> ", ">")
            .replace("< ", "<")
            .replace("~ ", "~")
            .replace("^ ", "^");
        if !req.is_empty() && req.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            let mut exact: Vec<&str> = req.split('.').collect();
            while exact.len() < 3 {
                exact.push("0");
            }
            if semver::Version::parse(&exact.join(".")).map_err(error)? == game {
                return Ok(true);
            }
            continue;
        }
        if req.starts_with(|c: char| c.is_ascii_digit()) && !req.contains(['*', 'x', 'X']) {
            req.insert(0, '=');
        }
        req = req
            .replace(',', " ")
            .split_whitespace()
            // Fabric's empty prerelease sorts below the stable release. Only stable
            // Minecraft versions are offered, so semver's -0 has the same boundary.
            .map(|part| {
                part.strip_suffix('-')
                    .map_or_else(|| part.to_string(), |base| format!("{base}-0"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        if semver::VersionReq::parse(&req)
            .map_err(|_| format!("Cannot verify Fabric Minecraft requirement: {requirement}"))?
            .matches(&game)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn validate_jar(
    path: &Path,
    software: Software,
    game: &str,
    loader: Option<&str>,
) -> Result<()> {
    let meta = std::fs::metadata(path).map_err(error)?;
    if meta.len() == 0 || meta.len() > 256 * 1024 * 1024 {
        return Err("Addon must be 1 byte to 256 MiB".into());
    }
    let mut jar = zip::ZipArchive::new(std::fs::File::open(path).map_err(error)?).map_err(error)?;
    match software {
        Software::Paper | Software::Purpur => {
            if jar.by_name("plugin.yml").is_err() && jar.by_name("paper-plugin.yml").is_err() {
                return Err("A Paper/Bukkit plugin descriptor is required; Fabric/Forge mods cannot be installed here".into());
            }
        }
        Software::Fabric => {
            let descriptor = jar.by_name("fabric.mod.json").map_err(|_| "A fabric.mod.json descriptor is required; Paper/Forge files cannot be installed here")?;
            if descriptor.size() > 65536 {
                return Err("Fabric descriptor exceeds 64 KiB".into());
            }
            let mut bytes = Vec::new();
            descriptor
                .take(65537)
                .read_to_end(&mut bytes)
                .map_err(error)?;
            if bytes.len() > 65536 {
                return Err("Fabric descriptor exceeds 64 KiB".into());
            }
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
            if value["environment"] == "client" {
                return Err("This Fabric mod is client-only".into());
            }
            if let Some(requirement) = value.pointer("/depends/fabricloader") {
                let loader =
                    loader.ok_or("Install Fabric server software before importing mods")?;
                let requirements: Vec<&str> = if let Some(s) = requirement.as_str() {
                    vec![s]
                } else {
                    requirement
                        .as_array()
                        .ok_or("Unrecognized Fabric loader dependency")?
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect()
                };
                let mut matched = false;
                for req in requirements {
                    if minecraft_matches(req, loader)? {
                        matched = true;
                    }
                }
                if !matched {
                    return Err("Mod requires a different Fabric Loader version; reinstall/update the Fabric server launcher first".into());
                }
            }
            if let Some(requirement) = value.pointer("/depends/minecraft") {
                let alternatives: Vec<&str> = if let Some(s) = requirement.as_str() {
                    vec![s]
                } else {
                    requirement
                        .as_array()
                        .ok_or("Unrecognized Minecraft dependency format")?
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect()
                };
                let mut matched = false;
                for req in alternatives {
                    if minecraft_matches(req, game)? {
                        matched = true;
                    }
                }
                if !matched {
                    return Err("Fabric mod requires a different Minecraft version".into());
                }
            }
        }
        Software::Forge | Software::Neoforge => {
            let descriptor = if software == Software::Neoforge {
                "META-INF/neoforge.mods.toml"
            } else {
                "META-INF/mods.toml"
            };
            if jar.by_name(descriptor).is_err()
                && !(software == Software::Neoforge
                    && matches!(game, "1.20.2" | "1.20.3" | "1.20.4")
                    && jar.by_name("META-INF/mods.toml").is_ok())
            {
                return Err(
                    "The JAR does not contain a matching Forge/NeoForge mod descriptor".into(),
                );
            }
        }
        Software::Vanilla => {
            software.addon_directory()?;
        }
    }
    Ok(())
}

pub fn import(root: &Path, software: Software, game: &str, source: &Path) -> Result<()> {
    let name = source
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("Invalid source filename")?;
    jar_filename(name)?;
    validate_jar(
        source,
        software,
        game,
        fabric_loader(root, software)?.as_deref(),
    )?;
    let target = addon_dir(root, software)?.join(name);
    safety::reject_links(&target)?;
    let disabled = target.with_file_name(format!("{name}.disabled"));
    safety::reject_links(&disabled)?;
    if disabled.exists() {
        return Err(
            "An addon with this name is disabled; remove it or enable it before replacing".into(),
        );
    }
    if target.exists() {
        std::fs::copy(
            &target,
            private_dir(root, "addon-backups")?.join(format!("{}-{name}", uuid::Uuid::new_v4())),
        )
        .map_err(error)?;
    }
    let mut registry = managed(root)?;
    registry.retain(|v| !v.file_name.eq_ignore_ascii_case(name));
    safety::atomic_write(&target, &std::fs::read(source).map_err(error)?)?;
    write_registry(root, &registry)
}

pub fn set_enabled(root: &Path, software: Software, name: &str, enabled: bool) -> Result<()> {
    let base = canonical(name)?;
    let dir = addon_dir(root, software)?;
    let source = dir.join(name);
    let destination = dir.join(if enabled {
        base.to_string()
    } else {
        format!("{base}.disabled")
    });
    safety::reject_links(&source)?;
    safety::reject_links(&destination)?;
    if source == destination {
        return Ok(());
    }
    if destination.exists() {
        return Err("The destination addon already exists".into());
    }
    std::fs::rename(source, destination).map_err(error)
}

pub fn remove(root: &Path, software: Software, name: &str) -> Result<()> {
    let base = canonical(name)?;
    let file = addon_dir(root, software)?.join(name);
    safety::reject_links(&file)?;
    let backup = private_dir(root, "addon-trash")?.join(format!("{}-{name}", uuid::Uuid::new_v4()));
    let mut registry = managed(root)?;
    registry.retain(|v| !v.file_name.eq_ignore_ascii_case(base));
    std::fs::rename(&file, &backup).map_err(error)?;
    if let Err(e) = write_registry(root, &registry) {
        let _ = std::fs::rename(backup, file);
        return Err(e);
    }
    Ok(())
}

pub(crate) fn file_hash(path: &Path, checksum: &str) -> Result<String> {
    if std::fs::metadata(path).map_err(error)?.len() > 256 * 1024 * 1024 {
        return Err("Existing addon exceeds the 256 MiB limit".into());
    }
    let mut input = std::fs::File::open(path).map_err(error)?;
    let mut hasher: Box<dyn sha2::digest::DynDigest> = match checksum.len() {
        128 => Box::new(Sha512::new()), 40 => Box::new(sha1::Sha1::new()),32 => Box::new(md5::Md5::new()),
        _ => return Err("Unsupported addon checksum".into()),
    };
    let mut bytes = [0_u8; 65536];
    loop {
        let size = input.read(&mut bytes).map_err(error)?;
        if size == 0 {
            break;
        }
        hasher.update(&bytes[..size]);
    }
    Ok(hasher.finalize().iter().map(|b|format!("{b:02x}")).collect())
}

fn fabric_loader(root: &Path, software: Software) -> Result<Option<String>> {
    if software != Software::Fabric {
        return Ok(None);
    }
    let path = root.join(".minehost/software.json");
    safety::reject_links(&path)?;
    if !path.exists() {
        return Ok(None);
    }
    if std::fs::metadata(&path).map_err(error)?.len() > 65536 {
        return Err("Software metadata exceeds 64 KiB".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).map_err(error)?).map_err(error)?;
    Ok(value["fabricLoader"].as_str().map(str::to_string))
}

pub async fn install(
    root: &Path,
    software: Software,
    game: &str,
    items: &[PlanItem],
    approved: &[String],
    progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> Result<()> {
    let items=items.iter().map(InstallItem::from).collect::<Vec<_>>();
    install_verified(root,software,game,&items,approved,progress,&ModrinthProvider).await
}
pub async fn install_verified(
    root: &Path, software: Software, game: &str, items: &[InstallItem], approved: &[String],
    progress: &Progress<'_>, provider: &dyn AddonProvider,
) -> Result<()> {
    if items.is_empty() || items.len()>32 || items.iter().any(|v|v.provider!=provider.provider() || v.size==0 || v.size>256*1024*1024) {
        return Err("Invalid addon provider plan or file size".into());
    }
    let expected: HashSet<&str> = items.iter().map(|v| v.version_id.as_str()).collect();
    let approved: HashSet<&str> = approved.iter().map(String::as_str).collect();
    if expected != approved {
        return Err("The installation plan changed. Review and approve it again.".into());
    }
    let dir = addon_dir(root, software)?;
    let mut registry = managed(root)?;
    let loader = fabric_loader(root, software)?;
    let staging = private_dir(root, &format!("staging/{}", uuid::Uuid::new_v4()))?;
    let backups = private_dir(root, &format!("addon-backups/{}", uuid::Uuid::new_v4()))?;
    let mut changes = Vec::new();
    let total: u64 = items.iter().map(|v| v.size).sum();
    let mut done = 0_u64;
    for item in items {
        jar_filename(&item.file_name)?;
        let old = registry.iter().find(|v| v.provider==item.provider && v.project_id == item.project_id);
        if registry.iter().any(|v| {
            v.file_name.eq_ignore_ascii_case(&item.file_name) && (v.provider!=item.provider || v.project_id != item.project_id)
        }) {
            return Err("Addon filename belongs to a different installed project".into());
        }
        let old_base = old.map(|v| v.file_name.as_str()).unwrap_or(&item.file_name);
        let old_disabled = dir.join(format!("{old_base}.disabled"));
        safety::reject_links(&old_disabled)?;
        let disabled = old_disabled.exists();
        let old_file = dir.join(if disabled {
            format!("{old_base}.disabled")
        } else {
            old_base.to_string()
        });
        safety::reject_links(&old_file)?;
        let target = dir.join(if disabled {
            format!("{}.disabled", item.file_name)
        } else {
            item.file_name.clone()
        });
        safety::reject_links(&target)?;
        if old.is_none() && target.exists() {
            return Err("A manually imported JAR already uses this filename. Remove or rename it before installing this project.".into());
        }
        if target != old_file && target.exists() {
            return Err("Another file occupies the new addon filename".into());
        }
        if old.is_some()
            && old_file.is_file()
            && old_base.eq_ignore_ascii_case(&item.file_name)
            && file_hash(&old_file,&item.checksum)? == item.checksum
        {
            registry.retain(|v| v.provider!=item.provider || v.project_id != item.project_id);
            registry.push(Managed {
                provider: item.provider,
                project_id: item.project_id.clone(),
                version_id: item.version_id.clone(),
                file_name: item.file_name.clone(),
                version_number: item.version_number.clone(),
            });
            done += item.size;
            progress(DownloadProgress::new(
                DownloadPhase::Verifying,
                done,
                Some(total),
            ));
            continue;
        }
        let file = staging.join(&item.file_name);
        let report = |event: DownloadProgress| {
            progress(DownloadProgress::new(
                event.phase,
                done + event.downloaded,
                Some(total),
            ))
        };
        provider.download(item,&file,&report).await?;
        if std::fs::metadata(&file).map_err(error)?.len() != item.size {
            return Err("Addon length differs from provider metadata".into());
        }
        validate_jar(&file, software, game, loader.as_deref())?;
        done += item.size;
        changes.push((item.clone(), old_file, target, file));
    }
    commit_files(root, &backups, &changes, registry)?;
    let _ = std::fs::remove_dir(&staging);
    progress(DownloadProgress::new(
        DownloadPhase::Complete,
        total,
        Some(total),
    ));
    Ok(())
}

fn commit_files(
    root: &Path,
    backups: &Path,
    changes: &[(InstallItem, PathBuf, PathBuf, PathBuf)],
    mut registry: Vec<Managed>,
) -> Result<()> {
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut created: Vec<PathBuf> = Vec::new();
    let result = (|| {
        for (item, old, target, staged) in changes {
            if old.exists() {
                let backup = backups.join(format!(
                    "{}-{}",
                    uuid::Uuid::new_v4(),
                    old.file_name()
                        .ok_or("Missing addon filename")?
                        .to_string_lossy()
                ));
                std::fs::rename(old, &backup).map_err(error)?;
                moved.push((old.clone(), backup));
            }
            std::fs::rename(staged, target).map_err(error)?;
            created.push(target.clone());
            registry.retain(|v| v.provider!=item.provider || v.project_id != item.project_id);
            registry.push(Managed {
                provider: item.provider,
                project_id: item.project_id.clone(),
                version_id: item.version_id.clone(),
                file_name: item.file_name.clone(),
                version_number: item.version_number.clone(),
            });
        }
        write_registry(root, &registry)
    })();
    if let Err(e) = result {
        let mut rollback_failed = false;
        for path in created.iter().rev() {
            if std::fs::remove_file(path).is_err() {
                rollback_failed = true;
            }
        }
        for (original, backup) in moved.iter().rev() {
            if std::fs::rename(backup, original).is_err() {
                rollback_failed = true;
            }
        }
        return Err(if rollback_failed {
            format!("{e}. Rollback incomplete; previous files remain under .minehost/addon-backups")
        } else {
            e
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let root = std::env::temp_dir().join(format!("minehost-addon-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        root
    }
    fn jar(path: &Path, descriptor: &str, data: &str) {
        use std::io::Write;
        let mut archive = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        archive
            .start_file(descriptor, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(data.as_bytes()).unwrap();
        archive.finish().unwrap();
    }
    #[test]
    fn distinguishes_modern_forge_and_neoforge_descriptors() {
        let root = temp();
        let file = root.join("mod.jar");
        jar(&file, "META-INF/mods.toml", "modLoader=\"javafml\"");
        assert!(validate_jar(&file, Software::Forge, "1.21.1", None).is_ok());
        assert!(validate_jar(&file, Software::Neoforge, "1.21.1", None).is_err());
        assert!(validate_jar(&file, Software::Neoforge, "1.20.2", None).is_ok());
        jar(
            &file,
            "META-INF/neoforge.mods.toml",
            "modLoader=\"javafml\"",
        );
        assert!(validate_jar(&file, Software::Neoforge, "1.21.1", None).is_ok());
        assert!(validate_jar(&file, Software::Paper, "1.21.1", None).is_err());
    }
    #[test]
    fn rejects_unsafe_names_and_wrong_loaders() {
        for name in [
            "../x.jar",
            "x/y.jar",
            "CON.jar",
            "CONIN$.jar",
            "COM¹.jar",
            "LPT².jar",
            " x.jar",
            "x.jar:stream",
            "a\\b.jar",
        ] {
            assert!(jar_filename(name).is_err());
        }
        let root = temp();
        let file = root.join("mod.jar");
        jar(
            &file,
            "fabric.mod.json",
            r#"{"environment":"*","depends":{"minecraft":"~1.21.11"}}"#,
        );
        assert!(validate_jar(&file, Software::Paper, "1.21.11", None).is_err());
        assert!(validate_jar(&file, Software::Fabric, "1.21.11", None).is_ok());
        assert!(validate_jar(&file, Software::Fabric, "1.20.1", None).is_err());
        jar(&file, "fabric.mod.json", r#"{"environment":"client"}"#);
        assert!(validate_jar(&file, Software::Fabric, "1.21.11", None).is_err());
        jar(
            &file,
            "fabric.mod.json",
            r#"{"environment":"*","depends":{"fabricloader":">=0.18.0"}}"#,
        );
        assert!(validate_jar(&file, Software::Fabric, "1.21.11", None).is_err());
        assert!(validate_jar(&file, Software::Fabric, "1.21.11", Some("0.20.1")).is_ok());
        assert!(validate_jar(&file, Software::Fabric, "1.21.11", Some("0.17.0")).is_err());
    }
    #[test]
    fn imported_plugins_can_be_disabled_enabled_and_removed_without_deleting_backups() {
        let root = temp();
        let source = root.join("Example.JAR");
        jar(
            &source,
            "plugin.yml",
            "name: Example\nversion: 1\nmain: example.Main\n",
        );
        import(&root, Software::Paper, "1.21.11", &source).unwrap();
        assert_eq!(installed(&root, Software::Paper).unwrap().len(), 1);
        set_enabled(&root, Software::Paper, "Example.JAR", false).unwrap();
        assert!(!installed(&root, Software::Paper).unwrap()[0].enabled);
        set_enabled(&root, Software::Paper, "Example.JAR.disabled", true).unwrap();
        remove(&root, Software::Paper, "Example.JAR").unwrap();
        assert!(installed(&root, Software::Paper).unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(root.join(".minehost/addon-trash"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn minecraft_ranges_do_not_treat_exact_versions_as_caret_ranges() {
        assert!(!minecraft_matches("1.21.1", "1.21.11").unwrap());
        assert!(minecraft_matches(">=1.21 <1.22", "1.21.11").unwrap());
        assert!(minecraft_matches("~1.21.11", "1.21.11").unwrap());
        assert!(!minecraft_matches("1.21", "1.21.11").unwrap());
        assert!(minecraft_matches(">=1.21.11- <1.21.12-", "1.21.11").unwrap());
        assert!(!minecraft_matches(">=1.21.11- <1.21.12-", "1.21.12").unwrap());
        assert!(minecraft_matches(">1.21.11-", "1.21.11").unwrap());
        assert!(!minecraft_matches("<=1.21.11-", "1.21.11").unwrap());
    }
    #[test]
    fn file_transaction_restores_previous_jars_if_a_later_move_fails() {
        let root = temp();
        let dir = addon_dir(&root, Software::Paper).unwrap();
        let backups = private_dir(&root, "addon-backups/test").unwrap();
        let old_a = dir.join("old-a.jar");
        let old_b = dir.join("old-b.jar");
        std::fs::write(&old_a, b"old-a").unwrap();
        std::fs::write(&old_b, b"old-b").unwrap();
        let staged = root.join("new-a.jar");
        std::fs::write(&staged, b"new-a").unwrap();
        let item = PlanItem {
            project_id: "ProjectA".into(),
            version_id: "VersionA".into(),
            title: "A".into(),
            version_number: "1".into(),
            file_name: "new-a.jar".into(),
            size: 5,
            url: String::new(),
            sha512: String::new(),
        };
        let item = InstallItem::from(&item);
        let new_a = dir.join("new-a.jar");
        let new_b = dir.join("new-b.jar");
        let changes = vec![
            (item.clone(), old_a.clone(), new_a.clone(), staged),
            (item, old_b.clone(), new_b.clone(), root.join("missing.jar")),
        ];
        assert!(commit_files(&root, &backups, &changes, Vec::new()).is_err());
        assert_eq!(std::fs::read(old_a).unwrap(), b"old-a");
        assert_eq!(std::fs::read(old_b).unwrap(), b"old-b");
        assert!(!new_a.exists());
        assert!(!new_b.exists());
    }
    #[tokio::test]
    #[ignore = "Real Modrinth download and install on GitHub Actions only"]
    async fn addon_install_contract() {
        let root = temp();
        let software = Software::Paper;
        let versions = crate::modrinth::versions("luckperms", software, "1.21.11")
            .await
            .unwrap();
        let items = crate::modrinth::plan(&versions[0].id, software, "1.21.11", &[])
            .await
            .unwrap();
        let approved: Vec<String> = items.iter().map(|v| v.version_id.clone()).collect();
        assert!(install(&root, software, "1.21.11", &items, &[], &|_| {})
            .await
            .is_err());
        install(&root, software, "1.21.11", &items, &approved, &|_| {})
            .await
            .unwrap();
        let list = installed(&root, software).unwrap();
        assert!(list[0].enabled);
        assert_eq!(list[0].project_id.as_deref(), Some("Vebnzrzj"));
        install(&root, software, "1.21.11", &items, &approved, &|_| {})
            .await
            .unwrap();
        assert_eq!(installed(&root, software).unwrap().len(), 1);
        let fabric = temp();
        Software::Fabric
            .install("1.21.11", &fabric, &|_| {})
            .await
            .unwrap();
        let versions = crate::modrinth::versions("fabric-api", Software::Fabric, "1.21.11")
            .await
            .unwrap();
        let items = crate::modrinth::plan(&versions[0].id, Software::Fabric, "1.21.11", &[])
            .await
            .unwrap();
        let approved = items
            .iter()
            .map(|v| v.version_id.clone())
            .collect::<Vec<_>>();
        install(
            &fabric,
            Software::Fabric,
            "1.21.11",
            &items,
            &approved,
            &|_| {},
        )
        .await
        .unwrap();
        assert!(!installed(&fabric, Software::Fabric).unwrap().is_empty());
        assert!(!fabric.join("eula.txt").exists());
    }
}
