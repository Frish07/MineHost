use crate::{archive, error, runtime, safety, software::Software, Result};
use runtime::{DownloadPhase, DownloadProgress};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn base(software: Software) -> Result<&'static str> {
    match software {
        Software::Forge => Ok("https://maven.minecraftforge.net/net/minecraftforge/forge"),
        Software::Neoforge => Ok("https://maven.neoforged.net/releases/net/neoforged/neoforge"),
        _ => Err("Not a Forge loader".into()),
    }
}
pub async fn text(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url).map_err(error)?;
    if parsed.scheme() != "https"
        || !matches!(
            parsed.host_str(),
            Some("maven.minecraftforge.net" | "maven.neoforged.net")
        )
    {
        return Err("Unexpected metadata origin".into());
    }
    let mut response = runtime::client()?
        .get(url)
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err("Metadata exceeds 2 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(error)
}
fn numeric(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}
fn numbers(value: &str) -> Vec<u32> {
    value.split('.').map(|v| v.parse().unwrap_or(0)).collect()
}
fn game(software: Software, build: &str) -> Option<String> {
    if software == Software::Forge {
        return build.split_once('-').map(|(v, _)| v.to_string());
    }
    let n = numbers(build);
    if n.len() < 3 {
        return None;
    }
    if n[0] < 26 {
        Some(
            format!("1.{}.{}", n[0], n[1])
                .trim_end_matches(".0")
                .to_string(),
        )
    } else if n.len() >= 4 {
        Some(if n[2] == 0 {
            format!("{}.{}", n[0], n[1])
        } else {
            format!("{}.{}.{}", n[0], n[1], n[2])
        })
    } else {
        None
    }
}
async fn builds(software: Software) -> Result<Vec<String>> {
    let xml = text(&format!("{}/maven-metadata.xml", base(software)?)).await?;
    Ok(xml
        .split("<version>")
        .skip(1)
        .filter_map(|v| v.split_once("</version>").map(|(v, _)| v.to_string()))
        .filter(|v| {
            v.bytes()
                .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-')
                && !v.ends_with("-beta")
        })
        .collect())
}
pub async fn versions(software: Software) -> Result<Vec<String>> {
    let mut values = builds(software)
        .await?
        .iter()
        .filter_map(|v| game(software, v))
        .filter(|v| numeric(v) && numbers(v) >= vec![1, 17])
        .collect::<Vec<_>>();
    values.sort_by_key(|v| std::cmp::Reverse(numbers(v)));
    values.dedup();
    Ok(values)
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Launch {
    software: Software,
    minecraft: String,
    loader_version: String,
    args_file: String,
}
pub fn loader_version(root: &Path) -> Result<Option<String>> {
    let path = root.join(".minehost/forge.json");
    safety::reject_links(&path)?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_launch(root)?.loader_version))
}
fn read_launch(root: &Path) -> Result<Launch> {
    let path = root.join(".minehost/forge.json");
    safety::reject_links(&path)?;
    if std::fs::metadata(&path).map_err(error)?.len() > 65536 {
        return Err("Invalid launcher metadata".into());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(error)?).map_err(error)
}
pub fn launch_arguments(root: &Path, software: Software, minecraft: &str) -> Result<Vec<String>> {
    let spec = read_launch(root)?;
    if spec.software != software || spec.minecraft != minecraft {
        return Err("Installed loader does not match this profile".into());
    }
    let prefix = if software == Software::Forge {
        "libraries/net/minecraftforge/forge/"
    } else {
        "libraries/net/neoforged/neoforge/"
    };
    archive::relative(&spec.args_file)?;
    if !spec.args_file.starts_with(prefix) || !spec.args_file.ends_with("/win_args.txt") {
        return Err("Invalid loader argument file".into());
    }
    let path = root.join(&spec.args_file);
    safety::reject_links(&path)?;
    if !path.is_file() {
        return Err("Loader argument file is missing; reinstall server software".into());
    }
    Ok(vec![format!("@{}", spec.args_file), "nogui".into()])
}
fn run_installer(java: &Path, installer: &Path, stage: &Path) -> Result<()> {
    let out = stage.join("installer.stdout.log");
    let err = stage.join("installer.stderr.log");
    let mut command = Command::new(java);
    runtime::hide_window(&mut command);
    command
        .args(["-jar"])
        .arg(installer)
        .arg("--installServer")
        .current_dir(stage)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&out).map_err(error)?))
        .stderr(Stdio::from(std::fs::File::create(&err).map_err(error)?));
    let mut child = command.spawn().map_err(error)?;
    let deadline = Instant::now() + Duration::from_secs(900);
    loop {
        if let Some(status) = child.try_wait().map_err(error)? {
            if status.success() {
                return Ok(());
            }
            let mut log = String::new();
            std::fs::File::open(err)
                .map_err(error)?
                .take(16384)
                .read_to_string(&mut log)
                .map_err(error)?;
            return Err(format!("Official installer failed ({status}): {log}"));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Installer exceeded 15 minutes".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
pub async fn install(
    software: Software,
    minecraft: &str,
    pinned: Option<&str>,
    root: &Path,
    java: &Path,
    report: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> Result<u64> {
    let mut candidates = builds(software)
        .await?
        .into_iter()
        .filter(|v| game(software, v).as_deref() == Some(minecraft))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|v| {
        std::cmp::Reverse(numbers(v.split_once('-').map(|(_, v)| v).unwrap_or(v)))
    });
    let build = if let Some(pinned) = pinned.filter(|v| !v.is_empty()) {
        let expected = if software == Software::Forge {
            format!("{minecraft}-{pinned}")
        } else {
            pinned.into()
        };
        candidates
            .into_iter()
            .find(|v| v == &expected)
            .ok_or("Pinned loader build is not available for this Minecraft version")?
    } else {
        candidates
            .into_iter()
            .next()
            .ok_or("No stable loader build for this Minecraft version")?
    };
    safety::version(&build)?;
    let artifact = if software == Software::Forge {
        "forge"
    } else {
        "neoforge"
    };
    let url = format!(
        "{}/{build}/{artifact}-{build}-installer.jar",
        base(software)?
    );
    let checksum = text(&format!("{url}.sha1"))
        .await?
        .split_whitespace()
        .next()
        .ok_or("Missing installer checksum")?
        .to_string();
    let stage = archive::Stage::new(&root.join(".minehost/installers"))?;
    let installer = stage.0.join("installer.jar");
    runtime::download(&url, &checksum, &installer, report).await?;
    let installer_size = std::fs::metadata(&installer).map_err(error)?.len();
    let mut zip = archive::open(&installer)?;
    let mut manifest = String::new();
    zip.by_name("install_profile.json")
        .map_err(error)?
        .take(1024 * 1024)
        .read_to_string(&mut manifest)
        .map_err(error)?;
    let value: serde_json::Value = serde_json::from_str(&manifest).map_err(error)?;
    if value["minecraft"] != minecraft {
        return Err("Official installer targets a different Minecraft version".into());
    }
    drop(zip);
    report(DownloadProgress::new(DownloadPhase::Installing, 0, None));
    let work = stage.0.join("server");
    std::fs::create_dir(&work).map_err(error)?;
    let java = java.to_path_buf();
    let work_clone = work.clone();
    tauri::async_runtime::spawn_blocking(move || run_installer(&java, &installer, &work_clone))
        .await
        .map_err(error)??;
    let group = if software == Software::Forge {
        "net/minecraftforge/forge"
    } else {
        "net/neoforged/neoforge"
    };
    let args_file = format!("libraries/{group}/{build}/win_args.txt");
    safety::reject_links(&work.join(&args_file))?;
    if !work.join(&args_file).is_file() {
        return Err("Official installer did not produce Windows launch arguments".into());
    }
    let names = archive::tree(&work)?
        .into_iter()
        .filter(|v| v.starts_with("libraries/"))
        .collect::<Vec<_>>();
    archive::overlay(root, &work, &names)?;
    let loader_version = if software == Software::Forge {
        build
            .strip_prefix(&format!("{minecraft}-"))
            .ok_or("Invalid Forge version")?
            .into()
    } else {
        build
    };
    let spec = Launch {
        software,
        minecraft: minecraft.into(),
        loader_version,
        args_file,
    };
    let private = root.join(".minehost");
    safety::atomic_write(
        &private.join("forge.json"),
        &serde_json::to_vec(&spec).map_err(error)?,
    )?;
    launch_arguments(root, software, minecraft)?;
    Ok(installer_size)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_official_loader_versions() {
        assert_eq!(
            game(Software::Neoforge, "21.1.203").as_deref(),
            Some("1.21.1")
        );
        assert_eq!(
            game(Software::Neoforge, "26.1.0.126").as_deref(),
            Some("26.1")
        );
        assert_eq!(
            game(Software::Forge, "1.20.1-47.4.0").as_deref(),
            Some("1.20.1")
        );
    }
    #[tokio::test]
    #[ignore = "Official installers execute only on GitHub runner"]
    async fn official_installer_contract() {
        for software in [Software::Forge, Software::Neoforge] {
            let game = "1.21.1";
            assert!(versions(software)
                .await
                .unwrap()
                .contains(&game.to_string()));
            let stage = archive::Stage::new(&std::env::temp_dir()).unwrap();
            let java = runtime::install_java(&stage.0, 21).await.unwrap();
            let root = stage.0.join("server");
            std::fs::create_dir(&root).unwrap();
            install(software, game, None, &root, Path::new(&java.path), &|_| {})
                .await
                .unwrap();
            assert!(launch_arguments(&root, software, game).unwrap()[0].starts_with("@libraries/"));
            assert!(!root.join("eula.txt").exists());
        }
    }
}
