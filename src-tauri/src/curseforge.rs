use crate::{
    addons::{self, AddonProvider, DownloadFuture, InstallItem, Progress, Provider},
    error,
    runtime::{DownloadPhase, DownloadProgress},
    safety,
    software::Software,
    win, Result, Shared,
};
use reqwest::{header::HeaderValue, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};
use tauri::State;
const KEY: &str = "MineHost/curseforge-api/v1";
const BASE: &str = "https://api.curseforge.com/v1";
const GAME: u32 = 432;
const LIMIT: u64 = 256 * 1024 * 1024;
pub fn identifier(id: &str) -> Result<u32> {
    let n = id.parse::<u32>().map_err(|_| "Invalid CurseForge ID")?;
    if n == 0 || n > i32::MAX as u32 || n.to_string() != id {
        return Err("Invalid CurseForge ID".into());
    }
    Ok(n)
}
fn key_header(value: &str) -> Result<HeaderValue> {
    if !(20..=512).contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("Expected one printable CurseForge API key, 20–512 bytes".into());
    }
    let mut h = HeaderValue::from_str(value).map_err(|_| "Invalid API key header")?;
    h.set_sensitive(true);
    Ok(h)
}
fn client() -> Result<Client> {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .user_agent("MineHost/0.9.0 (https://github.com/Frish07/MineHost)")
        .build()
        .map_err(|_| "Cannot create CurseForge HTTPS client".into())
}
pub struct Api {
    client: Client,
    key: HeaderValue,
    requests: AtomicU32,
    base: String,
}
impl Api {
    fn configured() -> Result<Self> {
        let key = win::credential_read(KEY)?
            .ok_or("Configure your approved CurseForge API access first")?;
        Ok(Self {
            client: client()?,
            key: key_header(&key)?,
            requests: AtomicU32::new(0),
            base: BASE.into(),
        })
    }
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        if self.requests.fetch_add(1, Ordering::Relaxed) >= 256 {
            return Err("CurseForge operation exceeds 256 API requests".into());
        }
        let mut url =
            Url::parse(&format!("{}/{path}", self.base)).map_err(|_| "Invalid API query")?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut response = self
            .client
            .get(url)
            .header("x-api-key", self.key.clone())
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| "CurseForge request failed; check connectivity")?;
        if !response.status().is_success() {
            return Err(format!("CurseForge API HTTP {}. Check key approval, scope and quota. No fallback endpoint was used.",response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Cannot read CurseForge response")?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err("CurseForge JSON exceeds 4 MiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| "Invalid CurseForge JSON".into())
    }
    async fn categories(&self) -> Result<Vec<Category>> {
        let v = self
            .get("categories", &[("gameId", GAME.to_string())])
            .await?;
        serde_json::from_value(v["data"].clone())
            .map_err(|_| "Invalid CurseForge categories".into())
    }
    async fn class(&self, software: Software) -> Result<(u32, Vec<Category>)> {
        software.addon_directory()?;
        let all = self.categories().await?;
        let plugin = matches!(software, Software::Paper | Software::Purpur);
        let class=all.iter().find(|v|v.game_id==GAME && v.is_class==Some(true) && if plugin {v.slug=="bukkit-plugins"||v.name.eq_ignore_ascii_case("Bukkit Plugins")}else{v.slug=="mc-mods"||v.name.eq_ignore_ascii_case("Mods")}).ok_or("This key/API does not expose the requested Minecraft Mods/Bukkit Plugins class; use the website and manual JAR import")?.id;
        Ok((
            class,
            all.into_iter()
                .filter(|c| {
                    c.game_id == GAME && c.is_class != Some(true) && c.class_id == Some(class)
                })
                .collect(),
        ))
    }
    async fn project(&self, id: u32) -> Result<Project> {
        let v = self.get(&format!("mods/{id}"), &[]).await?;
        let p: Project =
            serde_json::from_value(v["data"].clone()).map_err(|_| "Invalid CurseForge project")?;
        if p.id != id {
            return Err("CurseForge returned a different project".into());
        }
        Ok(p)
    }
    async fn file(&self, project: u32, file: u32) -> Result<File> {
        let v = self
            .get(&format!("mods/{project}/files/{file}"), &[])
            .await?;
        let f: File =
            serde_json::from_value(v["data"].clone()).map_err(|_| "Invalid CurseForge file")?;
        if f.id != file || f.mod_id != project {
            return Err("CurseForge file/project mismatch".into());
        }
        Ok(f)
    }
    async fn files(&self, project: u32, software: Software, game: &str) -> Result<Vec<File>> {
        let mut result = Vec::new();
        for page in 0..10 {
            let mut query = vec![
                ("gameVersion", game.into()),
                ("index", (page * 50).to_string()),
                ("pageSize", "50".into()),
            ];
            if let Some((loader, _)) = loader(software) {
                query.push(("modLoaderType", loader.to_string()));
            }
            let v = self.get(&format!("mods/{project}/files"), &query).await?;
            let files: Vec<File> = serde_json::from_value(v["data"].clone())
                .map_err(|_| "Invalid CurseForge files")?;
            let count = files.len();
            for f in files {
                if f.mod_id == project && compatible(&f, software, game) {
                    result.push(f);
                }
            }
            let total = v["pagination"]["totalCount"].as_u64();
            if total.is_some_and(|n| n > 500) {
                return Err(
                    "More than 500 matching file entries; refusing an incomplete version list"
                        .into(),
                );
            }
            if count < 50 || total.is_some_and(|n| ((page + 1) * 50) as u64 >= n) {
                result.sort_by(|a, b| b.file_date.cmp(&a.file_date).then_with(|| b.id.cmp(&a.id)));
                return Ok(result);
            }
        }
        Err("CurseForge file pagination exceeds limit".into())
    }
    async fn plan(
        &self,
        root: u32,
        file: u32,
        server: &crate::Server,
        active: &[(u32, u32)],
        class: u32,
    ) -> Result<Plan> {
        let mut queue = VecDeque::from([(root, file, 0)]);
        let mut selected = HashMap::new();
        let mut exclusions = Vec::new();
        let mut items = Vec::new();
        while let Some((project, file, depth)) = queue.pop_front() {
            if depth > 16 {
                return Err("Required dependencies exceed 16 levels".into());
            }
            if let Some(previous) = selected.get(&project) {
                if file != 0 && *previous != file {
                    return Err("Conflicting CurseForge file selections".into());
                }
                continue;
            }
            if selected.len() >= 32 {
                return Err("Required dependencies exceed 32 projects".into());
            }
            let p = self.project(project).await?;
            project_matches(&p, class)?;
            let f = if file != 0 {
                self.file(project, file).await?
            } else if let Some((_, existing)) = active.iter().find(|(p, _)| *p == project) {
                let current = self.file(project, *existing).await?;
                if compatible(&current, server.software, &server.version) {
                    current
                } else {
                    self.files(project, server.software, &server.version)
                        .await?
                        .into_iter()
                        .next()
                        .ok_or_else(|| {
                            format!("No compatible release for required project {project}")
                        })?
                }
            } else {
                self.files(project, server.software, &server.version)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        format!("No compatible release for required project {project}")
                    })?
            };
            if !compatible(&f, server.software, &server.version) {
                return Err(format!(
                    "File {} does not match Minecraft/loader, availability or release policy",
                    f.id
                ));
            }
            selected.insert(project, f.id);
            for dep in &f.dependencies {
                if dep.mod_id == 0 {
                    return Err("Invalid CurseForge dependency".into());
                }
                match dep.relation_type {
                    3 => queue.push_back((dep.mod_id, 0, depth + 1)),
                    5 => exclusions.push(dep.mod_id),
                    1 | 2 | 4 | 6 => {}
                    _ => {
                        return Err(
                            "Unknown dependency relation; cannot safely plan installation".into(),
                        )
                    }
                }
            }
            let item = plan_item(&p, &f)?;
            items.push(item);
        }
        let present: HashSet<u32> = selected
            .keys()
            .copied()
            .chain(active.iter().map(|(p, _)| *p))
            .collect();
        if exclusions.iter().any(|p| present.contains(p)) {
            return Err(
                "A selected/installed CurseForge project is explicitly incompatible".into(),
            );
        }
        if active.len() > 128 {
            return Err("Conflict checking is limited to 128 enabled CurseForge records".into());
        }
        for (project, file) in active.iter().filter(|(p, _)| !selected.contains_key(p)) {
            let installed = self.file(*project, *file).await?;
            if installed
                .dependencies
                .iter()
                .any(|d| d.relation_type == 5 && selected.contains_key(&d.mod_id))
            {
                return Err(format!(
                    "Installed CurseForge project {project} declares a conflict with this plan"
                ));
            }
        }
        let mut names = HashSet::new();
        if items
            .iter()
            .any(|v| !names.insert(v.file_name.to_ascii_lowercase()))
        {
            return Err("Two CurseForge projects supply the same filename".into());
        }
        items.reverse();
        Ok(Plan {
            root_project: root,
            root_file: file,
            items,
        })
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: u32,
    game_id: u32,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    is_class: Option<bool>,
    #[serde(default)]
    class_id: Option<u32>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: u32,
    game_id: u32,
    class_id: Option<u32>,
    pub name: String,
    pub summary: String,
    pub download_count: f64,
    #[serde(default)]
    pub allow_mod_distribution: Option<bool>,
    pub links: Links,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default, skip_serializing)]
    screenshots: Vec<Screenshot>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Screenshot {
    mod_id: u32,
    title: String,
    url: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Links {
    pub website_url: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Author {
    pub name: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub id: u32,
    game_id: u32,
    mod_id: u32,
    pub is_available: bool,
    pub display_name: String,
    pub file_name: String,
    pub release_type: u32,
    file_status: u32,
    pub hashes: Vec<Hash>,
    pub file_date: String,
    pub file_length: u64,
    #[serde(default, skip_serializing)]
    pub download_url: Option<String>,
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    is_server_pack: bool,
    #[serde(default)]
    is_early_access_content: bool,
    #[serde(default)]
    parent_project_file_id: Option<u32>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Hash {
    value: String,
    algo: u32,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dependency {
    mod_id: u32,
    relation_type: u32,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub project_id: u32,
    pub file_id: u32,
    pub title: String,
    pub version_name: String,
    pub file_name: String,
    pub size: u64,
    pub checksum: String,
    pub algorithm: &'static str,
    pub manual: bool,
    pub website: String,
    #[serde(skip)]
    url: String,
}
impl PlanItem {
    fn install_item(&self) -> InstallItem {
        InstallItem {
            provider: Provider::Curseforge,
            project_id: self.project_id.to_string(),
            version_id: self.file_id.to_string(),
            version_number: self.version_name.clone(),
            file_name: self.file_name.clone(),
            size: self.size,
            url: self.url.clone(),
            checksum: self.checksum.clone(),
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    root_project: u32,
    root_file: u32,
    pub items: Vec<PlanItem>,
}
fn loader(software: Software) -> Option<(u32, &'static str)> {
    match software {
        Software::Forge => Some((1, "Forge")),
        Software::Fabric => Some((4, "Fabric")),
        Software::Neoforge => Some((6, "NeoForge")),
        _ => None,
    }
}
fn compatible(f: &File, software: Software, game: &str) -> bool {
    f.game_id == GAME
        && f.id > 0
        && f.is_available
        && f.release_type == 1
        && matches!(f.file_status, 4 | 10)
        && !f.is_server_pack
        && !f.is_early_access_content
        && f.parent_project_file_id.unwrap_or(0) == 0
        && f.game_versions.iter().any(|v| v == game)
        && loader(software)
            .is_none_or(|(_, name)| f.game_versions.iter().any(|v| v.eq_ignore_ascii_case(name)))
        && software != Software::Vanilla
        && addons::jar_filename(&f.file_name).is_ok()
        && f.file_length > 0
        && f.file_length <= LIMIT
}
fn project_matches(p: &Project, class: u32) -> Result<()> {
    if p.id == 0 || p.game_id != GAME || p.class_id != Some(class) {
        return Err(
            "Project belongs to a different game/class, not this Minecraft addon catalog".into(),
        );
    }
    Ok(())
}
fn checksum(file: &File) -> Result<(String, &'static str)> {
    for (algo, len, name) in [(1, 40, "SHA-1"), (2, 32, "MD5")] {
        if let Some(h) = file.hashes.iter().find(|h| h.algo == algo) {
            if h.value.len() != len || !h.value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid CurseForge checksum".into());
            }
            return Ok((h.value.to_ascii_lowercase(), name));
        }
    }
    Err("CurseForge did not supply a supported checksum; automatic or tracked import is unavailable".into())
}
fn cdn(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| "Invalid CurseForge download URL")?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("edge.forgecdn.net" | "mediafilez.forgecdn.net")
        )
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || !url.path().starts_with("/files/")
    {
        return Err("Unapproved CurseForge CDN URL; no key or request was sent".into());
    }
    Ok(url)
}
fn website(value: &str) -> Result<Url> {
    let u = Url::parse(value).map_err(|_| "Invalid CurseForge project page")?;
    if u.scheme() != "https"
        || u.host_str() != Some("www.curseforge.com")
        || u.port_or_known_default() != Some(443)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.query().is_some()
        || !(u.path().starts_with("/minecraft/mc-mods/")
            || u.path().starts_with("/minecraft/bukkit-plugins/"))
    {
        return Err("Unapproved CurseForge project page".into());
    }
    Ok(u)
}
fn plan_item(project: &Project, file: &File) -> Result<PlanItem> {
    let (checksum, algorithm) = checksum(file)?;
    let website = website(&project.links.website_url)?.to_string();
    let url = if project.allow_mod_distribution == Some(false) {
        None
    } else {
        file.download_url.as_deref().filter(|s| !s.is_empty())
    };
    let url = url
        .map(cdn)
        .transpose()?
        .map(|u| u.to_string())
        .unwrap_or_default();
    Ok(PlanItem {
        project_id: project.id,
        file_id: file.id,
        title: project.name.clone(),
        version_name: file.display_name.clone(),
        file_name: file.file_name.clone(),
        size: file.file_length,
        checksum,
        algorithm,
        manual: url.is_empty(),
        website,
        url,
    })
}

struct Downloader {
    api: Api,
    manual: HashMap<String, PathBuf>,
}
impl AddonProvider for Downloader {
    fn provider(&self) -> Provider {
        Provider::Curseforge
    }
    fn download<'a>(
        &'a self,
        item: &'a InstallItem,
        path: &'a Path,
        progress: &'a Progress<'a>,
    ) -> DownloadFuture<'a> {
        Box::pin(async move {
            if let Some(source) = self.manual.get(&item.version_id) {
                safety::reject_links(source)?;
                safety::reject_links(path)?;
                let mut input = std::fs::File::open(source).map_err(error)?;
                if input.metadata().map_err(error)?.len() != item.size {
                    return Err(
                        "Selected local file length differs from CurseForge metadata".into(),
                    );
                }
                let mut output = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path)
                    .map_err(error)?;
                let mut buffer = vec![0; 1024 * 1024];
                let mut done = 0;
                loop {
                    let n = input.read(&mut buffer).map_err(error)?;
                    if n == 0 {
                        break;
                    }
                    done += n as u64;
                    if done > item.size {
                        return Err("Local file grew during import".into());
                    }
                    output.write_all(&buffer[..n]).map_err(error)?;
                    progress(DownloadProgress::new(
                        DownloadPhase::Verifying,
                        done,
                        Some(item.size),
                    ));
                }
                output.sync_all().map_err(error)?;
                drop(output);
                if done != item.size || addons::file_hash(path, &item.checksum)? != item.checksum {
                    return Err(
                        "Local JAR checksum differs from the selected CurseForge file".into(),
                    );
                }
                return Ok(());
            }
            let mut url = cdn(&item.url)?;
            let mut response = None;
            for _ in 0..5 {
                let r = self
                    .api
                    .client
                    .get(url.clone())
                    .header("x-api-key", self.api.key.clone())
                    .send()
                    .await
                    .map_err(|_| "CurseForge CDN request failed")?;
                if r.status().is_redirection() {
                    let target = r
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .ok_or("CurseForge redirect has no Location")?;
                    let next = url
                        .join(target)
                        .map_err(|_| "Invalid CurseForge redirect")?;
                    url = cdn(next.as_str())?;
                    continue;
                }
                if !r.status().is_success() {
                    return Err(format!(
                        "CurseForge CDN HTTP {}; use the project page if download is restricted",
                        r.status().as_u16()
                    ));
                }
                response = Some(r);
                break;
            }
            let mut response = response.ok_or("Too many CurseForge CDN redirects")?;
            if response.content_length().is_some_and(|v| v != item.size) {
                return Err("CurseForge CDN length differs from API metadata".into());
            }
            safety::reject_links(path)?;
            let mut output = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(path)
                .map_err(error)?;
            let mut done = 0;
            let mut last = std::time::Instant::now();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "CurseForge download interrupted")?
            {
                done += chunk.len() as u64;
                if done > item.size || done > LIMIT {
                    return Err("CurseForge file exceeds declared size".into());
                }
                output.write_all(&chunk).map_err(error)?;
                if last.elapsed() >= Duration::from_millis(100) {
                    progress(DownloadProgress::new(
                        DownloadPhase::Downloading,
                        done,
                        Some(item.size),
                    ));
                    last = std::time::Instant::now();
                }
            }
            output.sync_all().map_err(error)?;
            drop(output);
            progress(DownloadProgress::new(
                DownloadPhase::Verifying,
                done,
                Some(item.size),
            ));
            if done != item.size || addons::file_hash(path, &item.checksum)? != item.checksum {
                return Err("CurseForge download checksum/length mismatch".into());
            }
            Ok(())
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manual {
    file_id: u32,
    source: String,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    file_id: u32,
    checksum: String,
    size: u64,
    manual: bool,
}
fn approval(items: &[PlanItem]) -> Vec<Approval> {
    let mut values = items
        .iter()
        .map(|v| Approval {
            file_id: v.file_id,
            checksum: v.checksum.clone(),
            size: v.size,
            manual: v.manual,
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|v| v.file_id);
    values
}
fn active(dir: &Path, software: Software) -> Result<Vec<(u32, u32)>> {
    addons::installed(dir, software)?
        .into_iter()
        .filter(|v| v.enabled && v.provider == Some(Provider::Curseforge))
        .map(|v| {
            Ok((
                identifier(&v.project_id.ok_or("Missing CurseForge project ID")?)?,
                identifier(&v.version_id.ok_or("Missing CurseForge file ID")?)?,
            ))
        })
        .collect()
}
fn valid_id(value: u32) -> Result<u32> {
    identifier(&value.to_string())
}
#[derive(Serialize)]
pub struct KeyStatus {
    configured: bool,
}
#[tauri::command]
pub fn cf_status() -> Result<KeyStatus> {
    Ok(KeyStatus {
        configured: win::credential_read(KEY)?.is_some(),
    })
}
#[tauri::command]
pub async fn cf_configure(state: State<'_, Shared>, source: String, consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm this key is approved for MineHost and its intended API use".into());
    }
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let operation = {
            let mut inner = crate::lock(&shared)?;
            if !inner.busy.insert("curseforge-settings".into()) {
                return Err("CurseForge settings are busy".into());
            }
            crate::Operation {
                shared: shared.clone(),
                id: "curseforge-settings".into(),
            }
        };
        let path = Path::new(&source);
        safety::reject_links(path)?;
        let mut text = String::new();
        std::fs::File::open(path)
            .map_err(error)?
            .take(1025)
            .read_to_string(&mut text)
            .map_err(|_| "API key must be a UTF-8 text file")?;
        if text.len() > 1024 {
            return Err("API key file is too large".into());
        }
        let value = text.trim();
        key_header(value)?;
        win::credential_write(KEY, value)?;
        drop(operation);
        Ok(())
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub fn cf_forget(consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm forgetting the local CurseForge key".into());
    }
    win::credential_delete(KEY)
}
#[tauri::command]
pub async fn cf_test() -> Result<String> {
    let v = Api::configured()?
        .get(&format!("games/{GAME}"), &[])
        .await?;
    if v["data"]["id"] != GAME || v["data"]["slug"] != "minecraft" {
        return Err("This key did not expose the expected Minecraft game".into());
    }
    Ok("Minecraft API access confirmed".into())
}
#[tauri::command]
pub async fn cf_categories(state: State<'_, Shared>, id: String) -> Result<Vec<Category>> {
    let (server, _) = crate::profile_path(&state, &id)?;
    let (_, mut categories) = Api::configured()?.class(server.software).await?;
    categories.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(categories)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Search {
    projects: Vec<Project>,
    total: u64,
}
#[tauri::command]
pub async fn cf_search(
    state: State<'_, Shared>,
    id: String,
    query: String,
    index: u32,
    sort: String,
    category: Option<u32>,
) -> Result<Search> {
    if query.len() > 100
        || query.chars().any(char::is_control)
        || index > 9980
        || !index.is_multiple_of(20)
    {
        return Err("Invalid CurseForge search/page".into());
    }
    let sort = match sort.as_str() {
        "featured" => 1,
        "downloads" => 6,
        "updated" => 3,
        _ => return Err("Invalid CurseForge sort".into()),
    };
    let (server, _) = crate::profile_path(&state, &id)?;
    let api = Api::configured()?;
    let (class, categories) = api.class(server.software).await?;
    if category.is_some_and(|id| !categories.iter().any(|c| c.id == id)) {
        return Err("Category does not belong to this Minecraft addon class".into());
    }
    let mut params = vec![
        ("gameId", GAME.to_string()),
        ("classId", class.to_string()),
        ("gameVersion", server.version),
        ("searchFilter", query),
        ("index", index.to_string()),
        ("pageSize", "20".into()),
        ("sortField", sort.to_string()),
        ("sortOrder", "desc".into()),
    ];
    if let Some((loader, _)) = loader(server.software) {
        params.push(("modLoaderType", loader.to_string()));
    }
    if let Some(category) = category {
        params.push(("categoryId", category.to_string()));
    }
    let v = api.get("mods/search", &params).await?;
    let projects: Vec<Project> = serde_json::from_value(v["data"].clone())
        .map_err(|_| "Invalid CurseForge search response")?;
    if projects.len() > 20 {
        return Err("CurseForge returned too many search hits".into());
    }
    for p in &projects {
        project_matches(p, class)?;
    }
    Ok(Search {
        projects,
        total: v["pagination"]["totalCount"]
            .as_u64()
            .ok_or("Missing CurseForge pagination")?,
    })
}
#[tauri::command]
pub async fn cf_versions(state: State<'_, Shared>, id: String, project: u32) -> Result<Vec<File>> {
    let project = valid_id(project)?;
    let (server, _) = crate::profile_path(&state, &id)?;
    let api = Api::configured()?;
    let (class, _) = api.class(server.software).await?;
    project_matches(&api.project(project).await?, class)?;
    api.files(project, server.software, &server.version).await
}
#[tauri::command]
pub async fn cf_project(state: State<'_, Shared>, id: String, project: u32) -> Result<Project> {
    let project = valid_id(project)?;
    let (server, _) = crate::profile_path(&state, &id)?;
    let api = Api::configured()?;
    let (class, _) = api.class(server.software).await?;
    let p = api.project(project).await?;
    project_matches(&p, class)?;
    Ok(p)
}
#[derive(Serialize)]
pub struct Details {
    body: String,
    gallery: Vec<crate::catalog::Gallery>,
}
fn description(html: &str) -> String {
    let mut text = String::new();
    let mut tag = String::new();
    let mut inside = false;
    let mut ignore = false;
    for c in html.chars().take(262144) {
        if c == '<' {
            inside = true;
            tag.clear();
            continue;
        }
        if inside {
            if c == '>' {
                inside = false;
                let t = tag.trim().to_ascii_lowercase();
                let name = t.split_whitespace().next().unwrap_or("");
                if matches!(name, "script" | "style") {
                    ignore = true;
                } else if matches!(name, "/script" | "/style") {
                    ignore = false;
                }
                if !ignore
                    && matches!(
                        name,
                        "br" | "br/" | "p" | "/p" | "li" | "/li" | "h1" | "h2" | "h3"
                    )
                {
                    text.push('\n');
                }
            } else if tag.len() < 512 {
                tag.push(c);
            }
            continue;
        }
        if !ignore {
            text.push(c);
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}
#[tauri::command]
pub async fn cf_details(state: State<'_, Shared>, id: String, project: u32) -> Result<Details> {
    valid_id(project)?;
    let (server, _) = crate::profile_path(&state, &id)?;
    let api = Api::configured()?;
    let (class, _) = api.class(server.software).await?;
    let p = api.project(project).await?;
    project_matches(&p, class)?;
    let value = api.get(&format!("mods/{project}/description"), &[]).await?;
    let body = description(
        value["data"]
            .as_str()
            .ok_or("Missing CurseForge description")?,
    );
    let mut gallery = Vec::new();
    for s in p
        .screenshots
        .into_iter()
        .filter(|s| s.mod_id == project)
        .take(6)
    {
        if let Ok(image) = crate::catalog::provider_image(&s.url, "media.forgecdn.net").await {
            gallery.push(crate::catalog::Gallery {
                title: s.title.chars().take(200).collect(),
                image,
            });
        }
    }
    Ok(Details { body, gallery })
}
#[tauri::command]
pub async fn cf_plan(
    state: State<'_, Shared>,
    id: String,
    project: u32,
    file: u32,
) -> Result<Plan> {
    valid_id(project)?;
    valid_id(file)?;
    let (server, dir) = crate::profile_path(&state, &id)?;
    let api = Api::configured()?;
    let (class, _) = api.class(server.software).await?;
    let plan = api
        .plan(
            project,
            file,
            &server,
            &active(&dir, server.software)?,
            class,
        )
        .await?;
    check_disabled(&dir, server.software, &plan)?;
    Ok(plan)
}
fn check_disabled(dir: &Path, software: Software, plan: &Plan) -> Result<()> {
    for item in addons::installed(dir, software)? {
        if item.provider == Some(Provider::Curseforge) && !item.enabled {
            if let Some(project) = item.project_id {
                let project = identifier(&project)?;
                if project != plan.root_project
                    && plan.items.iter().any(|v| v.project_id == project)
                {
                    return Err(format!("Required CurseForge project {project} is disabled; enable it before planning installation"));
                }
            }
        }
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct InstallRequest {
    project: u32,
    file: u32,
    approved: Vec<Approval>,
    manual: Vec<Manual>,
    consent: bool,
}
#[tauri::command]
pub async fn cf_install(
    state: State<'_, Shared>,
    id: String,
    request: InstallRequest,
    on_progress: tauri::ipc::Channel<DownloadProgress>,
) -> Result<()> {
    let InstallRequest {
        project,
        file,
        mut approved,
        manual,
        consent,
    } = request;
    if !consent {
        return Err("Confirm trust in third-party mod/plugin code before installing".into());
    }
    valid_id(project)?;
    valid_id(file)?;
    let _operation = crate::begin(state.inner(), &id)?;
    let (server, dir) = crate::profile_path(&state, &id)?;
    if !server.installed {
        return Err("Install the server core first".into());
    }
    let report = |event| {
        let _ = on_progress.send(event);
    };
    report(DownloadProgress::new(DownloadPhase::Resolving, 0, None));
    let api = Api::configured()?;
    let (class, _) = api.class(server.software).await?;
    let plan = api
        .plan(
            project,
            file,
            &server,
            &active(&dir, server.software)?,
            class,
        )
        .await?;
    check_disabled(&dir, server.software, &plan)?;
    approved.sort_by_key(|v| v.file_id);
    if approved != approval(&plan.items) {
        return Err(
            "CurseForge plan changed. Review files, checksums and download permissions again."
                .into(),
        );
    }
    let mut sources = HashMap::new();
    for m in manual {
        if !plan
            .items
            .iter()
            .any(|v| v.file_id == m.file_id && v.manual)
            || sources
                .insert(m.file_id.to_string(), PathBuf::from(m.source))
                .is_some()
        {
            return Err("Unexpected or duplicate manual file selection".into());
        }
    }
    let provider = Downloader {
        api,
        manual: sources,
    };
    let items = plan
        .items
        .iter()
        .map(PlanItem::install_item)
        .collect::<Vec<_>>();
    let ids = items
        .iter()
        .map(|v| v.version_id.clone())
        .collect::<Vec<_>>();
    addons::install_verified(
        &dir,
        server.software,
        &server.version,
        &items,
        &ids,
        &report,
        &provider,
    )
    .await
}
#[tauri::command]
pub async fn cf_open_project(project: u32) -> Result<()> {
    let project = valid_id(project)?;
    let p = Api::configured()?.project(project).await?;
    let url = website(&p.links.website_url)?;
    std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url.as_str()])
        .spawn()
        .map_err(|_| "Cannot open CurseForge project page")?;
    Ok(())
}
#[tauri::command]
pub fn cf_help() -> Result<()> {
    std::process::Command::new("rundll32.exe")
        .args([
            "url.dll,FileProtocolHandler",
            "https://github.com/Frish07/MineHost/blob/main/docs/CURSEFORGE.md",
        ])
        .spawn()
        .map_err(|_| "Cannot open setup instructions")?;
    Ok(())
}
#[tauri::command]
pub fn cf_open_site() -> Result<()> {
    std::process::Command::new("rundll32.exe")
        .args([
            "url.dll,FileProtocolHandler",
            "https://www.curseforge.com/minecraft",
        ])
        .spawn()
        .map_err(|_| "Cannot open CurseForge website")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha1::Digest;
    fn file(id: u32, project: u32, dependencies: Value) -> Value {
        json!({"id":id,"gameId":432,"modId":project,"isAvailable":true,"displayName":format!("File {id}"),"fileName":format!("mod-{project}.jar"),"releaseType":1,"fileStatus":4,"hashes":[{"algo":1,"value":"0123456789abcdef0123456789abcdef01234567"}],"fileDate":"2026-11-25T00:00:00Z","fileLength":100,"downloadUrl":format!("https://edge.forgecdn.net/files/1/2/mod-{project}.jar"),"gameVersions":["1.21.1","Fabric"],"dependencies":dependencies})
    }
    fn project(id: u32) -> Value {
        json!({"id":id,"gameId":432,"classId":6,"name":format!("Project {id}"),"summary":"fixture","downloadCount":1,"allowModDistribution":true,"links":{"websiteUrl":format!("https://www.curseforge.com/minecraft/mc-mods/project-{id}")},"authors":[]})
    }
    fn fixture(replies: Vec<(String, Value)>) -> (Api, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for (expected, value) in replies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut b = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut b).unwrap();
                    bytes.push(b[0]);
                    assert!(bytes.len() < 16384);
                }
                let request = String::from_utf8(bytes).unwrap();
                assert!(
                    request.starts_with(&format!("GET {expected} HTTP/1.1")),
                    "Unexpected request path: {}",
                    request.lines().next().unwrap_or("")
                );
                assert!(request
                    .to_ascii_lowercase()
                    .contains("x-api-key: test-only-approved-fixture-key"));
                let body = value.to_string();
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        (
            Api {
                client: Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
                key: key_header("test-only-approved-fixture-key").unwrap(),
                requests: AtomicU32::new(0),
                base,
            },
            worker,
        )
    }
    #[tokio::test]
    async fn dependency_plan_uses_authenticated_api_and_includes_required_only() {
        let replies = vec![
            ("/mods/10".into(), json!({"data":project(10)})),
            (
                "/mods/10/files/100".into(),
                json!({"data":file(100,10,json!([{"modId":20,"relationType":3},{"modId":30,"relationType":2}]))}),
            ),
            ("/mods/20".into(), json!({"data":project(20)})),
            (
                "/mods/20/files?gameVersion=1.21.1&index=0&pageSize=50&modLoaderType=4".into(),
                json!({"data":[file(200,20,json!([]))],"pagination":{"totalCount":1}}),
            ),
        ];
        let (api, worker) = fixture(replies);
        let shared = crate::tests::memory_state();
        let mut server = {
            let inner = crate::lock(&shared).unwrap();
            let id: String = inner
                .db
                .query_row("SELECT id FROM servers", [], |r| r.get(0))
                .unwrap();
            crate::load(&inner, &id).unwrap()
        };
        server.software = Software::Fabric;
        server.version = "1.21.1".into();
        let plan = api.plan(10, 100, &server, &[], 6).await.unwrap();
        worker.join().unwrap();
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items[0].project_id, 20);
        assert!(plan.items.iter().all(|v| v.project_id != 30));
        let mut changed = approval(&plan.items);
        changed[0].manual = true;
        assert!(changed != approval(&plan.items));
    }
    #[test]
    fn permissions_loader_and_key_boundaries() {
        let p: Project = serde_json::from_value(project(10)).unwrap();
        let f: File = serde_json::from_value(file(100, 10, json!([]))).unwrap();
        assert!(compatible(&f, Software::Fabric, "1.21.1"));
        assert!(!compatible(&f, Software::Forge, "1.21.1"));
        assert!(!compatible(&f, Software::Fabric, "1.20.1"));
        let mut blocked = p.clone();
        blocked.allow_mod_distribution = Some(false);
        let item = plan_item(&blocked, &f).unwrap();
        assert!(item.manual && item.url.is_empty());
        for url in [
            "https://edge.forgecdn.net.evil.com/files/a.jar",
            "https://user@edge.forgecdn.net/files/a.jar",
            "http://edge.forgecdn.net/files/a.jar",
            "https://edge.forgecdn.net/files/a.jar?key=secret",
        ] {
            assert!(cdn(url).is_err());
        }
        assert!(key_header("bad\r\nHeader: injected").is_err());
        assert!(identifier("../10").is_err());
        assert!(identifier("010").is_err());
        assert!(website("https://www.curseforge.com.evil.com/minecraft/mc-mods/a").is_err());
        assert_eq!(
            description("<p>Hello &amp; world</p><script>bad()</script>"),
            "Hello & world"
        );
    }
    #[tokio::test]
    async fn blocked_manual_file_is_hashed_and_committed_without_cdn_request() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let source = stage.0.join("downloaded.jar");
        let mut jar = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
        jar.start_file("plugin.yml", zip::write::SimpleFileOptions::default())
            .unwrap();
        jar.write_all(b"name: Test\nversion: 1\nmain: Test\n")
            .unwrap();
        jar.finish().unwrap();
        let checksum = format!("{:x}", sha1::Sha1::digest(std::fs::read(&source).unwrap()));
        let size = std::fs::metadata(&source).unwrap().len();
        let api = Api {
            client: client().unwrap(),
            key: key_header("test-only-approved-fixture-key").unwrap(),
            requests: AtomicU32::new(0),
            base: BASE.into(),
        };
        let provider = Downloader {
            api,
            manual: HashMap::from([("100".into(), source.clone())]),
        };
        let item = InstallItem {
            provider: Provider::Curseforge,
            project_id: "10".into(),
            version_id: "100".into(),
            version_number: "1".into(),
            file_name: "test.jar".into(),
            size,
            url: String::new(),
            checksum,
        };
        let root = stage.0.join("profile");
        std::fs::create_dir(&root).unwrap();
        addons::install_verified(
            &root,
            Software::Paper,
            "1.21.1",
            std::slice::from_ref(&item),
            &["100".into()],
            &|_| {},
            &provider,
        )
        .await
        .unwrap();
        let installed = addons::installed(&root, Software::Paper).unwrap();
        assert_eq!(installed[0].provider, Some(Provider::Curseforge));
        assert_eq!(installed[0].project_id.as_deref(), Some("10"));
        addons::set_enabled(&root, Software::Paper, "test.jar", false).unwrap();
        let mut record: Project = serde_json::from_value(project(10)).unwrap();
        record.class_id = Some(5);
        let mut remote: File = serde_json::from_value(file(100, 10, json!([]))).unwrap();
        remote.file_name = "test.jar".into();
        let required = Plan {
            root_project: 20,
            root_file: 200,
            items: vec![plan_item(&record, &remote).unwrap()],
        };
        assert!(check_disabled(&root, Software::Paper, &required).is_err());
        addons::set_enabled(&root, Software::Paper, "test.jar.disabled", true).unwrap();
        let previous = std::fs::read(root.join("plugins/test.jar")).unwrap();
        let mut wrong_size = item.clone();
        wrong_size.size += 1;
        assert!(addons::install_verified(
            &root,
            Software::Paper,
            "1.21.1",
            &[wrong_size],
            &["100".into()],
            &|_| {},
            &provider
        )
        .await
        .is_err());
        let mut changed = item.clone();
        changed.version_id = "101".into();
        changed.checksum = "0000000000000000000000000000000000000000".into();
        let bad = Downloader {
            api: provider.api,
            manual: HashMap::from([("101".into(), source)]),
        };
        assert!(addons::install_verified(
            &root,
            Software::Paper,
            "1.21.1",
            &[changed],
            &["101".into()],
            &|_| {},
            &bad
        )
        .await
        .is_err());
        assert_eq!(
            std::fs::read(root.join("plugins/test.jar")).unwrap(),
            previous
        );
    }
}
