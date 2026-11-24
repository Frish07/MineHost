use crate::{error, runtime, software::Software, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Deserialize, Serialize)]
pub struct SearchHit { pub project_id: String, pub title: String, pub description: String, pub downloads: u64 }
#[derive(Deserialize, Serialize)]
pub struct SearchResult { pub hits: Vec<SearchHit>, pub total_hits: u64 }
#[derive(Clone, Deserialize, Serialize)]
pub struct Version {
    pub id: String, pub project_id: String, pub name: String, pub version_number: String,
    pub version_type: String, pub game_versions: Vec<String>, pub loaders: Vec<String>,
    pub files: Vec<VersionFile>, pub dependencies: Vec<Dependency>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct VersionFile { pub url: String, pub filename: String, pub primary: bool, pub size: u64, pub hashes: HashMap<String, String> }
#[derive(Clone, Deserialize, Serialize)]
pub struct Dependency { pub project_id: Option<String>, pub version_id: Option<String>, pub dependency_type: String }
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem { pub project_id: String, pub version_id: String, pub title: String, pub version_number: String, pub file_name: String, pub size: u64, pub url: String, pub sha512: String }

pub fn identifier(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') { return Err("Invalid Modrinth identifier".into()); }
    Ok(())
}

fn query_url(path: &str, query: &[(&str, String)]) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("https://api.modrinth.com/v2/{path}")).map_err(error)?;
    for (key, value) in query { url.query_pairs_mut().append_pair(key, value); }
    Ok(url.to_string())
}

pub async fn search(software: Software, game: &str, query: &str, offset: u32, sort: &str) -> Result<SearchResult> {
    software.addon_directory()?;
    if query.len() > 100 || offset > 10000 || !["relevance", "downloads", "updated"].contains(&sort) { return Err("Invalid catalog search".into()); }
    let categories: Vec<String> = software.loaders().iter().map(|loader| format!("categories:{loader}")).collect();
    let facets = serde_json::to_string(&vec![vec!["project_type:mod".into()], vec![format!("versions:{game}")], categories, vec!["environment:client_and_server".into(), "environment:server_only".into(), "environment:server_only_client_optional".into(), "environment:dedicated_server_only".into(), "environment:client_only_server_optional".into()]]).map_err(error)?;
    let url = query_url("search", &[("query", query.into()), ("facets", facets), ("offset", offset.to_string()), ("limit", "20".into()), ("index", sort.into())])?;
    serde_json::from_value(runtime::get_json(&url).await?).map_err(error)
}

pub fn compatible(version: &Version, software: Software, game: &str) -> bool {
    version.game_versions.iter().any(|v| v == game) && version.loaders.iter().any(|v| software.loaders().contains(&v.as_str()))
}

pub async fn versions(project: &str, software: Software, game: &str) -> Result<Vec<Version>> {
    identifier(project)?; software.addon_directory()?;
    let url = query_url(&format!("project/{project}/version"), &[("game_versions", serde_json::to_string(&[game]).map_err(error)?), ("loaders", serde_json::to_string(software.loaders()).map_err(error)?)])?;
    let values: Vec<Version> = serde_json::from_value(runtime::get_json(&url).await?).map_err(error)?;
    Ok(values.into_iter().filter(|v| v.version_type == "release" && compatible(v, software, game)).take(100).collect())
}

async fn version(id: &str) -> Result<Version> {
    identifier(id)?;
    serde_json::from_value(runtime::get_json(&format!("https://api.modrinth.com/v2/version/{id}")).await?).map_err(error)
}

