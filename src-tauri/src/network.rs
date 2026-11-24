use crate::{error, lock, runtime, win, Result, Shared};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};
use tauri::State;
pub const SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS network_settings(server_id TEXT PRIMARY KEY,data TEXT NOT NULL);";
const SCRIPT: &str = include_str!("network.ps1");
pub trait TunnelProvider {
    fn endpoint(&self) -> Result<ConnectionEndpoint>;
}
#[derive(Serialize)]
pub struct ConnectionEndpoint {
    host: String,
    port: u16,
}
struct ManualTunnel<'a> {
    host: &'a str,
    port: u16,
}
impl TunnelProvider for ManualTunnel<'_> {
    fn endpoint(&self) -> Result<ConnectionEndpoint> {
        if self.port == 0 {
            return Err("Invalid public port".into());
        }
        Ok(ConnectionEndpoint {
            host: host(self.host)?,
            port: self.port,
        })
    }
}
struct PlayitTunnel<'a> {
    tunnel: &'a crate::playit::Tunnel,
}
impl TunnelProvider for PlayitTunnel<'_> {
    fn endpoint(&self) -> Result<ConnectionEndpoint> {
        if self.tunnel.disabled || !self.tunnel.matches_profile {
            return Err(
                "Select an enabled playit tunnel targeting this profile's loopback port".into(),
            );
        }
        let (host, port) = endpoint(&self.tunnel.address)?;
        Ok(ConnectionEndpoint { host, port })
    }
}
#[tauri::command]
pub async fn connection_endpoint(
    state: State<'_, Shared>,
    id: String,
    settings: Settings,
) -> Result<ConnectionEndpoint> {
    validate(&settings)?;
    {
        let inner = lock(&state)?;
        crate::load(&inner, &id)?;
    }
    match settings.mode.as_str() {
        "direct" | "manual" => ManualTunnel {
            host: &settings.host,
            port: settings.public_port,
        }
        .endpoint(),
        "playit" => {
            let status = crate::playit::playit_status(state, Some(id)).await?;
            let tunnel = status
                .tunnels
                .iter()
                .find(|v| v.id == settings.tunnel_id)
                .ok_or("The selected playit tunnel is not currently reported by the agent")?;
            PlayitTunnel { tunnel }.endpoint()
        }
        _ => Err("LAN endpoints are not sent to the external service".into()),
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub mode: String,
    pub host: String,
    pub public_port: u16,
    pub domain: String,
    pub srv_target: String,
    pub wan_ipv4: String,
    pub lan_ipv4: String,
    pub tunnel_id: String,
}
impl Settings {
    fn defaults(port: u16) -> Self {
        Self {
            mode: "lan".into(),
            host: String::new(),
            public_port: port,
            domain: String::new(),
            srv_target: String::new(),
            wan_ipv4: String::new(),
            lan_ipv4: String::new(),
            tunnel_id: String::new(),
        }
    }
}
pub fn domain(value: &str) -> Result<String> {
    let value = value.trim_end_matches('.').to_ascii_lowercase();
    if value.len() > 253
        || !value.contains('.')
        || value.parse::<IpAddr>().is_ok()
        || !value.rsplit('.').next().is_some_and(|v|v.bytes().any(|b|b.is_ascii_alphabetic()))
        || !value.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err("Use a valid DNS hostname, without URL, path or port; use punycode for international names".into());
    }
    Ok(value)
}
pub fn global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !v.is_private()
                && !v.is_loopback()
                && !v.is_link_local()
                && !v.is_broadcast()
                && !v.is_multicast()
                && !v.is_unspecified()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !v.is_documentation()
                && !(a == 198 && (b == 18 || b == 19))
                && !(a==192 && b==0 && c==0)
        }
        IpAddr::V6(v) => {
            if let Some(v4)=v.to_ipv4_mapped(){return global(IpAddr::V4(v4));}
            let o = v.octets();
            !v.is_loopback()
                && !v.is_unspecified()
                && o[0] != 0xff
                && (o[0] & 0xfe) != 0xfc
                && !(o[0] == 0xfe && (o[1] & 0xc0) == 0x80)
                && v.segments()[..2] != [0x2001, 0x0db8]
                && (o[0]&0xe0)==0x20
                && !(v.segments()[0]==0x3fff && v.segments()[1]&0xf000==0)
        }
    }
}
pub fn host(value: &str) -> Result<String> {
    let value = value.trim().trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = value.parse::<IpAddr>() {
        if !global(ip) {
            return Err("Use a publicly routable address; private and documentation addresses cannot be checked externally".into());
        }
        Ok(ip.to_string())
    } else {
        domain(value)
    }
}
pub fn endpoint(address: &str) -> Result<(String, u16)> {
    let text = address.trim();
    if let Ok(socket) = text.parse::<SocketAddr>() {
        if socket.port()==0{return Err("Port must be 1–65535".into());}
        return Ok((host(&socket.ip().to_string())?, socket.port()));
    }
    let (name, port) = if let Some((name, port)) = text.rsplit_once(':') {
        if name.contains(':') {
            return Err("Use [IPv6]:port notation".into());
        }
        (name, port.parse::<u16>().map_err(|_| "Invalid port")?)
    } else {
        (text, 25565)
    };
    if port == 0 {
        return Err("Port must be 1–65535".into());
    }
    Ok((host(name)?, port))
}
fn validate(settings: &Settings) -> Result<()> {
    if !["lan", "direct", "playit", "manual"].contains(&settings.mode.as_str())
        || settings.public_port == 0
    {
        return Err("Invalid connection mode or port".into());
    }
    if !settings.host.is_empty() {
        host(&settings.host)?;
    }
    for value in [&settings.domain, &settings.srv_target] {
        if !value.is_empty() {
            domain(value)?;
        }
    }
    for value in [&settings.wan_ipv4, &settings.lan_ipv4] {
        if !value.is_empty() {
            value
                .parse::<Ipv4Addr>()
                .map_err(|_| "Use a valid IPv4 address")?;
        }
    }
    if !settings.tunnel_id.is_empty() {
        uuid::Uuid::parse_str(&settings.tunnel_id).map_err(error)?;
    }
    Ok(())
}
fn settings(shared: &Shared, id: &str) -> Result<Settings> {
    let inner = lock(shared)?;
    let server = crate::load(&inner, id)?;
    let mut query = inner
        .db
        .prepare("SELECT data FROM network_settings WHERE server_id=?1")
        .map_err(error)?;
    let mut rows = query.query([id]).map_err(error)?;
    if let Some(row) = rows.next().map_err(error)? {
        let text: String = row.get(0).map_err(error)?;
        serde_json::from_str(&text).map_err(error)
    } else {
        Ok(Settings::defaults(server.port))
    }
}
fn operation(shared: &Shared, id: &str) -> Result<crate::Operation> {
    let mut inner = lock(shared)?;
    crate::load(&inner, id)?;
    if !inner.busy.insert(id.into()) {
        return Err("Wait for current profile operations".into());
    }
    Ok(crate::Operation {
        shared: shared.clone(),
        id: id.into(),
    })
}
#[tauri::command]
pub fn network_settings(state: State<'_, Shared>, id: String) -> Result<Settings> {
    settings(&state, &id)
}
#[tauri::command]
pub fn save_network_settings(
    state: State<'_, Shared>,
    id: String,
    settings: Settings,
) -> Result<()> {
    validate(&settings)?;
    let _operation = operation(&state, &id)?;
    let inner = lock(&state)?;
    inner.db.execute("INSERT INTO network_settings(server_id,data) VALUES(?1,?2) ON CONFLICT(server_id) DO UPDATE SET data=excluded.data",rusqlite::params![id,serde_json::to_string(&settings).map_err(error)?]).map_err(error)?;
    Ok(())
}
#[tauri::command]
pub async fn network_adapters(state: State<'_, Shared>) -> Result<Value> {
    let root = { lock(&state)?.root.clone() };
    tauri::async_runtime::spawn_blocking(move || {
        win::powershell(&root, SCRIPT, &json!({"action":"adapters"}), false)
    })
    .await
    .map_err(error)?
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicIp {
    ipv4: String,
    cgnat: String,
}
fn classify_wan(wan: &str, external: &str) -> String {
    if wan.is_empty() {
        return "unknown_without_router_wan".into();
    }
    let Ok(wan) = wan.parse::<Ipv4Addr>() else {
        return "invalid_wan".into();
    };
    let [a, b, _, _] = wan.octets();
    if a == 100 && (64..=127).contains(&b) {
        "shared_address_likely_cgnat".into()
    } else if !global(IpAddr::V4(wan)) {
        "private_wan_double_nat_or_cgnat".into()
    } else if wan.to_string() != external {
        "wan_differs_upstream_nat_or_vpn".into()
    } else {
        "public_wan_matches_external".into()
    }
}
async fn json_get(url: &str, limit: usize) -> Result<Value> {
    let mut response = runtime::client()?
        .get(url)
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        if bytes.len() + chunk.len() > limit {
            return Err("Network response exceeds limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(error)
}
#[tauri::command]
pub async fn public_ip(wan_ipv4: String, consent: bool) -> Result<PublicIp> {
    if !consent {
        return Err("Approve contacting api.ipify.org to discover your public IPv4".into());
    }
    if !wan_ipv4.is_empty() {
        wan_ipv4.parse::<Ipv4Addr>().map_err(error)?;
    }
    let value = json_get("https://api.ipify.org?format=json", 4096).await?;
    let ipv4 = value["ip"]
        .as_str()
        .ok_or("Missing external IP")?
        .parse::<Ipv4Addr>()
        .map_err(error)?
        .to_string();
    Ok(PublicIp {
        cgnat: classify_wan(&wan_ipv4, &ipv4),
        ipv4,
    })
}
fn varint(mut n: u32) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        result.push(b);
        if n == 0 {
            return result;
        }
    }
}
fn read_varint(stream: &mut impl Read) -> Result<u32> {
    let mut value = 0;
    for shift in [0, 7, 14, 21, 28] {
        let mut b = [0];
        stream.read_exact(&mut b).map_err(error)?;
        if shift == 28 && (b[0] & 0xf0) != 0 {
            return Err("Invalid Minecraft VarInt".into());
        }
        value |= ((b[0] & 0x7f) as u32) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("Minecraft VarInt exceeds five bytes".into())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalStatus {
    tcp_open: bool,
    minecraft: bool,
    latency_ms: u128,
    version: Option<String>,
    online: Option<u64>,
    max: Option<u64>,
    detail: String,
}
fn probe(port: u16) -> LocalStatus {
    let started = Instant::now();
    let mut result = LocalStatus {
        tcp_open: false,
        minecraft: false,
        latency_ms: 0,
        version: None,
        online: None,
        max: None,
        detail: String::new(),
    };
    let work = (|| -> Result<()> {
        let mut stream = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(2),
        )
        .map_err(error)?;
        result.tcp_open = true;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(error)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(error)?;
        let mut data = vec![0];
        data.extend([0xff, 0xff, 0xff, 0xff, 0x0f]);
        data.push(9);
        data.extend(b"127.0.0.1");
        data.extend(port.to_be_bytes());
        data.push(1);
        let mut packet = varint(data.len() as u32);
        packet.extend(data);
        packet.extend([1, 0]);
        stream.write_all(&packet).map_err(error)?;
        let size = read_varint(&mut stream)?;
        if !(2..=65536).contains(&size) {
            return Err("Minecraft status packet exceeds limit".into());
        }
        let mut bytes = vec![0; size as usize];
        stream.read_exact(&mut bytes).map_err(error)?;
        let mut cursor = std::io::Cursor::new(bytes);
        if read_varint(&mut cursor)? != 0 {
            return Err("Unexpected Minecraft status packet".into());
        }
        let size = read_varint(&mut cursor)?;
        if size > 65536 || cursor.position() + size as u64 != cursor.get_ref().len() as u64 {
            return Err("Invalid Minecraft JSON length".into());
        }
        let value: Value = serde_json::from_slice(&cursor.get_ref()[cursor.position() as usize..])
            .map_err(error)?;
        if !value["version"].is_object() || !value["players"].is_object() {
            return Err("TCP responds but did not return a standard Minecraft status".into());
        }
        result.minecraft = true;
        result.version = value
            .pointer("/version/name")
            .and_then(Value::as_str)
            .map(|v| v.chars().take(200).collect());
        result.online = value.pointer("/players/online").and_then(Value::as_u64);
        result.max = value.pointer("/players/max").and_then(Value::as_u64);
        Ok(())
    })();
    if let Err(e) = work {
        result.detail = e;
    }
    result.latency_ms = started.elapsed().as_millis();
    result
}
#[tauri::command]
pub async fn local_network_status(state: State<'_, Shared>, id: String) -> Result<LocalStatus> {
    let port = {
        let inner = lock(&state)?;
        crate::load(&inner, &id)?.port
    };
    tauri::async_runtime::spawn_blocking(move || probe(port))
        .await
        .map_err(error)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalStatus {
    online: bool,
    version: Option<String>,
    ip: Option<String>,
    port: Option<u16>,
    cache_time: Option<u64>,
    cache_expire: Option<u64>,
}
#[tauri::command]
pub async fn external_network_status(
    hostname: String,
    port: u16,
    consent: bool,
) -> Result<ExternalStatus> {
    if !consent {
        return Err("Approve sending the public server address to api.mcsrvstat.us".into());
    }
    let hostname = host(&hostname)?;
    if port == 0 {
        return Err("Invalid public port".into());
    }
    let address = if hostname.contains(':') {
        format!("[{hostname}]:{port}")
    } else {
        format!("{hostname}:{port}")
    };
    let mut url = reqwest::Url::parse("https://api.mcsrvstat.us/3/").map_err(error)?;
    url.path_segments_mut()
        .map_err(|_| "Invalid status URL")?
        .pop_if_empty()
        .push(&address);
    let value = json_get(url.as_str(), 256 * 1024).await?;
    Ok(ExternalStatus {
        online: value["online"]
            .as_bool()
            .ok_or("The external service did not return a valid Minecraft status")?,
        version: value["version"]
            .as_str()
            .map(|v| v.chars().take(200).collect()),
        ip: value["ip"]
            .as_str()
            .filter(|v| v.parse::<IpAddr>().is_ok())
            .map(str::to_string),
        port: value["port"].as_u64().and_then(|v| u16::try_from(v).ok()),
        cache_time: value.pointer("/debug/cachetime").and_then(Value::as_u64),
        cache_expire: value.pointer("/debug/cacheexpire").and_then(Value::as_u64),
    })
}
#[tauri::command]
pub async fn dns_check(
    state: State<'_, Shared>,
    domain_name: String,
    target: String,
) -> Result<Value> {
    let name = domain(&domain_name)?;
    let target = if target.is_empty() {
        name.clone()
    } else {
        domain(&target)?
    };
    let root = { lock(&state)?.root.clone() };
    tauri::async_runtime::spawn_blocking(move || {
        win::powershell(
            &root,
            SCRIPT,
            &json!({"action":"dns","domain":name,"target":target}),
            false,
        )
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn firewall_status(state: State<'_, Shared>, id: String) -> Result<Value> {
    let (root, port) = {
        let inner = lock(&state)?;
        (inner.root.clone(), crate::load(&inner, &id)?.port)
    };
    tauri::async_runtime::spawn_blocking(move || {
        win::powershell(
            &root,
            SCRIPT,
            &json!({"action":"firewall_get","id":id,"port":port}),
            false,
        )
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn firewall_change(
    state: State<'_, Shared>,
    id: String,
    remove: bool,
    profile: String,
    remote: String,
    consent: bool,
) -> Result<()> {
    if !consent {
        return Err(
            "Approve the exact Windows firewall rule before requesting administrator permission"
                .into(),
        );
    }
    if !["Private", "Public"].contains(&profile.as_str())
        || !["LocalSubnet", "Any"].contains(&remote.as_str())
    {
        return Err("Invalid firewall scope".into());
    }
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move||{let _operation=operation(&shared,&id)?;let(root,port)={let inner=lock(&shared)?;(inner.root.clone(),crate::load(&inner,&id)?.port)};win::powershell(&root,SCRIPT,&json!({"action":if remove{"firewall_remove"}else{"firewall_add"},"id":id,"port":port,"profile":profile,"remote":remote}),true)?;Ok(())}).await.map_err(error)?
}
#[tauri::command]
pub async fn upnp_change(
    state: State<'_, Shared>,
    id: String,
    action: String,
    external_port: u16,
    internal_ip: String,
    consent: bool,
) -> Result<Value> {
    if !["upnp_get", "upnp_add", "upnp_remove"].contains(&action.as_str()) || external_port < 1024 {
        return Err("Invalid UPnP operation or external port".into());
    }
    if action != "upnp_get" && !consent {
        return Err("Approve changing the router's port mapping".into());
    }
    let internal_ip=if action=="upnp_get"&&internal_ip.is_empty(){"0.0.0.0".into()}else{internal_ip};
    let ip = internal_ip
        .parse::<Ipv4Addr>()
        .map_err(|_| "Select a local IPv4 address")?;
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move||{let _operation=operation(&shared,&id)?;let(root,port)={let inner=lock(&shared)?;(inner.root.clone(),crate::load(&inner,&id)?.port)};let adapters=if action=="upnp_add"{win::powershell(&root,SCRIPT,&json!({"action":"adapters"}),false)?}else{Value::Null};if action=="upnp_add" && !adapters.as_array().into_iter().flatten().any(|v|v["ipv4"].as_array().is_some_and(|values|values.iter().any(|v|v.as_str()==Some(&ip.to_string())))){return Err("Internal address does not belong to an active local adapter".into());}win::powershell(&root,SCRIPT,&json!({"action":action,"id":id,"port":port,"externalPort":external_port,"internalIp":ip.to_string()}),false)}).await.map_err(error)?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Temporary owned firewall rule on disposable Windows GitHub runner only"]
    fn firewall_rule_contract() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let port = 49173;
        struct Cleanup {
            root: std::path::PathBuf,
            id: String,
            port: u16,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = win::powershell(
                    &self.root,
                    SCRIPT,
                    &json!({"action":"firewall_remove","id":self.id,"port":self.port}),
                    false,
                );
            }
        }
        let _cleanup = Cleanup {
            root: stage.0.clone(),
            id: id.clone(),
            port,
        };
        win::powershell(&stage.0,SCRIPT,&json!({"action":"firewall_add","id":id,"port":port,"profile":"Private","remote":"LocalSubnet"}),false).unwrap();
        let value = win::powershell(
            &stage.0,
            SCRIPT,
            &json!({"action":"firewall_get","id":id,"port":port}),
            false,
        )
        .unwrap();
        assert_eq!(value["present"], true);
        assert_eq!(value["port"], port.to_string());
        assert_eq!(value["enabled"], true);
        win::powershell(
            &stage.0,
            SCRIPT,
            &json!({"action":"firewall_remove","id":id,"port":port}),
            false,
        )
        .unwrap();
        let value = win::powershell(
            &stage.0,
            SCRIPT,
            &json!({"action":"firewall_get","id":id,"port":port}),
            false,
        )
        .unwrap();
        assert_eq!(value["present"], false);
    }
    #[test]
    fn distinguishes_nat_without_false_cgnat_claim() {
        assert_eq!(classify_wan("", "1.1.1.1"), "unknown_without_router_wan");
        assert_eq!(
            classify_wan("100.64.0.1", "1.1.1.1"),
            "shared_address_likely_cgnat"
        );
        assert_eq!(
            classify_wan("192.168.1.1", "1.1.1.1"),
            "private_wan_double_nat_or_cgnat"
        );
        assert_eq!(
            classify_wan("1.1.1.1", "1.1.1.1"),
            "public_wan_matches_external"
        );
    }
    #[test]
    fn rejects_urls_private_hosts_and_dns_injection() {
        for value in [
            "https://example.com",
            "example.com/path",
            "x; stop",
            "localhost",
            "127.0.0.1",
            "127.1",
            "192.0.0.8",
            "100::1",
            "[::ffff:127.0.0.1]",
            "10.0.0.1",
            "100.64.1.1",
            "[::1]",
        ] {
            assert!(host(value).is_err(), "{value}");
        }
        assert!(domain("mc.example.com").is_ok());
        assert!(endpoint("mc.example.com:25565").is_ok());
        assert!(endpoint("[2606:4700:4700::1111]:25565").is_ok());
    }
    #[test]
    fn reads_real_minecraft_protocol_on_loopback() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let size = read_varint(&mut stream).unwrap();
            let mut handshake = vec![0; size as usize];
            stream.read_exact(&mut handshake).unwrap();
            let mut request = [0; 2];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(request, [1, 0]);
            let text = json!({"version":{"name":"protocol-test"},"players":{"online":2,"max":20}})
                .to_string();
            let mut data = vec![0];
            data.extend(varint(text.len() as u32));
            data.extend(text.as_bytes());
            let mut packet = varint(data.len() as u32);
            packet.extend(data);
            stream.write_all(&packet).unwrap();
        });
        let status = probe(port);
        thread.join().unwrap();
        assert!(status.tcp_open && status.minecraft);
        assert_eq!(status.online, Some(2));
    }
    #[tokio::test]
    #[ignore = "Windows native networking and public IPv4; GitHub runner only"]
    async fn network_contract() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let adapters =
            win::powershell(&stage.0, SCRIPT, &json!({"action":"adapters"}), false).unwrap();
        assert!(adapters.is_array());
        let dns = win::powershell(
            &stage.0,
            SCRIPT,
            &json!({"action":"dns","domain":"example.com","target":"example.com"}),
            false,
        )
        .unwrap();
        assert!(!dns["records"].as_array().unwrap().is_empty());
        let value = public_ip(String::new(), true).await.unwrap();
        assert!(value.ipv4.parse::<Ipv4Addr>().is_ok());
        assert_eq!(value.cgnat, "unknown_without_router_wan");
    }
}
