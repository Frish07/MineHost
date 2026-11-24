use crate::{runtime, safety, Result};
use runtime::{DownloadPhase, DownloadProgress};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Software {
    #[default]
    Paper,
    Vanilla,
    Purpur,
    Fabric,
}

impl Software {
    pub fn launcher(self) -> &'static str {
        if self == Self::Fabric {
            "fabric-launcher.jar"
        } else {
            "server.jar"
        }
    }
    pub fn addon_directory(self) -> Result<&'static str> {
        match self { Self::Paper | Self::Purpur => Ok("plugins"), Self::Fabric => Ok("mods"), Self::Vanilla => Err("Vanilla does not support mods or plugins. Create a Paper, Purpur or Fabric profile.".into()) }
    }
    pub fn loaders(self) -> &'static [&'static str] {
        match self {
            Self::Paper | Self::Purpur => &["paper", "spigot", "bukkit"],
            Self::Fabric => &["fabric"],
            Self::Vanilla => &[],
        }
    }
    pub async fn versions(self) -> Result<Vec<String>> {
        let versions = match self {
            Self::Paper => runtime::paper_versions().await?,
            Self::Vanilla => manifest().await?["versions"]
                .as_array()
                .ok_or("Missing Minecraft versions")?
                .iter()
                .filter(|v| v["type"] == "release")
                .filter_map(|v| v["id"].as_str())
                .map(str::to_string)
                .collect(),
            Self::Purpur => runtime::get_json("https://api.purpurmc.org/v2/purpur").await?
                ["versions"]
                .as_array()
                .ok_or("Missing Purpur versions")?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Self::Fabric => runtime::get_json("https://meta.fabricmc.net/v2/versions/game")
                .await?
                .as_array()
                .ok_or("Missing Fabric game versions")?
                .iter()
                .filter(|v| v["stable"] == true)
                .filter_map(|v| v["version"].as_str())
                .map(str::to_string)
                .collect(),
        };
        let mut versions: Vec<String> = versions
            .into_iter()
            .filter(|v| v.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
            .collect();
        versions.sort_by_key(|v| {
            std::cmp::Reverse(
                v.split('.')
                    .map(|s| s.parse::<u32>().unwrap_or(0))
                    .collect::<Vec<_>>(),
            )
        });
        versions.dedup();
        Ok(versions)
    }
    pub async fn install(
        self,
        version: &str,
        directory: &Path,
        progress: &(dyn Fn(DownloadProgress) + Send + Sync),
    ) -> Result<u32> {
        if self == Self::Paper {
            return runtime::install_paper(version, directory, progress).await;
        }
        safety::version(version)?;
        progress(DownloadProgress::new(DownloadPhase::Resolving, 0, None));
        let minecraft = minecraft_version(version).await?;
        let java = minecraft
            .pointer("/javaVersion/majorVersion")
            .and_then(Value::as_u64)
            .unwrap_or(8) as u32;
        if !(8..=50).contains(&java) {
            return Err("Unsupported Minecraft Java version".into());
        }
        let (url, checksum, fabric_loader) = match self {
            Self::Vanilla => {
                let artifact = &minecraft["downloads"]["server"];
                (
                    text(artifact, "url")?.to_string(),
                    text(artifact, "sha1")?.to_string(),
                    None,
                )
            }
            Self::Purpur => {
                let build = runtime::get_json(&format!(
                    "https://api.purpurmc.org/v2/purpur/{version}/latest"
                ))
                .await?;
                if build["result"] != "SUCCESS" {
                    return Err("Latest Purpur build is not successful".into());
                }
                let id = text(&build, "build")?;
                safety::version(id)?;
                (
                    format!("https://api.purpurmc.org/v2/purpur/{version}/{id}/download"),
                    text(&build, "md5")?.to_string(),
                    None,
                )
            }
            Self::Fabric => {
                let loaders = runtime::get_json(&format!(
                    "https://meta.fabricmc.net/v2/versions/loader/{version}"
                ))
                .await?;
                let loader = loaders
                    .as_array()
                    .ok_or("Missing Fabric loaders")?
                    .iter()
                    .find(|v| v["loader"]["stable"] == true)
                    .ok_or("No stable Fabric loader for this Minecraft version")?;
                let loader = text(&loader["loader"], "version")?;
                safety::version(loader)?;
                let installers =
                    runtime::get_json("https://meta.fabricmc.net/v2/versions/installer").await?;
                let installer = installers
                    .as_array()
                    .ok_or("Missing Fabric installers")?
                    .iter()
                    .find(|v| v["stable"] == true)
                    .ok_or("No stable Fabric installer")?;
                let installer = text(installer, "version")?;
                safety::version(installer)?;
                (format!("https://meta.fabricmc.net/v2/versions/loader/{version}/{loader}/{installer}/server/jar"), String::new(), Some(loader.to_string()))
            }
            Self::Paper => unreachable!(),
        };
        runtime::download(&url, &checksum, &directory.join(self.launcher()), progress).await?;
        if let Some(loader) = fabric_loader {
            let private = directory.join(".minehost");
            safety::reject_links(&private)?;
            std::fs::create_dir_all(&private).map_err(crate::error)?;
            safety::atomic_write(
                &private.join("software.json"),
                &serde_json::to_vec(&serde_json::json!({"fabricLoader": loader}))
                    .map_err(crate::error)?,
            )?;
        }
        Ok(java)
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Missing API field: {key}"))
}
async fn manifest() -> Result<Value> {
    runtime::get_json("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json").await
}
async fn minecraft_version(version: &str) -> Result<Value> {
    let manifest = manifest().await?;
    let entry = manifest["versions"]
        .as_array()
        .ok_or("Missing Minecraft versions")?
        .iter()
        .find(|v| v["id"] == version && v["type"] == "release")
        .ok_or("Not an official released Minecraft version")?;
    runtime::get_verified_json(text(entry, "url")?, Some(text(entry, "sha1")?)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_profiles_default_to_paper_and_loaders_stay_separate() {
        assert_eq!(Software::default(), Software::Paper);
        assert_eq!(Software::Fabric.launcher(), "fabric-launcher.jar");
        assert!(!Software::Paper.loaders().contains(&"fabric"));
        assert!(Software::Vanilla.addon_directory().is_err());
    }
    #[tokio::test]
    #[ignore = "Official network downloads; GitHub Actions only"]
    async fn software_download_contract() {
        let root = std::env::temp_dir().join(format!("minehost-software-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        for software in [Software::Vanilla, Software::Purpur, Software::Fabric] {
            assert!(software
                .versions()
                .await
                .unwrap()
                .contains(&"1.21.11".to_string()));
            let dir = root.join(format!("{software:?}"));
            std::fs::create_dir(&dir).unwrap();
            assert_eq!(
                software.install("1.21.11", &dir, &|_| {}).await.unwrap(),
                21
            );
            assert!(
                std::fs::metadata(dir.join(software.launcher()))
                    .unwrap()
                    .len()
                    > 100_000
            );
            assert!(!dir.join("eula.txt").exists());
        }
    }
}
