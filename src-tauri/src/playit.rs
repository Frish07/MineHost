use crate::{error, lock, runtime, safety, win, Result, Shared};
use serde::Serialize;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::windows::io::OwnedHandle,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tauri::State;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
const VERSION: &str = "2.0.2";
const HASH: &str = "6e62da9f56a3bcf72e0c62c29e94609c847cc9b110c460f68b20035525985238";
const URL:&str="https://github.com/playit-cloud/playit-agent/releases/download/v2.0.2/playitd-windows-x86_64-signed.exe";
const CREDENTIAL: &str = "MineHost/playit-agent/v1";
pub struct ManagedAgent {
    child: Child,
    job: Option<OwnedHandle>,
    pipe: String,
    session: PathBuf,
}
impl ManagedAgent {
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
    fn endpoint(&self) -> (String, u32) {
        (self.pipe.clone(), self.child.id())
    }
}
impl Drop for ManagedAgent {
    fn drop(&mut self) {
        drop(self.job.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.session);
    }
}
pub struct Claim {
    code: String,
    created: Instant,
}
fn binary(root: &Path) -> PathBuf {
    root.join(format!("network/tools/playitd-{VERSION}.exe"))
}
fn verify(path: &Path) -> Result<()> {
    safety::reject_links(path)?;
    let mut file = std::fs::File::open(path).map_err(error)?;
    if file.metadata().map_err(error)?.len() > 32 * 1024 * 1024 {
        return Err("Agent binary exceeds expected size".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(error)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if format!("{:x}", hash.finalize()) != HASH {
        return Err(
            "Agent checksum differs from the pinned official release; install the verified copy"
                .into(),
        );
    }
    Ok(())
}
fn begin(shared: &Shared) -> Result<crate::Operation> {
    let mut inner = lock(shared)?;
    if !inner.busy.insert("playit-operation".into()) {
        return Err("Wait for the current agent operation".into());
    }
    Ok(crate::Operation {
        shared: shared.clone(),
        id: "playit-operation".into(),
    })
}
#[tauri::command]
pub async fn playit_install(
    state: State<'_, Shared>,
    consent: bool,
    on_progress: tauri::ipc::Channel<runtime::DownloadProgress>,
) -> Result<()> {
    if !consent {
        return Err("Confirm downloading the official standalone playit agent".into());
    }
    let _operation = begin(&state)?;
    let root = {
        let mut inner = lock(&state)?;
        if inner.agent.as_mut().is_some_and(ManagedAgent::alive) {
            return Err("Stop the managed agent before replacing its binary".into());
        }
        inner.root.clone()
    };
    let path = binary(&root);
    let folder = path.parent().ok_or("Missing agent directory")?;
    safety::reject_links(folder)?;
    std::fs::create_dir_all(folder).map_err(error)?;
    runtime::download(URL, HASH, &path, &|value| {
        let _ = on_progress.send(value);
    })
    .await?;
    verify(&path)
}
#[tauri::command]
pub async fn playit_import_agent(
    state: State<'_, Shared>,
    source: String,
    consent: bool,
) -> Result<()> {
    if !consent {
        return Err("Confirm using this existing agent binary".into());
    }
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = begin(&shared)?;
        let root = {
            let mut inner = lock(&shared)?;
            if inner.agent.as_mut().is_some_and(ManagedAgent::alive) {
                return Err("Stop the managed agent first".into());
            }
            inner.root.clone()
        };
        let source = PathBuf::from(source);
        verify(&source)?;
        let path = binary(&root);
        safety::reject_links(&path)?;
        std::fs::create_dir_all(path.parent().ok_or("Missing parent")?).map_err(error)?;
        if source != path {
            let bytes = std::fs::read(source).map_err(error)?;
            safety::atomic_write(&path, &bytes)?;
        }
        verify(&path)
    })
    .await
    .map_err(error)?
}
async fn api(path: &str, input: &Value) -> Result<Value> {
    let mut response = runtime::client()?
        .post(format!("https://api.playit.gg{path}"))
        .timeout(Duration::from_secs(15))
        .json(input)
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        if bytes.len() + chunk.len() > 65536 {
            return Err("Playit response exceeds limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(error)?;
    if value["status"] != "success" {
        return Err(
            "Playit declined the claim request; retry the browser flow. No credential was exposed."
                .into(),
        );
    }
    Ok(value["data"].clone())
}
#[tauri::command]
pub async fn playit_claim(state: State<'_, Shared>, consent: bool) -> Result<String> {
    if !consent {
        return Err("Approve connecting MineHost to your playit account".into());
    }
    let _operation = begin(&state)?;
    if win::credential_read(CREDENTIAL)?.is_some() {
        return Err(
            "An agent is already linked; forget it explicitly before claiming another".into(),
        );
    }
    let code = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    api(
        "/claim/setup",
        &json!({"code":code,"agent_type":"self-managed","version":concat!("MineHost ",env!("CARGO_PKG_VERSION"))}),
    )
    .await?;
    lock(&state)?.claim = Some(Claim {
        code: code.clone(),
        created: Instant::now(),
    });
    Ok(format!("https://playit.gg/claim/{code}"))
}
#[tauri::command]
pub async fn playit_finish_claim(state: State<'_, Shared>) -> Result<String> {
    let _operation = begin(&state)?;
    let code = {
        let inner = lock(&state)?;
        let claim = inner.claim.as_ref().ok_or("Start agent linking first")?;
        if claim.created.elapsed() > Duration::from_secs(3600) {
            return Err("Claim expired; start again".into());
        }
        claim.code.clone()
    };
    let setup = api(
        "/claim/setup",
        &json!({"code":code,"agent_type":"self-managed","version":concat!("MineHost ",env!("CARGO_PKG_VERSION"))}),
    )
    .await?;
    let phase = setup.as_str().ok_or("Unknown claim response")?;
    match phase {
        "UserAccepted" => {
            let value = api("/claim/exchange", &json!({"code":code})).await?;
            let secret = value["secret_key"]
                .as_str()
                .filter(|v| {
                    v.len() >= 16 && v.len() <= 256 && v.bytes().all(|b| b.is_ascii_hexdigit())
                })
                .ok_or("Invalid agent credential")?;
            win::credential_write(CREDENTIAL, secret)?;
            lock(&state)?.claim = None;
            Ok("linked".into())
        }
        "UserRejected" => {
            lock(&state)?.claim = None;
            Ok("rejected".into())
        }
        "WaitingForUserVisit" | "WaitingForUser" => Ok("waiting".into()),
        _ => Err("Unsupported claim state".into()),
    }
}
fn spawn(root: &Path) -> Result<ManagedAgent> {
    let path = binary(root);
    verify(&path)?;
    let session = root
        .join("network/private")
        .join(uuid::Uuid::new_v4().to_string());
    win::protect_directory(&session)?;
    let pipe = format!(r"\\.\pipe\minehost-playit-{}", uuid::Uuid::new_v4());
    let mut command = Command::new(path);
    runtime::hide_window(&mut command);
    command
        .arg("--secret-path")
        .arg(session.join("playit.toml"))
        .arg("--socket-path")
        .arg(&pipe)
        .arg("--log-path")
        .arg(session.join("agent.log"))
        .arg("--cert-dir")
        .arg(session.join("certs"))
        .env_remove("SECRET_KEY")
        .env_remove("SECRET_KEY_FILE")
        .env_remove("API_BASE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(error)?;
    let job = match win::kill_on_close_job(&child) {
        Ok(job) => job,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    Ok(ManagedAgent {
        child,
        job: Some(job),
        pipe,
        session,
    })
}
async fn frame(
    reader: &mut BufReader<tokio::net::windows::named_pipe::NamedPipeClient>,
) -> Result<Value> {
    let mut bytes = Vec::new();
    loop {
        let byte = reader.read_u8().await.map_err(error)?;
        if byte == b'\n' {
            break;
        }
        bytes.push(byte);
        if bytes.len() > 512 * 1024 {
            return Err("Agent IPC frame exceeds limit".into());
        }
    }
    serde_json::from_slice(&bytes).map_err(error)
}
async fn ipc(pipe: &str, pid: u32, request: Value) -> Result<Value> {
    use std::os::windows::io::AsRawHandle;
    tokio::time::timeout(Duration::from_secs(5), async {
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(pipe)
            .map_err(error)?;
        win::verify_pipe_owner(client.as_raw_handle(), pid)?;
        let mut reader = BufReader::new(client);
        let hello = frame(&mut reader).await?;
        if hello["message_kind"] != "hello"
            || hello
                .pointer("/data/protocol/ipc_version")
                .and_then(Value::as_u64)
                != Some(3)
        {
            return Err("Unsupported playit IPC protocol".into());
        }
        let message = json!({"ipc_version":3,"request_id":1,"request":request}).to_string() + "\n";
        reader
            .get_mut()
            .write_all(message.as_bytes())
            .await
            .map_err(error)?;
        reader.get_mut().flush().await.map_err(error)?;
        let response = frame(&mut reader).await?;
        if response["message_kind"] != "response"
            || response
                .pointer("/data/ipc_version")
                .and_then(Value::as_u64)
                != Some(3)
            || response.pointer("/data/request_id").and_then(Value::as_u64) != Some(1)
        {
            return Err("Invalid agent response".into());
        }
        let reply = response
            .pointer("/data/response")
            .ok_or("Missing agent response")?;
        if reply["type"] == "error" {
            return Err("Agent rejected the local request; check its status or relink it".into());
        }
        Ok(reply.clone())
    })
    .await
    .map_err(|_| "Agent IPC request timed out".to_string())?
}
#[tauri::command]
pub async fn playit_start(state: State<'_, Shared>, consent: bool) -> Result<()> {
    if !consent {
        return Err(
            "Confirm starting playit and exposing configured tunnels to the Internet".into(),
        );
    }
    let _operation = begin(&state)?;
    let secret = win::credential_read(CREDENTIAL)?.ok_or("Link your playit account first")?;
    let root = {
        let mut inner = lock(&state)?;
        if inner.agent.as_mut().is_some_and(ManagedAgent::alive) {
            return Err("Managed agent is already running".into());
        }
        inner.agent = None;
        inner.root.clone()
    };
    let agent = tauri::async_runtime::spawn_blocking(move || spawn(&root))
        .await
        .map_err(error)??;
    let (pipe, pid) = agent.endpoint();
    lock(&state)?.agent = Some(agent);
    let deadline = Instant::now() + Duration::from_secs(8);
    let result = loop {
        match ipc(&pipe, pid, json!({"type":"set_secret","secret":secret})).await {
            Ok(response) => {
                break if response["type"] == "set_secret"
                    && response.pointer("/data/accepted").and_then(Value::as_bool) == Some(true)
                {
                    Ok(())
                } else {
                    Err("Agent did not accept credential provisioning".into())
                }
            }
            Err(e) => {
                if Instant::now() > deadline {
                    break Err(e);
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
    };
    if result.is_err() {
        let agent = lock(&state)?.agent.take();
        drop(agent);
    }
    result
}
#[tauri::command]
pub async fn playit_stop(state: State<'_, Shared>, consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm stopping the managed tunnels".into());
    }
    let _operation = begin(&state)?;
    let endpoint = { lock(&state)?.agent.as_ref().map(ManagedAgent::endpoint) };
    if let Some((pipe, pid)) = endpoint {
        let _ = ipc(&pipe, pid, json!({"type":"stop"})).await;
        for _ in 0..15 {
            let alive = {
                lock(&state)?
                    .agent
                    .as_mut()
                    .is_some_and(ManagedAgent::alive)
            };
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let agent = lock(&state)?.agent.take();
    tauri::async_runtime::spawn_blocking(move || drop(agent))
        .await
        .map_err(error)?;
    Ok(())
}
#[tauri::command]
pub async fn playit_forget(state: State<'_, Shared>, consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm forgetting the local playit credential".into());
    }
    let _operation = begin(&state)?;
    if lock(&state)?
        .agent
        .as_mut()
        .is_some_and(ManagedAgent::alive)
    {
        return Err("Stop the managed agent before forgetting its credential".into());
    }
    win::credential_delete(CREDENTIAL)?;
    lock(&state)?.claim = None;
    Ok(())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tunnel {
    pub(crate) id: String,
    pub(crate) address: String,
    destination: String,
    pub(crate) disabled: bool,
    pub(crate) matches_profile: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    installed: bool,
    version: &'static str,
    linked: bool,
    running: bool,
    phase: String,
    notice_count: usize,
    pending_tunnels: usize,
    pub(crate) tunnels: Vec<Tunnel>,
}
fn tunnels(value: &Value, port: Option<u16>) -> Vec<Tunnel> {
    value
        .pointer("/data/tunnels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(256)
        .filter_map(|t| {
            let id = t["id"].as_str()?;
            uuid::Uuid::parse_str(id).ok()?;
            let address = t["display_address"].as_str()?.chars().take(512).collect();
            let destination = t["destination"]
                .as_str()?
                .chars()
                .take(512)
                .collect::<String>();
            let matches_profile = port.is_some_and(|p| {
                [
                    format!("127.0.0.1:{p}"),
                    format!("localhost:{p}"),
                    format!("[::1]:{p}"),
                ]
                .contains(&destination)
            });
            Some(Tunnel {
                id: id.into(),
                address,
                destination,
                disabled: t["is_disabled"].as_bool().unwrap_or(true),
                matches_profile,
            })
        })
        .collect()
}
#[tauri::command]
pub async fn playit_status(state: State<'_, Shared>, id: Option<String>) -> Result<Status> {
    let (root, endpoint, port) = {
        let mut inner = lock(&state)?;
        let port = if let Some(id) = &id {
            Some(crate::load(&inner, id)?.port)
        } else {
            None
        };
        if inner.agent.as_mut().is_some_and(|v| !v.alive()) {
            inner.agent = None;
        }
        (
            inner.root.clone(),
            inner.agent.as_ref().map(ManagedAgent::endpoint),
            port,
        )
    };
    let installed = binary(&root).is_file();
    let mut status = Status {
        installed,
        version: VERSION,
        linked: win::credential_read(CREDENTIAL)?.is_some(),
        running: endpoint.is_some(),
        phase: "stopped".into(),
        notice_count: 0,
        pending_tunnels: 0,
        tunnels: vec![],
    };
    if let Some((pipe, pid)) = endpoint {
        match ipc(&pipe, pid, json!({"type":"get_state"})).await {
            Ok(reply) if reply["type"] == "state" => {
                let lifecycle = &reply["data"];
                status.phase = lifecycle["state"].as_str().unwrap_or("unknown").to_string();
                if status.phase == "running" {
                    status.notice_count = lifecycle
                        .pointer("/data/notices")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    status.pending_tunnels = lifecycle
                        .pointer("/data/pending_tunnels")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    status.tunnels = tunnels(lifecycle, port);
                }
            }
            _ => status.phase = "unavailable".into(),
        }
    }
    Ok(status)
}
#[tauri::command]
pub fn network_open_link(kind: String, claim_url: Option<String>) -> Result<()> {
    let url = match kind.as_str() {
        "playit-dashboard" => "https://playit.gg/account/tunnels".into(),
        "playit-domains" => "https://playit.gg/support/add-external-domain/".into(),
        "playit-claim" => {
            let value = claim_url.ok_or("Missing claim URL")?;
            let parsed = reqwest::Url::parse(&value).map_err(error)?;
            if parsed.scheme() != "https"
                || parsed.host_str() != Some("playit.gg")
                || !parsed.path().starts_with("/claim/")
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err("Invalid playit claim URL".into());
            }
            value
        }
        _ => return Err("Unknown network help page".into()),
    };
    Command::new("explorer.exe")
        .arg(url)
        .spawn()
        .map_err(error)?;
    Ok(())
}

// Agent keys never leave Rust. Never follow an authenticated redirect or retry creation.
async fn agent_api(path: &str, input: &Value, secret: &str) -> Result<Value> {
    let client=reqwest::Client::builder().https_only(true).redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(20)).build().map_err(error)?;
    let response=client.post(format!("https://api.playit.gg{path}")).header("Authorization",format!("agent-key {secret}")).header("User-Agent",concat!("MineHost/",env!("CARGO_PKG_VERSION"))).json(input).send().await.map_err(|_|"Playit request failed; check the connection and retry setup. A creation request will not be repeated automatically.".to_string())?;
    let status=response.status();
    if !status.is_success(){return Err(format!("Playit HTTP {}. Check account verification, limits and agent permissions in the dashboard.",status.as_u16()));}
    let mut response=response;let mut bytes=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|_|"Playit response interrupted")?{if bytes.len()+chunk.len()>512*1024{return Err("Playit response exceeds limit".into());}bytes.extend_from_slice(&chunk);}
    let value:Value=serde_json::from_slice(&bytes).map_err(|_|"Invalid playit response")?;
    if value["status"]!="success" {let code=value["data"].as_str().filter(|s|s.len()<=80&&s.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_')).unwrap_or("RequestDeclined");return Err(format!("Playit: {code}. Check account verification, free tunnel limits and agent permissions. No paid option is selected automatically."));}
    Ok(value["data"].clone())
}
fn create_request(agent:&str,profile:&str,port:u16)->Value {
    json!({"name":format!("MineHost-{profile}-{port}"),"tunnel_type":"minecraft-java","tunnel_description":null,"port_type":"tcp","port_count":1,"origin":{"type":"agent","data":{"agent_id":agent,"local_ip":"127.0.0.1","local_port":port}},"enabled":true,"alloc":null,"firewall_id":null,"proxy_protocol":null})
}
fn find_minecraft_tunnel(data:&Value,port:u16,expected:Option<&str>)->Result<Option<(String,String)>> {
    let entries=data["tunnels"].as_array().ok_or("Missing playit tunnels snapshot")?;
    for t in entries.iter().take(256){
        if expected.is_some_and(|id|t["id"].as_str()!=Some(id))||t["local_port"].as_u64()!=Some(u64::from(port))||t["local_ip"]!="127.0.0.1"||t["proto"]!="tcp"||t["tunnel_type"]!="minecraft-java"||!t["disabled"].is_null(){continue;}
        let id=t["id"].as_str().ok_or("Invalid tunnel ID")?;uuid::Uuid::parse_str(id).map_err(|_|"Invalid tunnel ID")?;
        let Some(host)=t["assigned_domain"].as_str().filter(|v|!v.is_empty()) else {continue;};
        let Some(public_port)=t.pointer("/port/from").and_then(Value::as_u64).and_then(|v|u16::try_from(v).ok()).filter(|v|*v>0) else {continue;};
        let address=format!("{}:{public_port}",crate::network::host(host)?);
        return Ok(Some((id.to_string(),address)));
    }Ok(None)
}
#[derive(Serialize)] #[serde(rename_all="camelCase")]
pub struct AutoTunnel {phase:&'static str,tunnel_id:Option<String>,address:Option<String>}
#[tauri::command]
pub async fn playit_auto_tunnel(state:State<'_,Shared>,id:String,consent:bool)->Result<AutoTunnel>{
    if !consent{return Err("Approve creating a free Minecraft Java tunnel to this profile and exposing it to Internet players".into());}
    let _agent=begin(&state)?;let _profile=crate::network::operation(&state,&id)?;
    let port={let inner=lock(&state)?;crate::load(&inner,&id)?.port};
    let secret=win::credential_read(CREDENTIAL)?.ok_or("Link the playit agent first")?;
    let data=agent_api("/agents/rundata",&json!({}),&secret).await?;
    let agent=data["agent_id"].as_str().ok_or("Missing playit agent ID")?;uuid::Uuid::parse_str(agent).map_err(|_|"Invalid playit agent ID")?;
    let previous={let inner=lock(&state)?;inner.db.query_row("SELECT tunnel_id FROM playit_requests WHERE profile_id=?1 AND agent_id=?2 AND port=?3",rusqlite::params![id,agent,port],|r|r.get::<_,Option<String>>(0)).optional().map_err(error)?};
    // A known created ID must match exactly. An uncertain POST can be recovered by its
    // destination; it is never blindly repeated after a timeout or application restart.
    if let Some((tunnel_id,address))=find_minecraft_tunnel(&data,port,previous.as_ref().and_then(|v|v.as_deref()))?{
        let inner=lock(&state)?;inner.db.execute("INSERT INTO playit_requests(profile_id,agent_id,port,tunnel_id) VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id,agent_id,port) DO UPDATE SET tunnel_id=excluded.tunnel_id",rusqlite::params![id,agent,port,tunnel_id]).map_err(error)?;
        return Ok(AutoTunnel{phase:"ready",tunnel_id:Some(tunnel_id),address:Some(address)});
    }
    if previous.is_some(){return Ok(AutoTunnel{phase:"pending",tunnel_id:None,address:None});}
    if data["agent_type"]!="self-managed"{return Err("This older agent was linked for manual configuration. Create its tunnel in the playit dashboard, or stop the agent, forget the local link and link it again for automatic setup. Existing account tunnels are never deleted automatically.".into());}
    {let inner=lock(&state)?;inner.db.execute("INSERT INTO playit_requests(profile_id,agent_id,port,tunnel_id) VALUES(?1,?2,?3,NULL)",rusqlite::params![id,agent,port]).map_err(error)?;}
    let created=match agent_api("/tunnels/create",&create_request(agent,&id,port),&secret).await {
        Ok(value)=>value,
        Err(e)=>{
            // Explicit validation/auth failures did not create an allocation. Uncertain
            // transport/5xx/parse failures retain the marker and cannot duplicate it.
            if e.starts_with("Playit: ")||e.starts_with("Playit HTTP 4") {let inner=lock(&state)?;inner.db.execute("DELETE FROM playit_requests WHERE profile_id=?1 AND agent_id=?2 AND port=?3 AND tunnel_id IS NULL",rusqlite::params![id,agent,port]).map_err(error)?;}
            return Err(e);
        }
    };
    let tunnel=created["id"].as_str().ok_or("Creation response did not contain a tunnel ID; inspect the playit dashboard before retrying")?;uuid::Uuid::parse_str(tunnel).map_err(|_|"Invalid created tunnel ID")?;
    {let inner=lock(&state)?;inner.db.execute("UPDATE playit_requests SET tunnel_id=?4 WHERE profile_id=?1 AND agent_id=?2 AND port=?3",rusqlite::params![id,agent,port,tunnel]).map_err(error)?;}
    Ok(AutoTunnel{phase:"pending",tunnel_id:Some(tunnel.into()),address:None})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_endpoint_rejects_wrong_destination_protocol_disabled_and_unsafe_addresses(){
        let id=uuid::Uuid::new_v4().to_string();let good=json!({"id":id,"local_port":25565,"local_ip":"127.0.0.1","proto":"tcp","tunnel_type":"minecraft-java","disabled":null,"assigned_domain":"test.ply.gg","port":{"from":1234,"to":1235}});
        assert_eq!(find_minecraft_tunnel(&json!({"tunnels":[good.clone()]}),25565,Some(&id)).unwrap().unwrap().1,"test.ply.gg:1234");
        for (field,value) in [("local_port",json!(25566)),("local_ip",json!("192.168.1.2")),("proto",json!("udp")),("tunnel_type",json!("minecraft-bedrock")),("disabled",json!("account-limit"))]{let mut bad=good.clone();bad[field]=value;assert!(find_minecraft_tunnel(&json!({"tunnels":[bad]}),25565,None).unwrap().is_none());}
        assert!(find_minecraft_tunnel(&json!({"tunnels":[good.clone()]}),25565,Some(&uuid::Uuid::new_v4().to_string())).unwrap().is_none());
        let mut bad=good;bad["assigned_domain"]=json!("https://evil.example/path");assert!(find_minecraft_tunnel(&json!({"tunnels":[bad]}),25565,None).is_err());
    }
    #[test]
    fn tunnel_snapshot_excludes_agent_secrets_and_checks_destination() {
        let value = json!({"state":"running","data":{"login_link":"sensitive","tunnels":[{"id":uuid::Uuid::new_v4().to_string(),"display_address":"test.ply.gg:1234","destination":"127.0.0.1:25565","is_disabled":false}]}});
        let result = tunnels(&value, Some(25565));
        assert!(result[0].matches_profile);
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("sensitive"));
    }
    #[tokio::test]
    #[ignore = "Pinned signed agent, GitHub Windows runner only; no account or tunnel created"]
    async fn agent_binary_and_ipc_contract() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let path = binary(&stage.0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        runtime::download(URL, HASH, &path, &|_| {}).await.unwrap();
        verify(&path).unwrap();
        let output = Command::new(&path).arg("--help").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("--socket-path"));
        let mut agent = spawn(&stage.0).unwrap();
        let (pipe, pid) = agent.endpoint();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match ipc(&pipe, pid, json!({"type":"get_state"})).await {
                Ok(reply) => {
                    assert_eq!(reply["type"], "state");
                    assert_eq!(reply["data"]["state"], "waiting_for_secret");
                    break;
                }
                Err(e) => {
                    assert!(Instant::now() < deadline, "{e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let _ = ipc(&pipe, pid, json!({"type":"stop"})).await;
        let _ = agent.alive();
        drop(agent);
    }
}
