use crate::{error, safety, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{digest::DynDigest, Digest, Sha256, Sha512};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const USER_AGENT: &str = "MineHost/0.2.0 (https://github.com/Frish07/MineHost)";

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum DownloadPhase {
    Resolving,
    Downloading,
    Verifying,
    Installing,
    Complete,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub phase: DownloadPhase,
    pub downloaded: u64,
    pub total: Option<u64>,
}

impl DownloadProgress {
    pub fn new(phase: DownloadPhase, downloaded: u64, total: Option<u64>) -> Self {
        Self {
            phase,
            downloaded,
            total,
        }
    }
}

pub(crate) fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .https_only(true)
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(error)
}

pub async fn paper_versions() -> Result<Vec<String>> {
    let value: Value = client()?
        .get("https://fill.papermc.io/v3/projects/paper")
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?
        .json()
        .await
        .map_err(error)?;
    let groups = value["versions"]
        .as_object()
        .ok_or("Paper API did not return version groups")?;
    let mut versions: Vec<String> = groups
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    versions.sort_by(|a, b| {
        fn numeric(v: &str) -> Vec<u32> {
            v.split('.').map(|s| s.parse().unwrap_or(0)).collect()
        }
        numeric(b).cmp(&numeric(a))
    });
    versions.dedup();
    Ok(versions)
}

fn required_text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing API field {pointer}"))
}

fn minimum_java(metadata: &Value) -> Result<u32> {
    let major = metadata
        .pointer("/version/java/version/minimum")
        .and_then(Value::as_u64)
        .ok_or(
            "Paper did not specify its minimum Java version; automatic installation is unsafe",
        )?;
    if !(8..=50).contains(&major) {
        return Err("Unsupported Java major in Paper metadata".into());
    }
    Ok(major as u32)
}

