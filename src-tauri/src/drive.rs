use crate::{
    archive,
    backups::{self, BackupProvider},
    drive_auth as auth, error, lock, safety, win, Operation, Result, Shared,
};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::State;
const FILES: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const PENDING: &str = "MineHost/google-upload-session/v1";
const CHUNK: u64 = 8 * 1024 * 1024;
pub struct GoogleDriveProvider<'a> {
    pub shared: &'a Shared,
    pub cancel: &'a AtomicBool,
    pub profile: &'a str,
    pub kind: &'a str,
    pub resume: bool,
}
impl BackupProvider for GoogleDriveProvider<'_> {
    fn store(&self, source: &Path) -> Result<String> {
        let name = source
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("Invalid backup filename")?;
        upload(
            self.shared,
            self.cancel,
            self.profile,
            name,
            self.kind,
            self.resume,
        )
    }
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: String,
    pub transferred: u64,
    pub total: u64,
    pub error: Option<String>,
}
#[derive(Default)]
pub struct Session {
    pub cancel: Option<Arc<AtomicBool>>,
    pub progress: Progress,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    configured: bool,
    linked: bool,
    busy: bool,
    resumable: bool,
    progress: Progress,
}
#[derive(Serialize, Deserialize)]
struct Pending {
    url: String,
    name: String,
    profile: String,
    size: u64,
    sha256: String,
    kind: String,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CloudFile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub created_time: String,
    #[serde(default)]
    pub md5_checksum: String,
    #[serde(default)]
    pub app_properties: std::collections::HashMap<String, String>,
}
pub fn job(shared: &Shared) -> Result<(Operation, Arc<AtomicBool>)> {
    let mut inner = lock(shared)?;
    if !inner.busy.insert("google-drive".into()) {
        return Err("Another Google Drive operation is active".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    inner.drive.cancel = Some(cancel.clone());
    inner.drive.progress = Progress {
        phase: "preparing".into(),
        ..Default::default()
    };
    Ok((
        Operation {
            shared: shared.clone(),
            id: "google-drive".into(),
        },
        cancel,
    ))
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("Operation paused/cancelled".into())
    } else {
        Ok(())
    }
}
pub fn progress(shared: &Shared, phase: &str, transferred: u64, total: u64) {
    if let Ok(mut inner) = lock(shared) {
        inner.drive.progress = Progress {
            phase: phase.into(),
            transferred,
            total,
            error: None,
        };
    }
}
pub fn finish<T>(shared: &Shared, result: &Result<T>) {
    if let Ok(mut inner) = lock(shared) {
        inner.drive.cancel = None;
        inner.drive.progress.phase = if result.is_ok() { "done" } else { "failed" }.into();
        inner.drive.progress.error = result.as_ref().err().cloned();
    }
}
async fn run<T: Send + 'static>(
    shared: Shared,
    task: impl FnOnce(&Shared, &AtomicBool) -> Result<T> + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_job, cancel) = job(&shared)?;
        let result = task(&shared, &cancel);
        finish(&shared, &result);
        result
    })
    .await
    .map_err(error)?
}
fn root(shared: &Shared) -> Result<PathBuf> {
    Ok(lock(shared)?.root.clone())
}
fn send(request: reqwest::blocking::RequestBuilder) -> Result<Response> {
    request.send().map_err(|_| {
        "Google request failed; check connectivity. An upload session can be resumed.".into()
    })
}
fn identifier(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 200
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Err("Invalid Drive file ID".into())
    } else {
        Ok(())
    }
}
fn size(file: &CloudFile) -> Result<u64> {
    let n = file
        .size
        .parse::<u64>()
        .map_err(|_| "Invalid cloud file size")?;
    if n == 0 || n > archive::LIMIT {
        return Err("Cloud backup exceeds the 32 GiB limit or is empty".into());
    }
    Ok(n)
}
fn owned(file: &CloudFile) -> Result<()> {
    identifier(&file.id)?;
    if file.app_properties.get("minehost").map(String::as_str) != Some("1")
        || !file.name.ends_with(".zip")
        || file
            .app_properties
            .get("profile")
            .is_none_or(|s| uuid::Uuid::parse_str(s).is_err())
    {
        return Err("This is not a MineHost backup created with this OAuth client".into());
    }
    Ok(())
}
fn session_url(value: &str) -> Result<()> {
    let u = reqwest::Url::parse(value).map_err(|_| "Invalid Google upload session")?;
    if u.scheme() != "https"
        || u.host_str() != Some("www.googleapis.com")
        || u.port_or_known_default() != Some(443)
        || u.path() != "/upload/drive/v3/files"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || !u
            .query_pairs()
            .any(|(k, v)| k == "upload_id" && !v.is_empty())
    {
        return Err("Untrusted Google upload session URL".into());
    }
    Ok(())
}
fn digest(path: &Path, cancel: &AtomicBool) -> Result<String> {
    safety::reject_links(path)?;
    let mut file = std::fs::File::open(path).map_err(error)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        check(cancel)?;
        let n = file.read(&mut buffer).map_err(error)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn pending() -> Result<Option<Pending>> {
    win::credential_read(PENDING)?
        .map(|s| serde_json::from_str(&s).map_err(|_| "Invalid saved upload session".into()))
        .transpose()
}
pub fn resume_scheduled(shared: &Shared, cancel: &AtomicBool, id: &str) -> Result<bool> {
    if let Some(p) = pending()? {
        if p.profile != id || p.kind != "scheduled" {
            return Err(
                "Another upload is paused; resume or discard it before scheduled cloud backups"
                    .into(),
            );
        }
        upload(shared, cancel, id, &p.name, "scheduled", true)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
fn get_file(client: &Client, token: &str, id: &str) -> Result<CloudFile> {
    identifier(id)?;
    let v = auth::json(send(
        client
            .get(format!("{FILES}/{id}"))
            .bearer_auth(token)
            .query(&[(
                "fields",
                "id,name,size,createdTime,md5Checksum,appProperties,trashed",
            )]),
    )?)?;
    if v["trashed"].as_bool() == Some(true) {
        return Err("Cloud backup is in Trash".into());
    }
    let file: CloudFile = serde_json::from_value(v).map_err(|_| "Invalid Drive metadata")?;
    owned(&file)?;
    Ok(file)
}
pub fn list(client: &Client, token: &str) -> Result<Vec<CloudFile>> {
    let mut page = String::new();
    let mut files = Vec::new();
    for _ in 0..100 {
        let v = auth::json(send(client.get(FILES).bearer_auth(token).query(&[
            (
                "q",
                "trashed = false and appProperties has { key='minehost' and value='1' }",
            ),
            ("spaces", "drive"),
            (
                "fields",
                "nextPageToken,files(id,name,size,createdTime,md5Checksum,appProperties)",
            ),
            ("pageSize", "100"),
            ("orderBy", "createdTime desc"),
            ("pageToken", &page),
        ]))?)?;
        let rows: Vec<CloudFile> =
            serde_json::from_value(v["files"].clone()).map_err(|_| "Invalid Drive backup list")?;
        for file in rows {
            owned(&file)?;
            files.push(file);
        }
        if let Some(next) = v["nextPageToken"].as_str() {
            page = next.into();
        } else {
            return Ok(files);
        }
    }
    Err("Too many cloud backups; refusing a partial retention list".into())
}
fn acknowledged(response: &Response, total: u64) -> Result<u64> {
    if let Some(range) = response.headers().get("range") {
        let n = range
            .to_str()
            .map_err(|_| "Invalid Google Range")?
            .strip_prefix("bytes=0-")
            .ok_or("Invalid Google Range")?
            .parse::<u64>()
            .map_err(|_| "Invalid Google Range")?
            .checked_add(1)
            .ok_or("Range overflow")?;
        if n > total {
            return Err("Google Range exceeds archive size".into());
        }
        Ok(n)
    } else {
        Ok(0)
    }
}
fn completed(
    shared: &Shared,
    response: Response,
    p: &Pending,
    client: &Client,
    token: &str,
) -> Result<String> {
    let v = auth::json(response)?;
    let id = v["id"]
        .as_str()
        .ok_or("Drive completion lacks file ID; keep session to retry")?;
    let file = get_file(client, token, id)?;
    if size(&file)? != p.size || file.app_properties.get("sha256") != Some(&p.sha256) {
        return Err("Drive completion metadata mismatch".into());
    }
    let local = root(shared)?.join("backups").join(&p.name);
    let mut hash = md5::Md5::new();
    let mut input = std::fs::File::open(local).map_err(error)?;
    let mut buf = vec![0; 1024 * 1024];
    loop {
        let n = input.read(&mut buf).map_err(error)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    if file.md5_checksum != format!("{:x}", hash.finalize()) {
        return Err("Drive archive MD5 checksum mismatch".into());
    }
    win::credential_delete(PENDING)?;
    progress(shared, "uploaded", p.size, p.size);
    Ok(id.into())
}
pub fn upload(
    shared: &Shared,
    cancel: &AtomicBool,
    id: &str,
    name: &str,
    kind: &str,
    resume: bool,
) -> Result<String> {
    let base = root(shared)?;
    let path = backups::source(&base, id, name)?;
    backups::preview(&path)?;
    let total = std::fs::metadata(&path).map_err(error)?.len();
    if total == 0 || total > archive::LIMIT {
        return Err("Archive must be between 1 byte and 32 GiB".into());
    }
    progress(shared, "hashing", 0, total);
    let sha256 = digest(&path, cancel)?;
    let token = auth::access_token()?;
    let client = auth::client()?;
    let p = if resume {
        let p = pending()?.ok_or("No interrupted upload to resume")?;
        if p.profile != id || p.name != name || p.size != total || p.sha256 != sha256 {
            return Err("Interrupted upload belongs to another or modified archive".into());
        }
        p
    } else {
        if pending()?.is_some() {
            return Err(
                "Resume or explicitly discard the interrupted upload before starting another"
                    .into(),
            );
        }
        check(cancel)?;
        let response=send(client.post(UPLOAD).bearer_auth(&token).query(&[("uploadType","resumable"),("fields","id")]).header("X-Upload-Content-Type","application/zip").header("X-Upload-Content-Length",total).json(&json!({"name":name,"mimeType":"application/zip","appProperties":{"minehost":"1","profile":id,"kind":kind,"sha256":sha256}})))?;
        if !response.status().is_success() {
            return Err(format!(
                "Cannot initiate Google upload: HTTP {}",
                response.status().as_u16()
            ));
        }
        let url = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or("Google upload session missing")?
            .to_string();
        session_url(&url)?;
        let p = Pending {
            url,
            name: name.into(),
            profile: id.into(),
            size: total,
            sha256,
            kind: kind.into(),
        };
        win::credential_write(PENDING, &serde_json::to_string(&p).map_err(error)?)?;
        p
    };
    session_url(&p.url)?;
    let mut transfer = Transfer {
        client: &client,
        token,
        url: &p.url,
        total,
        cancel,
        shared: Some(shared),
    };
    let response = transfer.run(&path)?;
    completed(shared, response, &p, &client, &transfer.token)
}
struct Transfer<'a> {
    client: &'a Client,
    token: String,
    url: &'a str,
    total: u64,
    cancel: &'a AtomicBool,
    shared: Option<&'a Shared>,
}
impl Transfer<'_> {
    fn run(&mut self, path: &Path) -> Result<Response> {
        let Self {
            client,
            url,
            total,
            cancel,
            ..
        } = *self;
        let mut offset = 0;
        let mut probe = true;
        let mut failures = 0;
        let mut stalled = 0;
        let mut retries = 0;
        let mut input = std::fs::File::open(path).map_err(error)?;
        while offset < total {
            check(cancel)?;
            let request = if probe {
                client
                    .put(url)
                    .bearer_auth(&self.token)
                    .header("Content-Length", 0)
                    .header("Content-Range", format!("bytes */{total}"))
            } else {
                input.seek(SeekFrom::Start(offset)).map_err(error)?;
                let n = (total - offset).min(CHUNK) as usize;
                let mut bytes = vec![0; n];
                input.read_exact(&mut bytes).map_err(error)?;
                client
                    .put(url)
                    .bearer_auth(&self.token)
                    .header("Content-Type", "application/zip")
                    .header(
                        "Content-Range",
                        format!("bytes {offset}-{}/{total}", offset + n as u64 - 1),
                    )
                    .body(bytes)
            };
            match send(request) {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) if response.status().as_u16()==308 => {
                let next=acknowledged(&response,total)?;
                if next<offset {return Err("Google upload offset moved backwards".into());}
                if !probe && next==offset {stalled+=1;} else {stalled=0;}
                if stalled>3 || next==total {return Err("Google upload stalled; session retained for retry".into());}
                if next>offset {retries=0;}
                offset=next;probe=false;failures=0;if let Some(shared)=self.shared {progress(shared,"uploading",offset,total);}
            }
            Ok(response) if response.status().as_u16()==401 => {self.token=auth::access_token()?;failures+=1;probe=true;}
            Ok(response) if response.status().is_server_error() || response.status().as_u16()==429 => {failures+=1;probe=true;}
            Ok(response) => return Err(format!("Google upload HTTP {}. Session retained; expired sessions must be discarded before retry.",response.status().as_u16())),
            Err(_) => {failures+=1;probe=true;}
        }
            if failures > 0 {
                retries += 1;
            }
            if failures > 4 || retries > 6 {
                return Err(
                    "Upload interrupted; use Resume to continue from Google's confirmed offset"
                        .into(),
                );
            }
            if failures > 0 {
                for _ in 0..(1_u32 << failures.min(4)) * 10 {
                    check(cancel)?;
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        Err("Upload incomplete; session retained".into())
    }
}
pub fn download(shared: &Shared, cancel: &AtomicBool, id: &str, remote: &str) -> Result<String> {
    let token = auth::access_token()?;
    let client = auth::client()?;
    let file = get_file(&client, &token, remote)?;
    let total = size(&file)?;
    let stage = archive::Stage::new(&root(shared)?.join("cache/drive"))?;
    let path = stage.0.join("backup.zip");
    let mut output = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(error)?;
    let mut response = send(
        client
            .get(format!("{FILES}/{remote}"))
            .bearer_auth(&token)
            .query(&[("alt", "media")]),
    )?;
    if !response.status().is_success() {
        return Err(format!(
            "Cloud download HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut bytes = 0;
    let mut sha = Sha256::new();
    let mut md5 = md5::Md5::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        check(cancel)?;
        let n = response
            .read(&mut buffer)
            .map_err(|_| "Cloud download interrupted")?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > total {
            return Err("Cloud archive exceeds declared size".into());
        }
        output.write_all(&buffer[..n]).map_err(error)?;
        sha.update(&buffer[..n]);
        md5.update(&buffer[..n]);
        progress(shared, "downloading", bytes, total);
    }
    output.sync_all().map_err(error)?;
    drop(output);
    if bytes != total
        || file.md5_checksum != format!("{:x}", md5.finalize())
        || file.app_properties.get("sha256") != Some(&format!("{:x}", sha.finalize()))
    {
        return Err("Cloud archive checksum/size mismatch".into());
    }
    let server = {
        let inner = lock(shared)?;
        crate::load(&inner, id)?
    };
    backups::LocalBackupProvider {
        root: &root(shared)?,
        server: &server,
    }
    .store(&path)
}
pub fn retain_cloud(
    client: &Client,
    token: &str,
    id: &str,
    keep: usize,
    cancel: &AtomicBool,
) -> Result<()> {
    let files = list(client, token)?;
    for file in files
        .iter()
        .filter(|f| {
            f.app_properties.get("profile").is_some_and(|v| v == id)
                && f.app_properties
                    .get("kind")
                    .is_some_and(|v| v == "scheduled")
        })
        .skip(keep)
    {
        check(cancel)?;
        trash(client, token, &file.id)?;
    }
    Ok(())
}
fn trash(client: &Client, token: &str, id: &str) -> Result<()> {
    get_file(client, token, id)?;
    auth::json(send(
        client
            .patch(format!("{FILES}/{id}"))
            .bearer_auth(token)
            .json(&json!({"trashed":true})),
    )?)?;
    Ok(())
}
#[tauri::command]
pub fn drive_status(state: State<'_, Shared>) -> Result<Status> {
    let inner = lock(&state)?;
    Ok(Status {
        configured: auth::configuration()?.is_some(),
        linked: auth::linked()?,
        busy: inner.busy.contains("google-drive"),
        resumable: pending()?.is_some(),
        progress: inner.drive.progress.clone(),
    })
}
#[tauri::command]
pub fn drive_cancel(state: State<'_, Shared>) -> Result<()> {
    if let Some(cancel) = &lock(&state)?.drive.cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}
#[tauri::command]
pub fn drive_help() -> Result<()> {
    std::process::Command::new("rundll32.exe")
        .args([
            "url.dll,FileProtocolHandler",
            "https://github.com/Frish07/MineHost/blob/main/docs/GOOGLE_DRIVE.md",
        ])
        .spawn()
        .map_err(|_| "Cannot open setup instructions")?;
    Ok(())
}
#[tauri::command]
pub async fn drive_configure(
    state: State<'_, Shared>,
    source: String,
    consent: bool,
) -> Result<()> {
    if !consent {
        return Err("Confirm importing the Google Desktop client configuration".into());
    }
    run(state.inner().clone(), move |_, _| {
        auth::import(Path::new(&source))
    })
    .await
}
#[tauri::command]
pub async fn drive_connect(state: State<'_, Shared>, consent: bool) -> Result<()> {
    if !consent {
        return Err("Consent to browser authorization is required".into());
    }
    run(state.inner().clone(), |s, c| {
        progress(s, "browser", 0, 0);
        auth::authorize(c)
    })
    .await
}
#[tauri::command]
pub async fn drive_disconnect(state: State<'_, Shared>, revoke: bool, consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm disconnecting Google Drive".into());
    }
    run(state.inner().clone(), move |_, _| {
        auth::disconnect(revoke)?;
        win::credential_delete(PENDING)
    })
    .await
}
#[tauri::command]
pub async fn drive_discard(state: State<'_, Shared>, consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm discarding the interrupted upload session".into());
    }
    run(state.inner().clone(), |_, _| {
        win::credential_delete(PENDING)
    })
    .await
}
#[tauri::command]
pub async fn drive_list(state: State<'_, Shared>) -> Result<Vec<CloudFile>> {
    run(state.inner().clone(), |_, _| {
        list(&auth::client()?, &auth::access_token()?)
    })
    .await
}
#[tauri::command]
pub async fn drive_upload(
    state: State<'_, Shared>,
    id: String,
    name: String,
    consent: bool,
) -> Result<String> {
    if !consent {
        return Err("Confirm sending this backup and its contents to Google Drive".into());
    }
    run(state.inner().clone(), move |s, c| {
        let _profile = crate::begin(s, &id)?;
        GoogleDriveProvider {
            shared: s,
            cancel: c,
            profile: &id,
            kind: "manual",
            resume: false,
        }
        .store(&backups::source(&root(s)?, &id, &name)?)
    })
    .await
}
#[tauri::command]
pub async fn drive_resume(state: State<'_, Shared>, consent: bool) -> Result<String> {
    if !consent {
        return Err("Confirm resuming the cloud upload".into());
    }
    run(state.inner().clone(), |s, c| {
        let p = pending()?.ok_or("No interrupted upload")?;
        let _profile = crate::begin(s, &p.profile)?;
        upload(s, c, &p.profile, &p.name, &p.kind, true)
    })
    .await
}
#[tauri::command]
pub async fn drive_download(
    state: State<'_, Shared>,
    id: String,
    remote: String,
) -> Result<String> {
    run(state.inner().clone(), move |s, c| {
        let _profile = crate::begin(s, &id)?;
        download(s, c, &id, &remote)
    })
    .await
}
#[tauri::command]
pub async fn drive_trash(state: State<'_, Shared>, remote: String, consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm moving the cloud backup to Google Trash".into());
    }
    run(state.inner().clone(), move |_, _| {
        trash(&auth::client()?, &auth::access_token()?, &remote)
    })
    .await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_transfer_uses_confirmed_offset_over_http() {
        let stage = archive::Stage::new(&std::env::temp_dir()).unwrap();
        let path = stage.0.join("source.zip");
        let data: Vec<u8> = (0..300000).map(|n| (n % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/upload", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for step in 0..4 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut header = Vec::new();
                let mut b = [0];
                while !header.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut b).unwrap();
                    header.push(b[0]);
                    assert!(header.len() < 16384);
                }
                let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
                let length = header
                    .lines()
                    .find_map(|s| s.strip_prefix("content-length:"))
                    .unwrap_or("0")
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                let mut body = vec![0; length];
                socket.read_exact(&mut body).unwrap();
                let response = match step {
                    0 => {
                        assert!(header.contains("content-range: bytes */300000"));
                        "HTTP/1.1 308 Resume Incomplete\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    }
                    1 => {
                        assert_eq!(body, data);
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    }
                    2 => {
                        assert_eq!(length, 0);
                        "HTTP/1.1 308 Resume Incomplete\r\nRange: bytes=0-262143\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    }
                    _ => {
                        assert!(header.contains("content-range: bytes 262144-299999/300000"));
                        assert_eq!(body, data[262144..]);
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                    }
                };
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let cancel = AtomicBool::new(false);
        let mut transfer = Transfer {
            client: &client,
            token: "test-only-not-a-credential".into(),
            url: &url,
            total: 300000,
            cancel: &cancel,
            shared: None,
        };
        assert!(transfer.run(&path).unwrap().status().is_success());
        worker.join().unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(transfer.run(&path).is_err());
    }
    #[test]
    fn session_and_metadata_boundaries() {
        assert!(
            session_url("https://www.googleapis.com/upload/drive/v3/files?upload_id=x").is_ok()
        );
        for u in [
            "https://evil.com/upload/drive/v3/files?upload_id=x",
            "http://www.googleapis.com/upload/drive/v3/files?upload_id=x",
            "https://user@www.googleapis.com/upload/drive/v3/files?upload_id=x",
        ] {
            assert!(session_url(u).is_err());
        }
        assert!(identifier("../x").is_err());
        let file:CloudFile=serde_json::from_value(json!({"id":"abc","name":"x.zip","size":"4","appProperties":{"minehost":"1","profile":uuid::Uuid::new_v4().to_string()}})).unwrap();
        assert!(owned(&file).is_ok());
        assert_eq!(size(&file).unwrap(), 4);
    }
}
