use crate::{error, safety, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{io::Write, path::{Path, PathBuf}, process::{Command, Stdio}, time::Duration};

const USER_AGENT: &str = "MineHost/0.1.0 (https://github.com/Frish07/MineHost)";

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder().user_agent(USER_AGENT).https_only(true).connect_timeout(Duration::from_secs(20)).timeout(Duration::from_secs(600)).build().map_err(error)
}

pub async fn paper_versions() -> Result<Vec<String>> {
    let value: Value = client()?.get("https://fill.papermc.io/v3/projects/paper").send().await.map_err(error)?.error_for_status().map_err(error)?.json().await.map_err(error)?;
    let groups = value["versions"].as_object().ok_or("Paper API did not return version groups")?;
    let mut versions: Vec<String> = groups.values().filter_map(Value::as_array).flatten().filter_map(Value::as_str).map(str::to_string).collect();
    versions.sort_by(|a, b| {
        fn numeric(v: &str) -> Vec<u32> { v.split('.').map(|s| s.parse().unwrap_or(0)).collect() }
        numeric(b).cmp(&numeric(a))
    });
    versions.dedup();
    Ok(versions)
}

fn required_text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value.pointer(pointer).and_then(Value::as_str).ok_or_else(|| format!("Missing API field {pointer}"))
}

async fn download(url: &str, checksum: &str, destination: &Path) -> Result<()> {
    if checksum.len() != 64 || !checksum.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("Invalid SHA-256 from provider".into()); }
    let parsed = reqwest::Url::parse(url).map_err(error)?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https" || !(host == "fill-data.papermc.io" || host == "github.com") { return Err(format!("Unexpected download origin: {host}")); }
    safety::reject_links(destination)?;
    let temporary = destination.with_extension(format!("{}.download", uuid::Uuid::new_v4()));
    let result = async {
        let mut response = client()?.get(url).send().await.map_err(error)?.error_for_status().map_err(error)?;
        let mut file = std::fs::OpenOptions::new().create_new(true).write(true).open(&temporary).map_err(error)?;
        let mut digest = Sha256::new();
        let mut size: u64 = 0;
        while let Some(chunk) = response.chunk().await.map_err(error)? {
            size += chunk.len() as u64;
            if size > 512 * 1024 * 1024 { return Err("Download exceeds 512 MiB".into()); }
            digest.update(&chunk);
            file.write_all(&chunk).map_err(error)?;
        }
        if size == 0 || format!("{:x}", digest.finalize()) != checksum.to_ascii_lowercase() { return Err("Download SHA-256 mismatch".into()); }
        file.sync_all().map_err(error)?;
        drop(file);
        std::fs::rename(&temporary, destination).map_err(error)
    }.await;
    if result.is_err() { let _ = std::fs::remove_file(temporary); }
    result
}

pub async fn install_paper(version: &str, directory: &Path) -> Result<u32> {
    safety::version(version)?;
    let builds: Vec<Value> = client()?.get(format!("https://fill.papermc.io/v3/projects/paper/versions/{version}/builds")).send().await.map_err(error)?.error_for_status().map_err(error)?.json().await.map_err(error)?;
    let build = builds.iter().find(|v| v["channel"] == "STABLE").ok_or("This Minecraft version has no stable Paper build")?;
    let artifact = &build["downloads"]["server:default"];
    let url = required_text(artifact, "/url")?;
    let checksum = required_text(artifact, "/checksums/sha256")?;
    let major = build.pointer("/java/version/minimum").and_then(Value::as_u64).ok_or("Paper did not specify its minimum Java version; automatic installation is unsafe")? as u32;
    download(url, checksum, &directory.join("server.jar")).await?;
    Ok(major)
}

#[derive(Serialize)]
pub struct Runtime { pub path: String, pub major: u32 }

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
    command.arg("-version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut command);
    let mut child = command.spawn().map_err(error)?;
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().map_err(error)?.is_some() { break; }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Java version probe timed out".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().map_err(error)?;
    if !output.status.success() { return Err("java -version failed".into()); }
    let text = format!("{}{}", String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout));
    let version = text.split('"').nth(1).ok_or("Cannot read Java version")?;
    let mut parts = version.split(['.', '-', '+']);
    let mut major: u32 = parts.next().unwrap_or_default().parse().map_err(error)?;
    if major == 1 { major = parts.next().unwrap_or_default().parse().map_err(error)?; }
    if major != required { return Err(format!("Java {required} required; found Java {major}")); }
    Ok(Runtime { path: path.to_string_lossy().into_owned(), major })
}