pub(crate) async fn download(
    url: &str,
    checksum: &str,
    destination: &Path,
    progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(error)?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https"
        || !matches!(
            host,
            "fill-data.papermc.io"
                | "github.com"
                | "piston-data.mojang.com"
                | "api.purpurmc.org"
                | "meta.fabricmc.net"
                | "cdn.modrinth.com"
        )
    {
        return Err(format!("Unexpected download origin: {host}"));
    }
    let is_fabric_launcher = host == "meta.fabricmc.net" && parsed.path().ends_with("/server/jar");
    if checksum.is_empty() && !is_fabric_launcher {
        return Err("The download provider must supply a checksum".into());
    }
    let mut digest = if checksum.is_empty() {
        None
    } else {
        Some(hasher(checksum)?)
    };
    safety::reject_links(destination)?;
    let temporary = destination.with_extension(format!("{}.download", uuid::Uuid::new_v4()));
    let result = async {
        let mut response = client()?
            .get(url)
            .send()
            .await
            .map_err(error)?
            .error_for_status()
            .map_err(error)?;
        let total = response.content_length();
        progress(DownloadProgress::new(DownloadPhase::Downloading, 0, total));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(error)?;
        let mut size: u64 = 0;
        let mut last_update = std::time::Instant::now();
        while let Some(chunk) = response.chunk().await.map_err(error)? {
            size += chunk.len() as u64;
            if size > 512 * 1024 * 1024 {
                return Err("Download exceeds 512 MiB".into());
            }
            if let Some(digest) = digest.as_mut() {
                digest.update(&chunk);
            }
            file.write_all(&chunk).map_err(error)?;
            if last_update.elapsed() >= Duration::from_millis(100) {
                progress(DownloadProgress::new(
                    DownloadPhase::Downloading,
                    size,
                    total,
                ));
                last_update = std::time::Instant::now();
            }
        }
        progress(DownloadProgress::new(DownloadPhase::Verifying, size, total));
        if size == 0 {
            return Err("Empty download".into());
        }
        if let Some(digest) = digest.take() {
            if hex(&digest.finalize()) != checksum.to_ascii_lowercase() {
                return Err("Download checksum mismatch".into());
            }
        }
        file.sync_all().map_err(error)?;
        drop(file);
        if checksum.is_empty() {
            let mut jar = zip::ZipArchive::new(std::fs::File::open(&temporary).map_err(error)?)
                .map_err(error)?;
            jar.by_name("META-INF/MANIFEST.MF").map_err(error)?;
            jar.by_name("install.properties").map_err(error)?;
        }
        progress(DownloadProgress::new(
            DownloadPhase::Installing,
            size,
            total,
        ));
        std::fs::rename(&temporary, destination).map_err(error)
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn hasher(checksum: &str) -> Result<Box<dyn DynDigest + Send>> {
    if !checksum.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid checksum".into());
    }
    match checksum.len() {
        32 => Ok(Box::new(md5::Md5::new())),
        40 => Ok(Box::new(sha1::Sha1::new())),
        64 => Ok(Box::new(Sha256::new())),
        128 => Ok(Box::new(Sha512::new())),
        _ => Err("Unsupported checksum algorithm".into()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

type JsonCache = std::collections::HashMap<String, (std::time::Instant, Value, usize)>;
static JSON_CACHE: std::sync::OnceLock<std::sync::Mutex<JsonCache>> = std::sync::OnceLock::new();

pub(crate) async fn get_json(url: &str) -> Result<Value> {
    get_verified_json(url, None).await
}

pub(crate) async fn get_verified_json(url: &str, checksum: Option<&str>) -> Result<Value> {
    let parsed = reqwest::Url::parse(url).map_err(error)?;
    if parsed.scheme() != "https"
        || !matches!(
            parsed.host_str(),
            Some(
                "piston-meta.mojang.com"
                    | "api.purpurmc.org"
                    | "meta.fabricmc.net"
                    | "api.modrinth.com"
            )
        )
    {
        return Err("Unexpected metadata origin".into());
    }
    let key = format!("{url}|{}", checksum.unwrap_or_default());
    let cache = JSON_CACHE.get_or_init(|| std::sync::Mutex::new(JsonCache::new()));
    {
        let guard = cache.lock().map_err(error)?;
        if let Some((at, value, _)) = guard.get(&key) {
            if at.elapsed() < Duration::from_secs(120) {
                return Ok(value.clone());
            }
        }
    }
    let mut response = client()?
        .get(parsed)
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err("API response exceeds 4 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if let Some(checksum) = checksum {
        let mut digest = hasher(checksum)?;
        digest.update(&bytes);
        if hex(&digest.finalize()) != checksum.to_ascii_lowercase() {
            return Err("Metadata checksum mismatch".into());
        }
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(error)?;
    let mut guard = cache.lock().map_err(error)?;
    if guard.len() >= 128
        || guard.values().map(|entry| entry.2).sum::<usize>() + bytes.len() > 16 * 1024 * 1024
    {
        guard.clear();
    }
    guard.insert(key, (std::time::Instant::now(), value.clone(), bytes.len()));
    Ok(value)
}

pub async fn install_paper(
    version: &str,
    directory: &Path,
    progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> Result<u32> {
    progress(DownloadProgress::new(DownloadPhase::Resolving, 0, None));
    safety::version(version)?;
    let metadata: Value = client()?
        .get(format!(
            "https://fill.papermc.io/v3/projects/paper/versions/{version}"
        ))
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?
        .json()
        .await
        .map_err(error)?;
    let builds: Vec<Value> = client()?
        .get(format!(
            "https://fill.papermc.io/v3/projects/paper/versions/{version}/builds"
        ))
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?
        .json()
        .await
        .map_err(error)?;
    let build = builds
        .iter()
        .find(|v| v["channel"] == "STABLE")
        .ok_or_else(|| format!("No stable Paper build for Minecraft {version}. Choose a released Minecraft version; unstable builds are not installed automatically."))?;
    let artifact = &build["downloads"]["server:default"];
    let url = required_text(artifact, "/url")?;
    let checksum = required_text(artifact, "/checksums/sha256")?;
    let major = minimum_java(&metadata)?;
    download(url, checksum, &directory.join("server.jar"), progress).await?;
    Ok(major)
}

#[derive(Serialize)]
pub struct Runtime {
    pub path: String,
    pub major: u32,
}

pub fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn probe(path: &Path, required: u32) -> Result<Runtime> {
    let mut command = Command::new(path);
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut command);
    let mut child = command.spawn().map_err(error)?;
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().map_err(error)?.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Java version probe timed out".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().map_err(error)?;
    if !output.status.success() {
        return Err("java -version failed".into());
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let version = text.split('"').nth(1).ok_or("Cannot read Java version")?;
    let mut parts = version.split(['.', '-', '+']);
    let mut major: u32 = parts.next().unwrap_or_default().parse().map_err(error)?;
    if major == 1 {
        major = parts.next().unwrap_or_default().parse().map_err(error)?;
    }
    if major != required {
        return Err(format!("Java {required} required; found Java {major}"));
    }
    Ok(Runtime {
        path: path.to_string_lossy().into_owned(),
        major,
    })
}

fn find_java(root: &Path, depth: u32) -> Option<PathBuf> {
    if depth > 4 {
        return None;
    }
    let executable = root.join("bin/java.exe");
    if executable.is_file() && safety::reject_links(&executable).is_ok() {
        return Some(executable);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry.file_type().ok()?.is_dir() && safety::reject_links(&entry.path()).is_ok() {
            if let Some(path) = find_java(&entry.path(), depth + 1) {
                return Some(path);
            }
        }
    }
    None
}

pub fn detect_java(root: &Path, required: u32, custom: &str) -> Result<Runtime> {
    if !custom.trim().is_empty() {
        let path = Path::new(custom.trim());
        if !path.is_absolute()
            || path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| !s.eq_ignore_ascii_case("java.exe"))
                .unwrap_or(true)
        {
            return Err("Provide the absolute path to java.exe".into());
        }
        return probe(path, required);
    }
    if let Some(path) = find_java(&root.join(format!("runtimes/java-{required}")), 0) {
        if let Ok(runtime) = probe(&path, required) {
            return Ok(runtime);
        }
    }
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        candidates.push(PathBuf::from(home).join("bin/java.exe"));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|p| p.join("java.exe")));
    }
    for path in candidates
        .into_iter()
        .filter(|p| p.is_absolute() && p.is_file())
    {
        if let Ok(runtime) = probe(&path, required) {
            return Ok(runtime);
        }
    }
    Err(format!(
        "Java {required} not found. Download a private runtime with consent or choose java.exe."
    ))
}

pub async fn install_java(root: &Path, major: u32) -> Result<Runtime> {
    if !(8..=50).contains(&major) {
        return Err("Unsupported Java major".into());
    }
    let values: Vec<Value> = client()?.get(format!("https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jre&os=windows&vendor=eclipse")).send().await.map_err(error)?.error_for_status().map_err(error)?.json().await.map_err(error)?;
    let binary = &values.first().ok_or("No Windows x64 JRE is available")?["binary"]["package"];
    let archive_path = root
        .join("cache")
        .join(format!("java-{}.zip", uuid::Uuid::new_v4()));
    download(
        required_text(binary, "/link")?,
        required_text(binary, "/checksum")?,
        &archive_path,
        &|_| {},
    )
    .await?;
    let staging = root
        .join("runtimes")
        .join(format!("staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging).map_err(error)?;
    let extracted = (|| {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&archive_path).map_err(error)?)
            .map_err(error)?;
        if archive.len() > 30000 {
            return Err("Too many archive entries".into());
        }
        let mut total = 0_u64;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(error)?;
            if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                return Err("Archive contains symbolic links".into());
            }
            total = total
                .checked_add(entry.size())
                .ok_or("Archive size overflow")?;
            if total > 1024 * 1024 * 1024 {
                return Err("Expanded archive exceeds 1 GiB".into());
            }
            let path = staging.join(safety::zip_path(entry.name())?);
            if entry.is_dir() {
                std::fs::create_dir_all(&path).map_err(error)?;
            } else {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(error)?;
                }
                let mut file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)
                    .map_err(error)?;
                std::io::copy(&mut entry, &mut file).map_err(error)?;
            }
        }
        let java = find_java(&staging, 0).ok_or("Java executable missing from archive")?;
        probe(&java, major)?;
        let destination = root.join(format!("runtimes/java-{major}"));
        safety::reject_links(&destination)?;
        if destination.exists() {
            return Err(
                "Managed Java directory already exists; select its java.exe or remove it manually"
                    .into(),
            );
        }
        std::fs::rename(&staging, &destination).map_err(error)?;
        detect_java(root, major, "")
    })();
    let _ = std::fs::remove_file(&archive_path);
    // Failed staging directories are retained for inspection; never recursively delete untrusted archives.
    extracted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_requirement_comes_from_version_metadata() {
        let metadata =
            serde_json::json!({"version":{"id":"1.21.11","java":{"version":{"minimum":21}}}});
        assert_eq!(minimum_java(&metadata).unwrap(), 21);
        assert!(minimum_java(&serde_json::json!({"downloads":{}})).is_err());
        assert!(
            minimum_java(&serde_json::json!({"version":{"java":{"version":{"minimum":999}}}}))
                .is_err()
        );
    }

    #[tokio::test]
    #[ignore = "Real network and runtime download; run explicitly on GitHub Actions only"]
    async fn managed_java_download_and_paper_contract() {
        let root = std::env::temp_dir().join(format!("minehost-ci-{}", uuid::Uuid::new_v4()));
        for dir in ["cache", "runtimes", "server"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let versions = paper_versions().await.unwrap();
        assert!(versions.iter().any(|v| v == "1.21.11"));
        let progress_events = std::sync::Mutex::new(Vec::new());
        let report = |event| {
            progress_events.lock().unwrap().push(event);
        };
        let major = install_paper("1.21.11", &root.join("server"), &report)
            .await
            .unwrap();
        assert_eq!(major, 21);
        let java = install_java(&root, major).await.unwrap();
        assert_eq!(java.major, major);
        assert!(Path::new(&java.path).is_file());
        assert!(detect_java(&root, major, "").is_ok());
        assert!(root.join("server/server.jar").metadata().unwrap().len() > 1_000_000);
        {
            let events = progress_events.lock().unwrap();
            assert_eq!(events.first().unwrap().phase, DownloadPhase::Resolving);
            assert!(events
                .iter()
                .any(|event| event.phase == DownloadPhase::Downloading));
            assert!(events
                .iter()
                .any(|event| event.phase == DownloadPhase::Verifying));
            assert_eq!(events.last().unwrap().phase, DownloadPhase::Installing);
            assert!(events
                .windows(2)
                .all(|pair| pair[0].downloaded <= pair[1].downloaded));
            let final_event = events.last().unwrap();
            assert_eq!(
                final_event.downloaded,
                root.join("server/server.jar").metadata().unwrap().len()
            );
            if let Some(total) = final_event.total {
                assert_eq!(final_event.downloaded, total);
            }
        }
        // Reinstall over the existing JAR too: retrying must remain safe.
        install_paper("1.21.11", &root.join("server"), &|_| {})
            .await
            .unwrap();
        // Never accept the Minecraft EULA or start a Minecraft server in an automated test.
        assert!(!root.join("server/eula.txt").exists());
    }
}