pub async fn plan(root: &str, software: Software, game: &str, installed: &[(String, String)]) -> Result<Vec<PlanItem>> {
    software.addon_directory()?;
    let mut queue = VecDeque::from([(version(root).await?, 0_u8)]);
    let mut selected: HashMap<String, String> = HashMap::new();
    let mut items = Vec::new(); let mut exclusions = Vec::new(); let mut total = 0_u64;
    while let Some((version, depth)) = queue.pop_front() {
        identifier(&version.project_id)?; identifier(&version.id)?;
        if depth > 16 || items.len() >= 32 { return Err("Dependency graph exceeds 32 projects or 16 levels".into()); }
        if !compatible(&version, software, game) { return Err(format!("{} is incompatible with this loader or Minecraft version", version.name)); }
        if let Some(previous) = selected.get(&version.project_id) {
            if previous != &version.id { return Err("Required dependencies demand conflicting versions of the same project".into()); }
            continue;
        }
        let project = runtime::get_json(&format!("https://api.modrinth.com/v2/project/{}", version.project_id)).await?;
        if !matches!(project["server_side"].as_str(), Some("required" | "optional")) || project["project_type"] != "mod" { return Err("Client-only projects, unknown server compatibility and modpacks are not supported on this server".into()); }
        let file = version.files.iter().find(|f| f.primary && f.filename.to_ascii_lowercase().ends_with(".jar")).or_else(|| version.files.iter().find(|f| f.filename.to_ascii_lowercase().ends_with(".jar"))).ok_or("No installable JAR file for this release")?;
        crate::addons::jar_filename(&file.filename)?;
        let sha512 = file.hashes.get("sha512").filter(|s| s.len() == 128 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("Modrinth did not provide a valid SHA-512")?;
        if file.size == 0 || file.size > 256 * 1024 * 1024 { return Err("Addon size must be between 1 byte and 256 MiB".into()); }
        total += file.size; if total > 1024 * 1024 * 1024 { return Err("Installation exceeds 1 GiB".into()); }
        let url = reqwest::Url::parse(&file.url).map_err(error)?;
        if url.scheme() != "https" || url.host_str() != Some("cdn.modrinth.com") { return Err("Unexpected Modrinth file origin".into()); }
        selected.insert(version.project_id.clone(), version.id.clone());
        items.push(PlanItem { project_id: version.project_id.clone(), version_id: version.id.clone(), title: project["title"].as_str().unwrap_or(&version.name).to_string(), version_number: version.version_number.clone(), file_name: file.filename.clone(), size: file.size, url: file.url.clone(), sha512: sha512.clone() });
        for dependency in version.dependencies {
            if dependency.dependency_type == "required" {
                let child = if let Some(id) = dependency.version_id { self::version(&id).await? }
                    else if let Some(id) = dependency.project_id { versions(&id, software, game).await?.into_iter().next().ok_or("A required dependency has no compatible stable release")? }
                    else { return Err("A required dependency has no downloadable project or version".into()); };
                queue.push_back((child, depth + 1));
            } else if dependency.dependency_type == "incompatible" { exclusions.push(dependency); }
        }
    }
    for exclusion in exclusions {
        let mut project = exclusion.project_id;
        if project.is_none() { if let Some(id) = &exclusion.version_id { project = Some(version(id).await?.project_id); } }
        if let Some(project) = project {
            let versions = selected.get(&project).into_iter().chain(installed.iter().filter(|(p, _)| p == &project && !selected.contains_key(p)).map(|(_, v)| v));
            for installed_version in versions {
                if exclusion.version_id.as_ref().is_none_or(|v| v == installed_version) { return Err(format!("Explicitly incompatible addon is installed or selected: {project}")); }
            }
        }
    }
    let mut names = HashSet::new();
    for item in &items { if !names.insert(item.file_name.to_ascii_lowercase()) { return Err("Two projects supply the same filename".into()); } }
    items.reverse(); Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_cannot_escape_api_paths() { for id in ["../a", "a?token=b", "a/b", ""] { assert!(identifier(id).is_err()); } assert!(identifier("fabric-api").is_ok()); }
    #[tokio::test]
    #[ignore = "Official Modrinth API, GitHub Actions only"]
    async fn modrinth_catalog_contract() {
        let results = search(Software::Paper, "1.21.11", "luckperms", 0, "relevance").await.unwrap();
        assert!(results.hits.iter().any(|v| v.project_id == "Vebnzrzj"));
        let versions = versions("luckperms", Software::Paper, "1.21.11").await.unwrap();
        let plan = plan(&versions[0].id, Software::Paper, "1.21.11", &[]).await.unwrap();
        assert!(!plan.is_empty()); assert!(plan[0].sha512.len() == 128);
        assert!(self::plan(&versions[0].id, Software::Fabric, "1.21.11", &[]).await.is_err());
    }
}