fn find_java(root: &Path, depth: u32) -> Option<PathBuf> {
    if depth > 4 { return None; }
    let executable = root.join("bin/java.exe");
    if executable.is_file() && safety::reject_links(&executable).is_ok() { return Some(executable); }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry.file_type().ok()?.is_dir() && safety::reject_links(&entry.path()).is_ok() {
            if let Some(path) = find_java(&entry.path(), depth + 1) { return Some(path); }
        }
    }
    None
}

pub fn detect_java(root: &Path, required: u32, custom: &str) -> Result<Runtime> {
    if !custom.trim().is_empty() {
        let path = Path::new(custom.trim());
        if !path.is_absolute() || path.file_name().and_then(|s| s.to_str()).map(|s| !s.eq_ignore_ascii_case("java.exe")).unwrap_or(true) {
            return Err("Provide the absolute path to java.exe".into());
        }
        return probe(path, required);
    }
    if let Some(path) = find_java(&root.join(format!("runtimes/java-{required}")), 0) {
        if let Ok(runtime) = probe(&path, required) { return Ok(runtime); }
    }
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME") { candidates.push(PathBuf::from(home).join("bin/java.exe")); }
    if let Some(paths) = std::env::var_os("PATH") { candidates.extend(std::env::split_paths(&paths).map(|p| p.join("java.exe"))); }
    for path in candidates.into_iter().filter(|p| p.is_absolute() && p.is_file()) {
        if let Ok(runtime) = probe(&path, required) { return Ok(runtime); }
    }
    Err(format!("Java {required} not found. Download a private runtime with consent or choose java.exe."))
}

pub async fn install_java(root: &Path, major: u32) -> Result<Runtime> {
    if !(8..=50).contains(&major) { return Err("Unsupported Java major".into()); }
    let values: Vec<Value> = client()?.get(format!("https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jre&os=windows&vendor=eclipse")).send().await.map_err(error)?.error_for_status().map_err(error)?.json().await.map_err(error)?;
    let binary = &values.first().ok_or("No Windows x64 JRE is available")?["binary"]["package"];
    let archive_path = root.join("cache").join(format!("java-{}.zip", uuid::Uuid::new_v4()));
    download(required_text(binary, "/link")?, required_text(binary, "/checksum")?, &archive_path).await?;
    let staging = root.join("runtimes").join(format!("staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging).map_err(error)?;
    let extracted = (|| {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&archive_path).map_err(error)?).map_err(error)?;
        if archive.len() > 30000 { return Err("Too many archive entries".into()); }
        let mut total = 0_u64;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(error)?;
            if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) { return Err("Archive contains symbolic links".into()); }
            total = total.checked_add(entry.size()).ok_or("Archive size overflow")?;
            if total > 1024 * 1024 * 1024 { return Err("Expanded archive exceeds 1 GiB".into()); }
            let path = staging.join(safety::zip_path(entry.name())?);
            if entry.is_dir() { std::fs::create_dir_all(&path).map_err(error)?; }
            else {
                if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(error)?; }
                let mut file = std::fs::OpenOptions::new().create_new(true).write(true).open(&path).map_err(error)?;
                std::io::copy(&mut entry, &mut file).map_err(error)?;
            }
        }
        let java = find_java(&staging, 0).ok_or("Java executable missing from archive")?;
        probe(&java, major)?;
        let destination = root.join(format!("runtimes/java-{major}"));
        safety::reject_links(&destination)?;
        if destination.exists() { return Err("Managed Java directory already exists; select its java.exe or remove it manually".into()); }
        std::fs::rename(&staging, &destination).map_err(error)?;
        detect_java(root, major, "")
    })();
    let _ = std::fs::remove_file(&archive_path);
    // Failed staging directories are retained for inspection; never recursively delete untrusted archives.
    extracted
}
